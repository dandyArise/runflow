use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde_json::json;
use uuid::Uuid;

use crate::events::{EventSubscriber, EventType, FlowEvent};
use crate::state::{RunState, StateChangedPayload, StateEngine};

#[derive(Debug, Default)]
pub struct AuditProjection {
    manual_restarts: Mutex<Vec<Uuid>>,
}

#[derive(Debug, Default)]
pub struct MetricsProjection {
    totals: Mutex<HashMap<String, f64>>,
    metrics_path: Option<PathBuf>,
}

#[derive(Debug, Default)]
pub struct NotificationProjection;

#[derive(Debug, Default)]
pub struct StateProjection {
    engine: Mutex<StateEngine>,
}

impl AuditProjection {
    pub fn manual_restart_count(&self) -> Result<usize> {
        Ok(self
            .manual_restarts
            .lock()
            .map_err(|error| anyhow::anyhow!("audit projection lock poisoned: {error}"))?
            .len())
    }
}

impl MetricsProjection {
    pub fn with_metrics_path(path: impl Into<PathBuf>) -> Self {
        Self {
            totals: Mutex::new(HashMap::new()),
            metrics_path: Some(path.into()),
        }
    }

    pub fn total(&self, metric_name: &str) -> Result<Option<f64>> {
        Ok(self
            .totals
            .lock()
            .map_err(|error| anyhow::anyhow!("metrics projection lock poisoned: {error}"))?
            .get(metric_name)
            .copied())
    }
}

impl StateProjection {
    pub fn current(&self, run_id: Uuid) -> Result<Option<RunState>> {
        Ok(self
            .engine
            .lock()
            .map_err(|error| anyhow::anyhow!("state projection lock poisoned: {error}"))?
            .current(run_id))
    }
}

impl EventSubscriber for AuditProjection {
    fn name(&self) -> &'static str {
        "audit"
    }

    fn on_event(&self, event: &FlowEvent) -> Result<()> {
        if event.event_type == EventType::ManualRestart {
            self.manual_restarts
                .lock()
                .map_err(|error| anyhow::anyhow!("audit projection lock poisoned: {error}"))?
                .push(event.event_id);
        }

        Ok(())
    }
}

impl EventSubscriber for MetricsProjection {
    fn name(&self) -> &'static str {
        "metrics"
    }

    fn on_event(&self, event: &FlowEvent) -> Result<()> {
        let Some(metrics) = event
            .payload
            .get("metrics")
            .and_then(|value| value.as_object())
        else {
            return Ok(());
        };

        let mut totals = self
            .totals
            .lock()
            .map_err(|error| anyhow::anyhow!("metrics projection lock poisoned: {error}"))?;
        for (name, value) in metrics {
            let Some(value) = value.as_f64() else {
                continue;
            };
            *totals.entry(name.clone()).or_default() += value;

            if let Some(path) = &self.metrics_path {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("failed to create {}", parent.display()))?;
                }
                let mut file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .with_context(|| format!("failed to open {}", path.display()))?;
                let line = json!({
                    "run_id": event.run_id,
                    "event_id": event.event_id,
                    "metric_name": name,
                    "metric_value": value,
                    "timestamp": event.timestamp,
                });
                serde_json::to_writer(&mut file, &line).context("failed to write metric JSON")?;
                file.write_all(b"\n")
                    .context("failed to write metric newline")?;
            }
        }

        Ok(())
    }
}

impl EventSubscriber for NotificationProjection {
    fn name(&self) -> &'static str {
        "notification"
    }

    fn on_event(&self, _event: &FlowEvent) -> Result<()> {
        Ok(())
    }
}

impl EventSubscriber for StateProjection {
    fn name(&self) -> &'static str {
        "state"
    }

    fn on_event(&self, event: &FlowEvent) -> Result<()> {
        if event.event_type != EventType::StateChanged {
            return Ok(());
        }

        let payload = serde_json::from_value::<StateChangedPayload>(event.payload.clone())
            .context("invalid STATE_CHANGED payload")?;
        self.engine
            .lock()
            .map_err(|error| anyhow::anyhow!("state projection lock poisoned: {error}"))?
            .apply_state_changed(event.run_id, payload)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::events::{EventType, FlowEvent};

    #[test]
    fn projections_consume_supported_events() {
        let run_id = Uuid::new_v4();
        let state_event = FlowEvent::new(
            EventType::StateChanged,
            run_id,
            json!(StateChangedPayload {
                from: RunState::Created,
                to: RunState::Running,
            }),
        );
        let metric_event = FlowEvent::new(
            EventType::RunFinished,
            run_id,
            json!({ "metrics": { "workflow_duration_ms": 42.0 } }),
        );
        let audit_event = FlowEvent::new(EventType::ManualRestart, run_id, json!({}));

        let state = StateProjection::default();
        let metrics = MetricsProjection::default();
        let audit = AuditProjection::default();
        let notification = NotificationProjection;

        state.on_event(&state_event).unwrap();
        metrics.on_event(&metric_event).unwrap();
        audit.on_event(&audit_event).unwrap();
        notification.on_event(&audit_event).unwrap();

        assert_eq!(state.current(run_id).unwrap(), Some(RunState::Running));
        assert_eq!(metrics.total("workflow_duration_ms").unwrap(), Some(42.0));
        assert_eq!(audit.manual_restart_count().unwrap(), 1);
        assert_eq!(notification.name(), "notification");
    }

    #[test]
    fn metrics_projection_appends_metrics_jsonl() {
        let path = std::env::temp_dir()
            .join(format!("runflow-metrics-{}", Uuid::new_v4()))
            .join("metrics.jsonl");
        let projection = MetricsProjection::with_metrics_path(&path);
        let event = FlowEvent::new(
            EventType::RunFinished,
            Uuid::new_v4(),
            json!({ "metrics": { "retry_count": 2.0 } }),
        );

        projection.on_event(&event).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("\"metric_name\":\"retry_count\""));
        assert_eq!(projection.total("retry_count").unwrap(), Some(2.0));

        fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}
