use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SchemaKind {
    Workflow,
    Event,
    Manifest,
    PluginInput,
    PluginOutput,
    Snapshot,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct ValidationDiagnostic {
    pub path: String,
    pub message: String,
}

impl SchemaKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Workflow => "workflow",
            Self::Event => "event",
            Self::Manifest => "manifest",
            Self::PluginInput => "plugin-input",
            Self::PluginOutput => "plugin-output",
            Self::Snapshot => "snapshot",
        }
    }

    pub fn schema_json(self) -> &'static str {
        match self {
            Self::Workflow => include_str!("schema_defs/v1/workflow.schema.json"),
            Self::Event => include_str!("schema_defs/v1/event.schema.json"),
            Self::Manifest => include_str!("schema_defs/v1/manifest.schema.json"),
            Self::PluginInput => include_str!("schema_defs/v1/plugin-input.schema.json"),
            Self::PluginOutput => include_str!("schema_defs/v1/plugin-output.schema.json"),
            Self::Snapshot => include_str!("schema_defs/v1/snapshot.schema.json"),
        }
    }
}

pub fn validate_workflow_file(path: &Path) -> Result<Vec<ValidationDiagnostic>> {
    let source =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    validate_workflow_yaml(&source)
}

pub fn validate_workflow_yaml(source: &str) -> Result<Vec<ValidationDiagnostic>> {
    let instance =
        serde_yaml::from_str::<Value>(source).context("failed to parse workflow YAML")?;
    validate_value(SchemaKind::Workflow, &instance)
}

pub fn validate_json_str(kind: SchemaKind, source: &str) -> Result<Vec<ValidationDiagnostic>> {
    let instance = serde_json::from_str::<Value>(source)
        .with_context(|| format!("failed to parse {} JSON", kind.name()))?;
    validate_value(kind, &instance)
}

pub fn validate_value(kind: SchemaKind, instance: &Value) -> Result<Vec<ValidationDiagnostic>> {
    let schema = serde_json::from_str::<Value>(kind.schema_json())
        .with_context(|| format!("failed to parse {} schema", kind.name()))?;
    let validator = jsonschema::validator_for(&schema)
        .with_context(|| format!("failed to compile {} schema", kind.name()))?;

    Ok(validator
        .iter_errors(instance)
        .map(|error| ValidationDiagnostic {
            path: error.instance_path().to_string(),
            message: error.to_string(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_minimal_workflow_yaml() {
        let diagnostics = validate_workflow_yaml(
            r#"
name: backup-db
version: 1
schema_version: 1
steps:
  - name: dump
    type: command
    run:
      command: rustc
      args: ["--version"]
"#,
        )
        .expect("workflow validation should run");

        assert_eq!(diagnostics, Vec::new());
    }

    #[test]
    fn reports_structured_workflow_errors() {
        let diagnostics = validate_workflow_yaml(
            r#"
name: Bad Name
version: 0
schema_version: 1
steps: []
"#,
        )
        .expect("workflow validation should run");

        assert!(diagnostics.iter().any(|item| item.path == "/name"));
        assert!(diagnostics.iter().any(|item| item.path == "/version"));
        assert!(diagnostics.iter().any(|item| item.path == "/steps"));
    }

    #[test]
    fn validates_event_json() {
        let diagnostics = validate_json_str(
            SchemaKind::Event,
            r#"{
  "event_id": "018f53e4-5c2f-7c95-b3be-87039f9bc3a1",
  "event_type": "RUN_STARTED",
  "event_version": 1,
  "timestamp": "2026-05-29T12:00:00Z",
  "run_id": "018f53e4-5c2f-7c95-b3be-87039f9bc3a2",
  "trace_id": "018f53e4-5c2f-7c95-b3be-87039f9bc3a3",
  "span_id": "018f53e4-5c2f-7c95-b3be-87039f9bc3a4",
  "correlation_id": "018f53e4-5c2f-7c95-b3be-87039f9bc3a5",
  "payload": {}
}"#,
        )
        .expect("event validation should run");

        assert_eq!(diagnostics, Vec::new());
    }

    #[test]
    fn validates_plugin_contracts() {
        let input = validate_json_str(
            SchemaKind::PluginInput,
            r#"{
  "contract_version": "v1",
  "plugin": { "id": "demo", "version": "1.0.0" },
  "flow": { "run_id": "018f53e4-5c2f-7c95-b3be-87039f9bc3a2" },
  "input": {},
  "env": {},
  "paths": { "work_dir": ".flow/runs/demo/workspace" },
  "limits": { "timeout_ms": 30000 }
}"#,
        )
        .expect("plugin input validation should run");

        let output = validate_json_str(
            SchemaKind::PluginOutput,
            r#"{
  "contract_version": "v1",
  "status": "SUCCESS",
  "exit_code": 0,
  "outputs": {},
  "metrics": {},
  "errors": []
}"#,
        )
        .expect("plugin output validation should run");

        assert_eq!(input, Vec::new());
        assert_eq!(output, Vec::new());
    }

    #[test]
    fn validates_snapshot_json() {
        let diagnostics = validate_json_str(
            SchemaKind::Snapshot,
            r#"{
  "snapshot_version": 1,
  "run_id": "018f53e4-5c2f-7c95-b3be-87039f9bc3a2",
  "last_event_id": "018f53e4-5c2f-7c95-b3be-87039f9bc3a1",
  "created_at": "2026-05-29T12:00:00Z",
  "state": {
    "run_status": "RUNNING",
    "steps": {
      "dump": { "status": "SUCCESS", "attempt": 1, "outputs": {} }
    }
  }
}"#,
        )
        .expect("snapshot validation should run");

        assert_eq!(diagnostics, Vec::new());
    }
}
