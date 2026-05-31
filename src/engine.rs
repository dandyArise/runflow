use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use tokio::time::{Instant, sleep, sleep_until};

use crate::dag::{StepDefinition, StepType};
use crate::logs;
use crate::plugins::{
    PluginFlowContext, PluginInput, PluginLimits, PluginPaths, PluginRef, PluginRuntime,
};
use crate::supervisor::{CommandLimits, CommandOutput, ManagedProcessLogs, ProcessSupervisor};
use crate::workspace::RunWorkspace;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RetryPolicy {
    pub attempts: u32,
    pub initial_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: 1,
            initial_delay: Duration::from_millis(0),
            max_delay: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StepOutcome {
    Command(CommandOutput),
    Sleep,
    WaitUntil,
    Plugin(Value),
}

#[derive(Debug, Default)]
pub struct WorkflowEngine;

impl WorkflowEngine {
    pub async fn execute_step(
        step: &StepDefinition,
        workspace: &RunWorkspace,
        retry: RetryPolicy,
    ) -> Result<StepOutcome> {
        Self::execute_step_with_events(step, workspace, retry)
            .await
            .map(|(outcome, _events)| outcome)
    }

    pub async fn execute_step_with_events(
        step: &StepDefinition,
        workspace: &RunWorkspace,
        retry: RetryPolicy,
    ) -> Result<(StepOutcome, Vec<crate::events::FlowEvent>)> {
        let attempts = retry.attempts.max(1);
        let mut delay = retry.initial_delay;
        let mut last_error = None;

        for attempt in 1..=attempts {
            match Self::execute_once(step, workspace).await {
                Ok(outcome) => return Ok(outcome),
                Err(error) => {
                    last_error = Some(error);
                    if attempt < attempts {
                        sleep(delay).await;
                        delay = next_backoff(delay, retry.max_delay);
                    }
                }
            }
        }

        Err(last_error.expect("retry loop always executes at least once"))
    }

    async fn execute_once(
        step: &StepDefinition,
        workspace: &RunWorkspace,
    ) -> Result<(StepOutcome, Vec<crate::events::FlowEvent>)> {
        match step.step_type {
            StepType::Command => {
                let run = step.run.as_ref().context("command step missing run")?;
                let started_at = Utc::now();
                let mut prepared = logs::prepare_step_logs(
                    &workspace.root_dir,
                    workspace.run_id,
                    &step.name,
                    run,
                    run.working_directory().map(str::to_owned),
                    started_at,
                )?;
                let stderr_path = prepared.stderr_path.clone();
                let (stdout_file, stderr_file) = prepared.take_stdio()?;
                let (process, started) = match ProcessSupervisor::spawn_managed_run(
                    run,
                    &workspace.work_dir,
                    workspace.run_id,
                    Some(step.name.clone()),
                    ManagedProcessLogs {
                        stdout: stdout_file,
                        stderr: stderr_file,
                        stdout_path: prepared.stdout_path.clone(),
                        stderr_path: prepared.stderr_path.clone(),
                    },
                ) {
                    Ok(result) => result,
                    Err(error) => {
                        let error = format!("{error:#}");
                        logs::write_spawn_error(&stderr_path, &error)?;
                        logs::write_step_finished(prepared, Utc::now(), None, Some(error.clone()))?;
                        bail!("command step {} failed to spawn: {error}", step.name);
                    }
                };
                let output = process.wait_logged(CommandLimits::default()).await?;
                logs::write_step_finished(prepared, Utc::now(), output.exit_code, None)?;
                if output.exit_code != Some(0) {
                    bail!(
                        "command step {} failed with {:?}",
                        step.name,
                        output.exit_code
                    );
                }
                Ok((StepOutcome::Command(output), vec![started]))
            }
            StepType::Sleep => {
                let duration = parse_duration(
                    step.duration
                        .as_deref()
                        .or(step.command.as_deref())
                        .context("sleep step missing duration")?,
                )?;
                sleep(duration).await;
                Ok((StepOutcome::Sleep, Vec::new()))
            }
            StepType::WaitUntil => {
                let target = step
                    .command
                    .as_deref()
                    .context("wait_until step missing command timestamp")?
                    .parse::<DateTime<Utc>>()
                    .context("invalid wait_until timestamp")?;
                wait_until(target).await;
                Ok((StepOutcome::WaitUntil, Vec::new()))
            }
            StepType::Plugin => {
                let run = step.run.as_ref().context("plugin step missing run")?;
                let command = run.display_command();
                let plugin_id = step
                    .plugin_id
                    .clone()
                    .context("plugin step missing plugin_id")?;
                let input = PluginInput {
                    contract_version: "v1".to_owned(),
                    plugin: PluginRef {
                        id: plugin_id,
                        version: "1.0.0".to_owned(),
                    },
                    flow: PluginFlowContext {
                        run_id: workspace.run_id,
                        step_name: Some(step.name.clone()),
                        job_name: None,
                    },
                    input: json!({}),
                    env: Default::default(),
                    paths: PluginPaths {
                        work_dir: workspace.work_dir.display().to_string(),
                    },
                    limits: PluginLimits { timeout_ms: 30_000 },
                };
                let output = PluginRuntime::run(&command, &workspace.work_dir, &input).await?;
                Ok((
                    StepOutcome::Plugin(output.outputs.unwrap_or_else(|| json!({}))),
                    Vec::new(),
                ))
            }
        }
    }
}

fn parse_duration(source: &str) -> Result<Duration> {
    let (number, unit) = source.trim().split_at(
        source
            .trim()
            .find(|ch: char| !ch.is_ascii_digit())
            .unwrap_or(source.trim().len()),
    );
    let value = number.parse::<u64>().context("invalid duration value")?;
    match unit {
        "ms" => Ok(Duration::from_millis(value)),
        "s" => Ok(Duration::from_secs(value)),
        "m" => Ok(Duration::from_secs(value * 60)),
        "h" => Ok(Duration::from_secs(value * 60 * 60)),
        _ => bail!("invalid duration unit: {unit}"),
    }
}

async fn wait_until(target: DateTime<Utc>) {
    let now = Utc::now();
    if target <= now {
        return;
    }
    if let Ok(duration) = (target - now).to_std() {
        sleep_until(Instant::now() + duration).await;
    }
}

fn next_backoff(current: Duration, max: Duration) -> Duration {
    if current.is_zero() {
        return Duration::from_millis(1).min(max);
    }
    (current * 2).min(max)
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::dag::{RunCommandDefinition, RunDefinition, StepType};
    use crate::workspace::WorkspaceIsolation;

    #[tokio::test]
    async fn executes_command_step_in_workspace() {
        let root = std::env::temp_dir().join(format!("runflow-engine-{}", Uuid::new_v4()));
        let workspace = WorkspaceIsolation::new(&root)
            .create(Uuid::new_v4())
            .unwrap();
        let step = StepDefinition {
            name: "hello".to_owned(),
            step_type: StepType::Command,
            depends_on: vec![],
            run: Some(RunDefinition::Command(RunCommandDefinition {
                command: "rustc".to_owned(),
                args: vec!["--version".to_owned()],
                working_directory: None,
            })),
            plugin_id: None,
            duration: None,
            command: None,
            interval: None,
        };

        let outcome = WorkflowEngine::execute_step(&step, &workspace, RetryPolicy::default())
            .await
            .unwrap();

        match outcome {
            StepOutcome::Command(output) => assert!(output.stdout.contains("rustc")),
            _ => panic!("expected command outcome"),
        }

        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn command_step_returns_process_started_event() {
        let root = std::env::temp_dir().join(format!("runflow-engine-events-{}", Uuid::new_v4()));
        let workspace = WorkspaceIsolation::new(&root)
            .create(Uuid::new_v4())
            .unwrap();
        let step = StepDefinition {
            name: "hello".to_owned(),
            step_type: StepType::Command,
            depends_on: vec![],
            run: Some(RunDefinition::Command(RunCommandDefinition {
                command: "rustc".to_owned(),
                args: vec!["--version".to_owned()],
                working_directory: None,
            })),
            plugin_id: None,
            duration: None,
            command: None,
            interval: None,
        };

        let (_outcome, events) =
            WorkflowEngine::execute_step_with_events(&step, &workspace, RetryPolicy::default())
                .await
                .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].event_type,
            crate::events::EventType::ProcessStarted
        );
        assert_eq!(events[0].step_name.as_deref(), Some("hello"));

        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn executes_sleep_step() {
        let root = std::env::temp_dir().join(format!("runflow-sleep-{}", Uuid::new_v4()));
        let workspace = WorkspaceIsolation::new(&root)
            .create(Uuid::new_v4())
            .unwrap();
        let step = StepDefinition {
            name: "pause".to_owned(),
            step_type: StepType::Sleep,
            depends_on: vec![],
            run: None,
            plugin_id: None,
            duration: Some("1ms".to_owned()),
            command: None,
            interval: None,
        };

        assert_eq!(
            WorkflowEngine::execute_step(&step, &workspace, RetryPolicy::default())
                .await
                .unwrap(),
            StepOutcome::Sleep
        );

        std::fs::remove_dir_all(root).ok();
    }
}
