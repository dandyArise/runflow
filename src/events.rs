use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::schemas::{self, SchemaKind};

pub const CURRENT_EVENT_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EventType {
    FlowCreated,
    FlowUpdated,
    JobRegistered,
    JobDeleted,
    RunCreated,
    RunStarted,
    RunFinished,
    RunReplaced,
    StepStarted,
    StepFinished,
    StepFailed,
    AttemptStarted,
    AttemptFailed,
    PluginStarted,
    PluginFinished,
    ArtifactCreated,
    LockAcquired,
    LockReleased,
    StateChanged,
    ManualRestart,
    DaemonStarted,
    DaemonStopped,
    ProcessStarted,
    ProcessKilled,
    ProcessTreeKilled,
    RetentionStarted,
    RetentionFinished,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowEvent {
    pub event_id: Uuid,
    pub event_type: EventType,
    pub event_version: u32,
    pub timestamp: DateTime<Utc>,
    pub run_id: Uuid,
    pub trace_id: Uuid,
    pub span_id: Uuid,
    pub correlation_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    pub payload: Value,
}

impl FlowEvent {
    pub fn new(event_type: EventType, run_id: Uuid, payload: Value) -> Self {
        let trace_id = Uuid::new_v4();

        Self {
            event_id: Uuid::new_v4(),
            event_type,
            event_version: CURRENT_EVENT_VERSION,
            timestamp: Utc::now(),
            run_id,
            trace_id,
            span_id: Uuid::new_v4(),
            correlation_id: trace_id,
            step_name: None,
            attempt: None,
            payload,
        }
    }

    pub fn with_step(mut self, step_name: impl Into<String>) -> Self {
        self.step_name = Some(step_name.into());
        self
    }

    pub fn with_attempt(mut self, attempt: u32) -> Self {
        self.attempt = Some(attempt);
        self
    }
}

#[derive(Debug, Clone)]
pub struct EventStore {
    root: PathBuf,
}

impl EventStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn append(&self, event: &FlowEvent) -> Result<()> {
        validate_event(event)?;

        let path = self.events_path(event.run_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        serde_json::to_writer(&mut file, event).context("failed to serialize event")?;
        file.write_all(b"\n").context("failed to append newline")?;

        Ok(())
    }

    pub fn read_run(&self, run_id: Uuid) -> Result<Vec<FlowEvent>> {
        let path = self.events_path(run_id);
        if !path.exists() {
            return Ok(Vec::new());
        }

        let file = OpenOptions::new()
            .read(true)
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;

        BufReader::new(file)
            .lines()
            .enumerate()
            .filter_map(|(index, line)| match line {
                Ok(line) if line.trim().is_empty() => None,
                Ok(line) => Some(read_event_line(&line, index + 1)),
                Err(error) => Some(Err(error.into())),
            })
            .collect()
    }

    pub fn replay(&self, run_id: Uuid) -> Result<Vec<FlowEvent>> {
        self.read_run(run_id)
    }

    pub fn events_path(&self, run_id: Uuid) -> PathBuf {
        self.run_dir(run_id).join("events.jsonl")
    }

    pub fn run_dir(&self, run_id: Uuid) -> PathBuf {
        self.root
            .join(".flow")
            .join("runs")
            .join(run_id.to_string())
    }
}

#[derive(Debug, Default)]
pub struct EventMigrator;

impl EventMigrator {
    pub fn migrate_value(mut value: Value) -> Result<Value> {
        let mut version = value
            .get("event_version")
            .and_then(Value::as_u64)
            .context("event_version is missing or invalid")? as u32;

        while version < CURRENT_EVENT_VERSION {
            value = Self::migrate_one(value, version)?;
            version += 1;
        }

        if version > CURRENT_EVENT_VERSION {
            bail!("unsupported future event version: {version}");
        }

        Ok(value)
    }

    fn migrate_one(mut value: Value, from_version: u32) -> Result<Value> {
        match from_version {
            1 => {
                value["event_version"] = json!(2);
                Ok(value)
            }
            version => bail!("unsupported event version: {version}"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct EventBus {
    sender: broadcast::Sender<FlowEvent>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<FlowEvent> {
        self.sender.subscribe()
    }

    pub fn publish(&self, event: FlowEvent) -> Result<usize> {
        self.sender.send(event).context("failed to publish event")
    }
}

pub trait EventSubscriber {
    fn name(&self) -> &'static str;
    fn on_event(&self, event: &FlowEvent) -> Result<()>;
}

fn read_event_line(line: &str, line_number: usize) -> Result<FlowEvent> {
    let value = serde_json::from_str::<Value>(line)
        .with_context(|| format!("failed to parse event JSONL line {line_number}"))?;
    let value = EventMigrator::migrate_value(value)
        .with_context(|| format!("failed to migrate event JSONL line {line_number}"))?;

    validate_event_value(&value)
        .with_context(|| format!("event JSONL line {line_number} failed schema validation"))?;
    serde_json::from_value(value)
        .with_context(|| format!("failed to deserialize event JSONL line {line_number}"))
}

fn validate_event(event: &FlowEvent) -> Result<()> {
    let value = serde_json::to_value(event).context("failed to convert event to JSON")?;
    validate_event_value(&value)
}

fn validate_event_value(value: &Value) -> Result<()> {
    let diagnostics = schemas::validate_value(SchemaKind::Event, value)?;
    if diagnostics.is_empty() {
        return Ok(());
    }

    let messages = diagnostics
        .into_iter()
        .map(|item| format!("{}: {}", item.path, item.message))
        .collect::<Vec<_>>()
        .join("; ");
    bail!("invalid event: {messages}");
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn temp_store() -> (PathBuf, EventStore) {
        let root = std::env::temp_dir().join(format!("runflow-test-{}", Uuid::new_v4()));
        let store = EventStore::new(&root);
        (root, store)
    }

    #[test]
    fn writes_reads_and_replays_events_jsonl() {
        let (root, store) = temp_store();
        let run_id = Uuid::new_v4();
        let event = FlowEvent::new(EventType::RunStarted, run_id, json!({ "source": "test" }));

        store.append(&event).expect("append should succeed");

        let events = store.read_run(run_id).expect("read should succeed");
        let replayed = store.replay(run_id).expect("replay should succeed");

        assert_eq!(events, vec![event.clone()]);
        assert_eq!(replayed, vec![event]);
        assert!(
            store
                .events_path(run_id)
                .ends_with(Path::new("events.jsonl"))
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn migrates_event_version_one_to_current_version() {
        let run_id = Uuid::new_v4();
        let value = json!({
            "event_id": Uuid::new_v4(),
            "event_type": "RUN_STARTED",
            "event_version": 1,
            "timestamp": Utc::now(),
            "run_id": run_id,
            "trace_id": Uuid::new_v4(),
            "span_id": Uuid::new_v4(),
            "correlation_id": Uuid::new_v4(),
            "payload": {}
        });

        let migrated = EventMigrator::migrate_value(value).expect("migration should succeed");
        assert_eq!(migrated["event_version"], json!(CURRENT_EVENT_VERSION));
    }

    #[tokio::test]
    async fn broadcasts_events_to_subscribers() {
        let bus = EventBus::new(8);
        let mut state = bus.subscribe();
        let mut metrics = bus.subscribe();
        let mut audit = bus.subscribe();
        let mut notification = bus.subscribe();
        let event = FlowEvent::new(EventType::RunStarted, Uuid::new_v4(), json!({}));

        let receivers = bus.publish(event.clone()).expect("publish should succeed");

        assert_eq!(receivers, 4);
        assert_eq!(state.recv().await.expect("state event"), event);
        assert_eq!(metrics.recv().await.expect("metrics event"), event);
        assert_eq!(audit.recv().await.expect("audit event"), event);
        assert_eq!(
            notification.recv().await.expect("notification event"),
            event
        );
    }
}
