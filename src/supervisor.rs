use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use uuid::Uuid;

use crate::events::{EventType, FlowEvent};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CommandLimits {
    pub stdout_max: usize,
    pub stderr_max: usize,
}

impl Default for CommandLimits {
    fn default() -> Self {
        Self {
            stdout_max: 1024 * 1024,
            stderr_max: 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CommandOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

#[derive(Debug)]
pub struct ManagedProcess {
    child: Child,
    pid: u32,
    command: String,
    run_id: Uuid,
    step_id: Option<String>,
}

#[derive(Debug, Default)]
pub struct ProcessSupervisor;

impl ProcessSupervisor {
    pub async fn run_command(
        command: &str,
        cwd: impl AsRef<Path>,
        limits: CommandLimits,
    ) -> Result<CommandOutput> {
        shell_command(command)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .with_context(|| format!("failed to run command: {command}"))
            .map(|output| command_output(output, limits))
    }

    pub fn spawn_managed(
        command: &str,
        cwd: impl AsRef<Path>,
        run_id: Uuid,
        step_id: Option<String>,
    ) -> Result<(ManagedProcess, FlowEvent)> {
        let child = shell_command(command)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn command: {command}"))?;
        let pid = child
            .id()
            .context("spawned process did not expose a process id")?;
        let started = process_event(
            EventType::ProcessStarted,
            run_id,
            step_id.clone(),
            json!({ "pid": pid, "command": command }),
        );

        Ok((
            ManagedProcess {
                child,
                pid,
                command: command.to_owned(),
                run_id,
                step_id,
            },
            started,
        ))
    }

    pub async fn run_command_with_stdin(
        command: &str,
        cwd: impl AsRef<Path>,
        stdin: &[u8],
        limits: CommandLimits,
    ) -> Result<CommandOutput> {
        let mut child = shell_command(command)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn command: {command}"))?;

        if let Some(mut child_stdin) = child.stdin.take() {
            child_stdin
                .write_all(stdin)
                .await
                .context("failed to write child stdin")?;
        }

        let output = child
            .wait_with_output()
            .await
            .with_context(|| format!("failed to wait for command: {command}"))?;

        Ok(command_output(output, limits))
    }

    pub async fn kill_process_tree_events(
        run_id: Uuid,
        step_id: Option<String>,
        pid: u32,
        command: &str,
    ) -> Result<Vec<FlowEvent>> {
        kill_process_tree(pid).await?;
        Ok(vec![
            process_event(
                EventType::ProcessKilled,
                run_id,
                step_id.clone(),
                json!({ "pid": pid, "command": command }),
            ),
            process_event(
                EventType::ProcessTreeKilled,
                run_id,
                step_id,
                json!({ "root_pid": pid, "command": command }),
            ),
        ])
    }
}

impl ManagedProcess {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub async fn wait(self, limits: CommandLimits) -> Result<CommandOutput> {
        let output = self
            .child
            .wait_with_output()
            .await
            .with_context(|| format!("failed to wait for command: {}", self.command))?;
        Ok(command_output(output, limits))
    }

    pub async fn kill_tree(&mut self) -> Result<Vec<FlowEvent>> {
        let events = ProcessSupervisor::kill_process_tree_events(
            self.run_id,
            self.step_id.clone(),
            self.pid,
            &self.command,
        )
        .await?;
        let _ = self.child.kill().await;

        Ok(events)
    }
}

fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(command);
        cmd
    }

    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd
    }
}

async fn kill_process_tree(pid: u32) -> Result<()> {
    #[cfg(windows)]
    {
        let status = Command::new("taskkill")
            .arg("/PID")
            .arg(pid.to_string())
            .arg("/T")
            .arg("/F")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .context("failed to run taskkill")?;
        if !status.success() {
            bail!("taskkill failed for pid {pid}");
        }
        Ok(())
    }

    #[cfg(not(windows))]
    {
        let status = Command::new("sh")
            .arg("-c")
            .arg(format!("pkill -TERM -P {pid}; kill -TERM {pid}"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .context("failed to kill process tree")?;
        if !status.success() {
            bail!("process tree kill failed for pid {pid}");
        }
        Ok(())
    }
}

fn command_output(output: std::process::Output, limits: CommandLimits) -> CommandOutput {
    let (stdout, stdout_truncated) = truncate_bytes(output.stdout, limits.stdout_max);
    let (stderr, stderr_truncated) = truncate_bytes(output.stderr, limits.stderr_max);

    CommandOutput {
        exit_code: output.status.code(),
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
    }
}

fn process_event(
    event_type: EventType,
    run_id: Uuid,
    step_id: Option<String>,
    payload: serde_json::Value,
) -> FlowEvent {
    let mut event = FlowEvent::new(event_type, run_id, payload);
    event.step_id = step_id;
    event
}

fn truncate_bytes(bytes: Vec<u8>, max: usize) -> (String, bool) {
    let truncated = bytes.len() > max;
    let bytes = if truncated { &bytes[..max] } else { &bytes };
    (String::from_utf8_lossy(bytes).to_string(), truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn captures_command_stdout_and_limits() {
        let output = ProcessSupervisor::run_command(
            "echo hello",
            std::env::temp_dir(),
            CommandLimits {
                stdout_max: 3,
                stderr_max: 1024,
            },
        )
        .await
        .unwrap();

        assert_eq!(output.exit_code, Some(0));
        assert_eq!(output.stdout, "hel");
        assert!(output.stdout_truncated);
    }

    #[tokio::test]
    async fn spawns_managed_process_and_emits_started_event() {
        let run_id = Uuid::new_v4();
        let (process, event) = ProcessSupervisor::spawn_managed(
            "echo managed",
            std::env::temp_dir(),
            run_id,
            Some("step".to_owned()),
        )
        .unwrap();

        assert_eq!(event.event_type, EventType::ProcessStarted);
        assert_eq!(event.run_id, run_id);
        assert!(process.pid() > 0);

        let output = process.wait(CommandLimits::default()).await.unwrap();
        assert_eq!(output.exit_code, Some(0));
    }

    #[tokio::test]
    async fn kills_managed_process_tree_and_emits_events() {
        let command = long_running_command();
        let (mut process, _started) = ProcessSupervisor::spawn_managed(
            &command,
            std::env::temp_dir(),
            Uuid::new_v4(),
            Some("long".to_owned()),
        )
        .unwrap();

        let events = process.kill_tree().await.unwrap();

        assert_eq!(events[0].event_type, EventType::ProcessKilled);
        assert_eq!(events[1].event_type, EventType::ProcessTreeKilled);
    }

    fn long_running_command() -> String {
        #[cfg(windows)]
        {
            "ping 127.0.0.1 -n 6 > nul".to_owned()
        }

        #[cfg(not(windows))]
        {
            "sleep 5".to_owned()
        }
    }
}
