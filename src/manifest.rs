use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::dag::{FailurePolicy, WorkflowDefinition};
use crate::schemas::{self, SchemaKind};
use crate::state::RunState;

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunManifest {
    pub workflow_version: String,
    pub schema_version: String,
    pub run_id: Uuid,
    pub job_id: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub status: ManifestStatus,
    pub failure_policy: String,
    pub artifacts: Vec<ManifestArtifact>,
    pub metrics: ManifestMetrics,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ManifestStatus {
    Success,
    Failed,
    Cancelled,
    Timeout,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManifestArtifact {
    pub step_id: String,
    pub path: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManifestMetrics {
    pub workflow_duration_ms: u64,
    pub retry_count: u64,
    pub step_count: u64,
    pub failed_step_count: u64,
}

pub fn write_run_manifest(
    run_dir: impl AsRef<Path>,
    workflow: &WorkflowDefinition,
    run_id: Uuid,
    started_at: DateTime<Utc>,
    ended_at: DateTime<Utc>,
    status: RunState,
    failed_step_count: u64,
) -> Result<RunManifest> {
    let manifest = build_run_manifest(
        workflow,
        run_id,
        started_at,
        ended_at,
        status,
        failed_step_count,
    )?;
    validate_manifest(&manifest)?;

    let path = manifest_path(run_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create manifest directory {}", parent.display()))?;
    }
    fs::write(&path, serde_json::to_vec_pretty(&manifest)?)
        .with_context(|| format!("failed to write run manifest {}", path.display()))?;

    Ok(manifest)
}

pub fn read_run_manifest(path: impl AsRef<Path>) -> Result<RunManifest> {
    let source = fs::read_to_string(path.as_ref())
        .with_context(|| format!("failed to read run manifest {}", path.as_ref().display()))?;
    let manifest =
        serde_json::from_str::<RunManifest>(&source).context("invalid run manifest JSON")?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn build_run_manifest(
    workflow: &WorkflowDefinition,
    run_id: Uuid,
    started_at: DateTime<Utc>,
    ended_at: DateTime<Utc>,
    status: RunState,
    failed_step_count: u64,
) -> Result<RunManifest> {
    let status = manifest_status(status)?;
    let duration_ms = (ended_at - started_at).num_milliseconds().max(0) as u64;
    Ok(RunManifest {
        workflow_version: workflow.version.to_string(),
        schema_version: workflow.schema_version.to_string(),
        run_id,
        job_id: workflow.id.clone(),
        started_at,
        ended_at,
        status,
        failure_policy: failure_policy_name(workflow.failure_policy).to_owned(),
        artifacts: Vec::new(),
        metrics: ManifestMetrics {
            workflow_duration_ms: duration_ms,
            retry_count: 0,
            step_count: workflow.steps.len() as u64,
            failed_step_count,
        },
    })
}

fn validate_manifest(manifest: &RunManifest) -> Result<()> {
    let value = serde_json::to_value(manifest).context("failed to convert manifest to JSON")?;
    let diagnostics = schemas::validate_value(SchemaKind::Manifest, &value)?;
    if diagnostics.is_empty() {
        Ok(())
    } else {
        bail!("invalid run manifest: {:?}", diagnostics)
    }
}

fn manifest_path(run_dir: impl AsRef<Path>) -> PathBuf {
    run_dir.as_ref().join("manifest.json")
}

fn manifest_status(status: RunState) -> Result<ManifestStatus> {
    match status {
        RunState::Success => Ok(ManifestStatus::Success),
        RunState::Failed => Ok(ManifestStatus::Failed),
        RunState::Cancelled => Ok(ManifestStatus::Cancelled),
        RunState::Timeout => Ok(ManifestStatus::Timeout),
        other => bail!("run state {other:?} cannot be written to a completion manifest"),
    }
}

fn failure_policy_name(policy: FailurePolicy) -> &'static str {
    match policy {
        FailurePolicy::Stop => "stop",
        FailurePolicy::Continue => "continue",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_reads_run_manifest() {
        let root = std::env::temp_dir().join(format!("runflow-manifest-{}", Uuid::new_v4()));
        let workflow = WorkflowDefinition::from_yaml(
            r#"
id: manifest-demo
version: 2
schema_version: 1
failure_policy: continue
steps:
  - id: hello
    type: command
    run: echo hello
"#,
        )
        .unwrap();
        let run_id = Uuid::new_v4();
        let started_at = Utc::now();
        let ended_at = started_at + chrono::Duration::milliseconds(42);

        let manifest = write_run_manifest(
            &root,
            &workflow,
            run_id,
            started_at,
            ended_at,
            RunState::Success,
            0,
        )
        .unwrap();
        let read = read_run_manifest(root.join("manifest.json")).unwrap();

        assert_eq!(read, manifest);
        assert_eq!(read.job_id, "manifest-demo");
        assert_eq!(read.metrics.workflow_duration_ms, 42);
        assert_eq!(read.failure_policy, "continue");

        fs::remove_dir_all(root).ok();
    }
}
