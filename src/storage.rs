use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

use crate::events::{EventType, FlowEvent};
use crate::state::{RunState, StateChangedPayload};

pub struct SqliteProjectionStore {
    connection: Connection,
}

impl SqliteProjectionStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let connection = Connection::open(path.as_ref()).with_context(|| {
            format!(
                "failed to open SQLite projection {}",
                path.as_ref().display()
            )
        })?;
        let store = Self { connection };
        store.init_schema()?;
        Ok(store)
    }

    pub fn in_memory() -> Result<Self> {
        let store = Self {
            connection: Connection::open_in_memory().context("failed to open in-memory SQLite")?,
        };
        store.init_schema()?;
        Ok(store)
    }

    pub fn init_schema(&self) -> Result<()> {
        self.connection
            .execute_batch(
                r#"
CREATE TABLE IF NOT EXISTS jobs (
    job_name TEXT PRIMARY KEY,
    workflow_version TEXT,
    schema_version TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS runs (
    run_id TEXT PRIMARY KEY,
    job_name TEXT,
    state TEXT NOT NULL,
    started_at TEXT,
    ended_at TEXT,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS steps (
    run_id TEXT NOT NULL,
    step_name TEXT NOT NULL,
    state TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (run_id, step_name)
);

CREATE TABLE IF NOT EXISTS attempts (
    run_id TEXT NOT NULL,
    step_name TEXT NOT NULL,
    attempt INTEGER NOT NULL,
    state TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (run_id, step_name, attempt)
);

CREATE TABLE IF NOT EXISTS events (
    event_id TEXT PRIMARY KEY,
    event_type TEXT NOT NULL,
    event_version INTEGER NOT NULL,
    timestamp TEXT NOT NULL,
    run_id TEXT NOT NULL,
    step_name TEXT,
    attempt INTEGER,
    payload TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS metrics (
    run_id TEXT NOT NULL,
    metric_name TEXT NOT NULL,
    metric_value REAL NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (run_id, metric_name)
);

CREATE TABLE IF NOT EXISTS artifacts (
    run_id TEXT NOT NULL,
    step_name TEXT NOT NULL,
    path TEXT NOT NULL,
    size_bytes INTEGER,
    created_at TEXT NOT NULL,
    PRIMARY KEY (run_id, step_name, path)
);

CREATE TABLE IF NOT EXISTS locks (
    lock_name TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    acquired_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS state_events (
    event_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    from_state TEXT NOT NULL,
    to_state TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    FOREIGN KEY (event_id) REFERENCES events(event_id)
);

CREATE TABLE IF NOT EXISTS audit_events (
    event_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    action TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    payload TEXT NOT NULL,
    FOREIGN KEY (event_id) REFERENCES events(event_id)
);

CREATE TABLE IF NOT EXISTS plugins (
    plugin_id TEXT PRIMARY KEY,
    version TEXT NOT NULL,
    contract_version TEXT NOT NULL,
    registered_at TEXT NOT NULL
);
"#,
            )
            .context("failed to initialize SQLite projection schema")
    }

    pub fn project_event(&mut self, event: &FlowEvent) -> Result<()> {
        let tx = self
            .connection
            .transaction()
            .context("failed to start projection transaction")?;

        tx.execute(
            r#"
INSERT OR IGNORE INTO events (
    event_id, event_type, event_version, timestamp, run_id, step_name, attempt, payload
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
"#,
            params![
                event.event_id.to_string(),
                event_type_name(event.event_type)?,
                event.event_version,
                event.timestamp.to_rfc3339(),
                event.run_id.to_string(),
                event.step_name.as_deref(),
                event.attempt,
                serde_json::to_string(&event.payload)?,
            ],
        )
        .context("failed to insert event projection")?;

        match event.event_type {
            EventType::StateChanged => {
                let payload = serde_json::from_value::<StateChangedPayload>(event.payload.clone())
                    .context("invalid STATE_CHANGED payload")?;
                let from_state = state_name(payload.from)?;
                let to_state = state_name(payload.to)?;
                tx.execute(
                    r#"
INSERT INTO state_events (event_id, run_id, from_state, to_state, timestamp)
VALUES (?1, ?2, ?3, ?4, ?5)
"#,
                    params![
                        event.event_id.to_string(),
                        event.run_id.to_string(),
                        from_state,
                        to_state,
                        event.timestamp.to_rfc3339(),
                    ],
                )
                .context("failed to insert state event projection")?;
                tx.execute(
                    r#"
INSERT INTO runs (run_id, state, updated_at)
VALUES (?1, ?2, ?3)
ON CONFLICT(run_id) DO UPDATE SET
    state = excluded.state,
    updated_at = excluded.updated_at
"#,
                    params![
                        event.run_id.to_string(),
                        state_name(payload.to)?,
                        event.timestamp.to_rfc3339(),
                    ],
                )
                .context("failed to update run state projection")?;
            }
            EventType::ManualRestart => {
                tx.execute(
                    r#"
INSERT INTO audit_events (event_id, run_id, action, timestamp, payload)
VALUES (?1, ?2, ?3, ?4, ?5)
"#,
                    params![
                        event.event_id.to_string(),
                        event.run_id.to_string(),
                        "MANUAL_RESTART",
                        event.timestamp.to_rfc3339(),
                        serde_json::to_string(&event.payload)?,
                    ],
                )
                .context("failed to insert audit event projection")?;
            }
            _ => {}
        }

        tx.commit()
            .context("failed to commit projection transaction")
    }

    pub fn table_count(&self, table: &str) -> Result<u64> {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        self.connection
            .query_row(&sql, [], |row| row.get::<_, u64>(0))
            .with_context(|| format!("failed to count table {table}"))
    }

    pub fn run_state(&self, run_id: impl ToString) -> Result<Option<String>> {
        let mut statement = self
            .connection
            .prepare("SELECT state FROM runs WHERE run_id = ?1")
            .context("failed to prepare run state query")?;
        let mut rows = statement
            .query(params![run_id.to_string()])
            .context("failed to query run state")?;

        match rows.next().context("failed to read run state row")? {
            Some(row) => row
                .get::<_, String>(0)
                .map(Some)
                .context("failed to read run state"),
            None => Ok(None),
        }
    }
}

fn event_type_name(event_type: EventType) -> Result<String> {
    let value = serde_json::to_value(event_type).context("failed to serialize event type")?;
    Ok(value.as_str().unwrap_or("UNKNOWN").to_owned())
}

fn state_name(state: RunState) -> Result<String> {
    let value = serde_json::to_value(state).context("failed to serialize run state")?;
    Ok(value.as_str().unwrap_or("UNKNOWN").to_owned())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::events::{EventStore, EventType};
    use crate::state::{RunState, StateChangedPayload};

    #[test]
    fn creates_projection_schema_with_all_tables() {
        let store = SqliteProjectionStore::in_memory().unwrap();
        for table in [
            "jobs",
            "runs",
            "steps",
            "attempts",
            "events",
            "metrics",
            "artifacts",
            "locks",
            "state_events",
            "audit_events",
            "plugins",
        ] {
            assert_eq!(store.table_count(table).unwrap(), 0);
        }
    }

    #[test]
    fn projects_state_after_event_exists_in_sqlite() {
        let root = std::env::temp_dir().join(format!("runflow-projection-{}", Uuid::new_v4()));
        let event_store = EventStore::new(&root);
        let mut projection = SqliteProjectionStore::in_memory().unwrap();
        let run_id = Uuid::new_v4();
        let event = FlowEvent::new(
            EventType::StateChanged,
            run_id,
            json!(StateChangedPayload {
                from: RunState::Created,
                to: RunState::Running,
            }),
        );

        event_store.append(&event).unwrap();
        projection.project_event(&event).unwrap();

        assert_eq!(projection.table_count("events").unwrap(), 1);
        assert_eq!(projection.table_count("state_events").unwrap(), 1);
        assert_eq!(
            projection.run_state(run_id).unwrap(),
            Some("RUNNING".to_owned())
        );

        std::fs::remove_dir_all(root).ok();
    }
}
