use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::events::{EventStore, EventType, FlowEvent};
use crate::schemas::{self, SchemaKind};
use crate::state::{RunState, StateChangedPayload};

pub const SNAPSHOT_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct SnapshotEngine {
    root: PathBuf,
    every_n_events: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSnapshot {
    pub snapshot_version: u32,
    pub run_id: Uuid,
    pub last_event_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub state: SnapshotState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotState {
    pub run_status: RunState,
    pub steps: HashMap<String, SnapshotStepState>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotStepState {
    pub status: String,
    pub attempt: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outputs: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotMeta {
    pub snapshot_path: String,
    pub run_id: Uuid,
    pub last_event_id: Uuid,
    pub created_at: DateTime<Utc>,
}

impl SnapshotEngine {
    pub fn new(root: impl Into<PathBuf>, every_n_events: usize) -> Self {
        Self {
            root: root.into(),
            every_n_events: every_n_events.max(1),
        }
    }

    pub fn should_snapshot(&self, event_count: usize) -> bool {
        event_count > 0 && event_count.is_multiple_of(self.every_n_events)
    }

    pub fn write_snapshot(
        &self,
        run_id: Uuid,
        last_event_id: Uuid,
        state: SnapshotState,
    ) -> Result<RunSnapshot> {
        let snapshot = RunSnapshot {
            snapshot_version: SNAPSHOT_VERSION,
            run_id,
            last_event_id,
            created_at: Utc::now(),
            state,
        };
        validate_snapshot(&snapshot)?;

        let path = self.snapshot_path(run_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::write(&path, serde_json::to_vec_pretty(&snapshot)?)
            .with_context(|| format!("failed to write snapshot {}", path.display()))?;

        let meta = SnapshotMeta {
            snapshot_path: path.display().to_string(),
            run_id,
            last_event_id,
            created_at: snapshot.created_at,
        };
        fs::write(self.meta_path(run_id), serde_json::to_vec_pretty(&meta)?)
            .context("failed to write snapshot meta")?;

        Ok(snapshot)
    }

    pub fn read_snapshot(&self, run_id: Uuid) -> Result<RunSnapshot> {
        read_snapshot_file(self.snapshot_path(run_id))
    }

    pub fn restore_run_state(&self, run_id: Uuid, event_store: &EventStore) -> Result<RunState> {
        let snapshot = self.read_snapshot(run_id)?;
        let events = event_store.read_run(run_id)?;
        restore_state_from_snapshot(snapshot, &events)
    }

    pub fn snapshot_path(&self, run_id: Uuid) -> PathBuf {
        self.root
            .join(".flow")
            .join("snapshots")
            .join(format!("{run_id}.snapshot"))
    }

    pub fn meta_path(&self, run_id: Uuid) -> PathBuf {
        self.root
            .join(".flow")
            .join("snapshots")
            .join(format!("{run_id}.snapshot.meta"))
    }
}

pub fn read_snapshot_file(path: impl AsRef<Path>) -> Result<RunSnapshot> {
    let source = fs::read_to_string(path.as_ref())
        .with_context(|| format!("failed to read snapshot {}", path.as_ref().display()))?;
    let snapshot = serde_json::from_str::<RunSnapshot>(&source).context("invalid snapshot JSON")?;
    validate_snapshot(&snapshot)?;
    Ok(snapshot)
}

pub fn restore_state_from_snapshot(
    snapshot: RunSnapshot,
    events: &[FlowEvent],
) -> Result<RunState> {
    let mut state = snapshot.state.run_status;
    let mut after_snapshot = false;

    for event in events {
        if event.event_id == snapshot.last_event_id {
            after_snapshot = true;
            continue;
        }
        if !after_snapshot {
            continue;
        }
        if event.event_type == EventType::StateChanged {
            let payload = serde_json::from_value::<StateChangedPayload>(event.payload.clone())
                .context("invalid STATE_CHANGED payload during snapshot restore")?;
            if payload.from != state {
                bail!(
                    "snapshot restore state mismatch: expected {:?}, got {:?}",
                    state,
                    payload.from
                );
            }
            state = payload.to;
        }
    }

    Ok(state)
}

fn validate_snapshot(snapshot: &RunSnapshot) -> Result<()> {
    let value = serde_json::to_value(snapshot).context("failed to convert snapshot to JSON")?;
    let diagnostics = schemas::validate_value(SchemaKind::Snapshot, &value)?;
    if diagnostics.is_empty() {
        Ok(())
    } else {
        bail!("invalid snapshot: {:?}", diagnostics)
    }
}

pub fn snapshot_state(run_status: RunState) -> SnapshotState {
    SnapshotState {
        run_status,
        steps: HashMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::events::{EventType, FlowEvent};

    #[test]
    fn writes_snapshot_and_meta() {
        let root = std::env::temp_dir().join(format!("runflow-snapshot-{}", Uuid::new_v4()));
        let engine = SnapshotEngine::new(&root, 2);
        let run_id = Uuid::new_v4();
        let last_event_id = Uuid::new_v4();

        let snapshot = engine
            .write_snapshot(run_id, last_event_id, snapshot_state(RunState::Running))
            .unwrap();

        assert_eq!(snapshot.run_id, run_id);
        assert!(engine.snapshot_path(run_id).exists());
        assert!(engine.meta_path(run_id).exists());
        assert!(engine.should_snapshot(2));
        assert!(!engine.should_snapshot(3));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn restores_snapshot_plus_delta_events() {
        let root = std::env::temp_dir().join(format!("runflow-restore-{}", Uuid::new_v4()));
        let event_store = EventStore::new(&root);
        let engine = SnapshotEngine::new(&root, 2);
        let run_id = Uuid::new_v4();

        let first = FlowEvent::new(
            EventType::StateChanged,
            run_id,
            json!(StateChangedPayload {
                from: RunState::Created,
                to: RunState::Running,
            }),
        );
        let second = FlowEvent::new(
            EventType::StateChanged,
            run_id,
            json!(StateChangedPayload {
                from: RunState::Running,
                to: RunState::Success,
            }),
        );
        event_store.append(&first).unwrap();
        event_store.append(&second).unwrap();

        engine
            .write_snapshot(run_id, first.event_id, snapshot_state(RunState::Running))
            .unwrap();

        assert_eq!(
            engine.restore_run_state(run_id, &event_store).unwrap(),
            RunState::Success
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn replay_from_zero_matches_snapshot_restore() {
        let root = std::env::temp_dir().join(format!("runflow-replay-{}", Uuid::new_v4()));
        let event_store = EventStore::new(&root);
        let engine = SnapshotEngine::new(&root, 2);
        let run_id = Uuid::new_v4();
        let events = [
            FlowEvent::new(
                EventType::StateChanged,
                run_id,
                json!(StateChangedPayload {
                    from: RunState::Created,
                    to: RunState::Running,
                }),
            ),
            FlowEvent::new(
                EventType::StateChanged,
                run_id,
                json!(StateChangedPayload {
                    from: RunState::Running,
                    to: RunState::Failed,
                }),
            ),
        ];
        for event in &events {
            event_store.append(event).unwrap();
        }

        engine
            .write_snapshot(
                run_id,
                events[0].event_id,
                snapshot_state(RunState::Running),
            )
            .unwrap();

        let replayed = restore_state_from_snapshot(
            RunSnapshot {
                snapshot_version: SNAPSHOT_VERSION,
                run_id,
                last_event_id: events[0].event_id,
                created_at: Utc::now(),
                state: snapshot_state(RunState::Running),
            },
            &event_store.read_run(run_id).unwrap(),
        )
        .unwrap();

        assert_eq!(
            replayed,
            engine.restore_run_state(run_id, &event_store).unwrap()
        );

        fs::remove_dir_all(root).ok();
    }
}
