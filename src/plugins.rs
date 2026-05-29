use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::schemas::{self, SchemaKind};
use crate::supervisor::{CommandLimits, ProcessSupervisor};

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct PluginRef {
    pub id: String,
    pub version: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub version: String,
    pub contract_version: String,
    pub author: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginInput {
    pub contract_version: String,
    pub plugin: PluginRef,
    pub flow: PluginFlowContext,
    pub input: Value,
    pub env: HashMap<String, String>,
    pub paths: PluginPaths,
    pub limits: PluginLimits,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct PluginFlowContext {
    pub run_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct PluginPaths {
    pub work_dir: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct PluginLimits {
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginOutput {
    pub contract_version: String,
    pub status: PluginStatus,
    pub exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outputs: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<HashMap<String, f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errors: Option<Vec<PluginError>>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PluginStatus {
    Success,
    Failed,
    RetryableFailed,
    Timeout,
    Cancelled,
    ContractError,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct PluginError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct PluginRuntime;

impl PluginRuntime {
    pub async fn run(
        command: &str,
        cwd: impl AsRef<Path>,
        input: &PluginInput,
    ) -> Result<PluginOutput> {
        validate_input(input)?;
        let stdin = serde_json::to_vec(input).context("failed to serialize plugin input")?;
        let output = ProcessSupervisor::run_command_with_stdin(
            command,
            cwd,
            &stdin,
            CommandLimits::default(),
        )
        .await?;

        if output.exit_code != Some(0) {
            bail!(
                "plugin command failed with {:?}: {}",
                output.exit_code,
                output.stderr
            );
        }

        Self::parse_output(&output.stdout)
    }

    pub fn parse_output(source: &str) -> Result<PluginOutput> {
        let value =
            serde_json::from_str::<Value>(source).context("failed to parse plugin stdout")?;
        validate_output_value(&value)?;
        serde_json::from_value(value).context("failed to deserialize plugin output")
    }
}

pub fn parse_manifest(source: &str) -> Result<PluginManifest> {
    let manifest: PluginManifest =
        serde_json::from_str(source).context("failed to parse plugin manifest")?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn validate_manifest(manifest: &PluginManifest) -> Result<()> {
    if manifest.id.trim().is_empty() {
        bail!("plugin manifest id is required");
    }
    if manifest.version.trim().is_empty() {
        bail!("plugin manifest version is required");
    }
    if manifest.contract_version != "v1" {
        bail!(
            "unsupported plugin contract version: {}",
            manifest.contract_version
        );
    }
    Ok(())
}

fn validate_input(input: &PluginInput) -> Result<()> {
    let value = serde_json::to_value(input).context("failed to convert plugin input to JSON")?;
    let diagnostics = schemas::validate_value(SchemaKind::PluginInput, &value)?;
    if diagnostics.is_empty() {
        Ok(())
    } else {
        bail!("invalid plugin input: {:?}", diagnostics)
    }
}

fn validate_output_value(value: &Value) -> Result<()> {
    let diagnostics = schemas::validate_value(SchemaKind::PluginOutput, value)?;
    if diagnostics.is_empty() {
        Ok(())
    } else {
        bail!("invalid plugin output: {:?}", diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_plugin_output() {
        let output = PluginRuntime::parse_output(
            r#"{
  "contract_version": "v1",
  "status": "SUCCESS",
  "exit_code": 0,
  "outputs": {"answer": 42},
  "metrics": {"duration_ms": 10},
  "errors": []
}"#,
        )
        .unwrap();

        assert_eq!(output.status, PluginStatus::Success);
        assert_eq!(output.exit_code, 0);
    }

    #[test]
    fn parses_valid_plugin_manifest() {
        let manifest = parse_manifest(
            r#"{
  "id": "demo-plugin",
  "version": "0.1.0",
  "contract_version": "v1",
  "author": "TODO",
  "description": "TODO"
}"#,
        )
        .unwrap();

        assert_eq!(manifest.id, "demo-plugin");
    }

    #[test]
    fn serializes_plugin_input_without_null_optionals() {
        let input = PluginInput {
            contract_version: "v1".to_owned(),
            plugin: PluginRef {
                id: "demo".to_owned(),
                version: "1.0.0".to_owned(),
            },
            flow: PluginFlowContext {
                run_id: Uuid::new_v4(),
                step_id: Some("step".to_owned()),
                job_id: None,
            },
            input: serde_json::json!({}),
            env: HashMap::new(),
            paths: PluginPaths {
                work_dir: ".".to_owned(),
            },
            limits: PluginLimits { timeout_ms: 1000 },
        };

        let value = serde_json::to_value(input).unwrap();

        assert!(
            schemas::validate_value(SchemaKind::PluginInput, &value)
                .unwrap()
                .is_empty()
        );
        assert!(value["flow"].get("job_id").is_none());
    }
}
