use std::fs::{self, File};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::dag::{RunDefinition, WorkflowDefinition};
use crate::state::RunState;

const SHELL_WARNING: &str =
    "Shell command detected. This step is platform-specific and may not be portable.";
const LEGACY_SHELL_WARNING: &str =
    "Legacy shell run string detected. Prefer run.command with separated run.args.";

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct WorkflowLogMetadata {
    pub workflow_id: String,
    pub workflow_name: String,
    pub status: WorkflowLogStatus,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub duration_ms: Option<u128>,
    pub total_steps: usize,
    pub successful_steps: usize,
    pub failed_steps: usize,
    pub log_dir: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct StepLogMetadata {
    pub workflow_id: String,
    pub step_id: String,
    pub status: StepLogStatus,
    pub command: String,
    pub args: Vec<String>,
    pub working_directory: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub duration_ms: Option<u128>,
    pub exit_code: Option<i32>,
    pub spawn_error: Option<String>,
    pub stdout_log: String,
    pub stderr_log: String,
    pub is_shell: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WorkflowLogStatus {
    Running,
    Success,
    Failed,
    Cancelled,
    Timeout,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StepLogStatus {
    Running,
    Success,
    Failed,
}

pub struct PreparedStepLogs {
    pub metadata: StepLogMetadata,
    pub stdout_file: Option<File>,
    pub stderr_file: Option<File>,
    pub stdout_path: PathBuf,
    pub stderr_path: PathBuf,
    pub metadata_path: PathBuf,
}

impl PreparedStepLogs {
    pub fn take_stdio(&mut self) -> Result<(File, File)> {
        let stdout = self
            .stdout_file
            .take()
            .context("stdout log file already consumed")?;
        let stderr = self
            .stderr_file
            .take()
            .context("stderr log file already consumed")?;
        Ok((stdout, stderr))
    }
}

pub fn workflow_log_dir(root: &Path, run_id: Uuid) -> PathBuf {
    root.join("logs").join(run_id.to_string())
}

pub fn write_workflow_started(
    root: &Path,
    run_id: Uuid,
    workflow: &WorkflowDefinition,
    started_at: DateTime<Utc>,
) -> Result<WorkflowLogMetadata> {
    let metadata = WorkflowLogMetadata {
        workflow_id: run_id.to_string(),
        workflow_name: workflow.name.clone(),
        status: WorkflowLogStatus::Running,
        started_at: started_at.to_rfc3339(),
        finished_at: None,
        duration_ms: None,
        total_steps: workflow.steps.len(),
        successful_steps: 0,
        failed_steps: 0,
        log_dir: format!("logs/{run_id}"),
    };
    write_workflow_metadata(root, run_id, &metadata)?;
    Ok(metadata)
}

pub fn write_workflow_finished(
    root: &Path,
    run_id: Uuid,
    mut metadata: WorkflowLogMetadata,
    finished_at: DateTime<Utc>,
    status: RunState,
    successful_steps: usize,
    failed_steps: usize,
) -> Result<WorkflowLogMetadata> {
    let started_at = DateTime::parse_from_rfc3339(&metadata.started_at)
        .context("invalid workflow log started_at")?
        .with_timezone(&Utc);
    metadata.status = workflow_log_status(status);
    metadata.finished_at = Some(finished_at.to_rfc3339());
    metadata.duration_ms = Some((finished_at - started_at).num_milliseconds().max(0) as u128);
    metadata.successful_steps = successful_steps;
    metadata.failed_steps = failed_steps;
    write_workflow_metadata(root, run_id, &metadata)?;
    Ok(metadata)
}

pub fn prepare_step_logs(
    root: &Path,
    run_id: Uuid,
    step_name: &str,
    run: &RunDefinition,
    working_directory: Option<String>,
    started_at: DateTime<Utc>,
) -> Result<PreparedStepLogs> {
    let step_log_dir = workflow_log_dir(root, run_id).join(step_name);
    fs::create_dir_all(&step_log_dir).with_context(|| {
        format!(
            "failed to create step log directory {}",
            step_log_dir.display()
        )
    })?;

    let stdout_path = step_log_dir.join("stdout.log");
    let stderr_path = step_log_dir.join("stderr.log");
    let metadata_path = step_log_dir.join("step.metadata.json");
    let stdout_file = File::create(&stdout_path)
        .with_context(|| format!("failed to create stdout log {}", stdout_path.display()))?;
    let stderr_file = File::create(&stderr_path)
        .with_context(|| format!("failed to create stderr log {}", stderr_path.display()))?;
    let metadata = StepLogMetadata {
        workflow_id: run_id.to_string(),
        step_id: step_name.to_owned(),
        status: StepLogStatus::Running,
        command: run.command().to_owned(),
        args: run.args().to_vec(),
        working_directory,
        started_at: started_at.to_rfc3339(),
        finished_at: None,
        duration_ms: None,
        exit_code: None,
        spawn_error: None,
        stdout_log: "stdout.log".to_owned(),
        stderr_log: "stderr.log".to_owned(),
        is_shell: run.is_legacy_shell() || is_shell_command(run.command()),
        warnings: run_warnings(run),
    };
    write_json(&metadata_path, &metadata)?;
    Ok(PreparedStepLogs {
        metadata,
        stdout_file: Some(stdout_file),
        stderr_file: Some(stderr_file),
        stdout_path,
        stderr_path,
        metadata_path,
    })
}

pub fn write_step_finished(
    prepared: PreparedStepLogs,
    finished_at: DateTime<Utc>,
    exit_code: Option<i32>,
    spawn_error: Option<String>,
) -> Result<StepLogMetadata> {
    let started_at = DateTime::parse_from_rfc3339(&prepared.metadata.started_at)
        .context("invalid step log started_at")?
        .with_timezone(&Utc);
    let mut metadata = prepared.metadata;
    metadata.finished_at = Some(finished_at.to_rfc3339());
    metadata.duration_ms = Some((finished_at - started_at).num_milliseconds().max(0) as u128);
    metadata.exit_code = exit_code;
    metadata.spawn_error = spawn_error;
    metadata.status = if metadata.exit_code == Some(0) {
        StepLogStatus::Success
    } else {
        StepLogStatus::Failed
    };
    write_json(&prepared.metadata_path, &metadata)?;
    Ok(metadata)
}

pub fn write_spawn_error(stderr_path: &Path, error: &str) -> Result<()> {
    fs::write(stderr_path, format!("Process spawn error: {error}\n"))
        .with_context(|| format!("failed to write spawn error to {}", stderr_path.display()))
}

pub fn is_shell_command(command: &str) -> bool {
    let normalized = command.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "cmd"
            | "cmd.exe"
            | "powershell"
            | "powershell.exe"
            | "pwsh"
            | "pwsh.exe"
            | "bash"
            | "sh"
            | "zsh"
    )
}

fn run_warnings(run: &RunDefinition) -> Vec<String> {
    let mut warnings = Vec::new();
    if run.is_legacy_shell() {
        warnings.push(LEGACY_SHELL_WARNING.to_owned());
    }
    if is_shell_command(run.command()) {
        warnings.push(SHELL_WARNING.to_owned());
    }
    warnings
}

fn write_workflow_metadata(
    root: &Path,
    run_id: Uuid,
    metadata: &WorkflowLogMetadata,
) -> Result<()> {
    let path = workflow_log_dir(root, run_id).join("workflow.metadata.json");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create workflow log directory {}",
                parent.display()
            )
        })?;
    }
    write_json(&path, metadata)
}

fn workflow_log_status(status: RunState) -> WorkflowLogStatus {
    match status {
        RunState::Success => WorkflowLogStatus::Success,
        RunState::Failed => WorkflowLogStatus::Failed,
        RunState::Cancelled => WorkflowLogStatus::Cancelled,
        RunState::Timeout => WorkflowLogStatus::Timeout,
        _ => WorkflowLogStatus::Failed,
    }
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    fs::write(path, serde_json::to_vec_pretty(value)?)
        .with_context(|| format!("failed to write JSON file {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::{RunCommandDefinition, RunDefinition};

    #[test]
    fn detects_shell_commands() {
        assert!(is_shell_command("cmd"));
        assert!(is_shell_command("PowerShell.exe"));
        assert!(!is_shell_command("ping"));
    }

    #[test]
    fn writes_step_metadata_with_shell_warning() {
        let root = std::env::temp_dir().join(format!("runflow-logs-{}", Uuid::new_v4()));
        let run_id = Uuid::new_v4();
        let run = RunDefinition::Command(RunCommandDefinition {
            command: "cmd".to_owned(),
            args: vec!["/C".to_owned(), "dir".to_owned()],
            working_directory: None,
        });

        let prepared = prepare_step_logs(&root, run_id, "list", &run, None, Utc::now()).unwrap();
        let metadata = write_step_finished(prepared, Utc::now(), Some(0), None).unwrap();

        assert_eq!(metadata.status, StepLogStatus::Success);
        assert!(metadata.is_shell);
        assert!(!metadata.warnings.is_empty());
        assert!(
            workflow_log_dir(&root, run_id)
                .join("list")
                .join("step.metadata.json")
                .exists()
        );

        fs::remove_dir_all(root).ok();
    }
}
