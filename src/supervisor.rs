use std::fs::File;
use std::path::{Path, PathBuf};
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
    stdout_path: Option<PathBuf>,
    stderr_path: Option<PathBuf>,
    run_id: Uuid,
    step_name: Option<String>,
}

pub struct ManagedProcessLogs {
    pub stdout: File,
    pub stderr: File,
    pub stdout_path: PathBuf,
    pub stderr_path: PathBuf,
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
        step_name: Option<String>,
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
            step_name.clone(),
            json!({ "pid": pid, "command": command }),
        );

        Ok((
            ManagedProcess {
                child,
                pid,
                command: command.to_owned(),
                stdout_path: None,
                stderr_path: None,
                run_id,
                step_name,
            },
            started,
        ))
    }

    pub fn spawn_managed_run(
        run: &crate::dag::RunDefinition,
        cwd: impl AsRef<Path>,
        run_id: Uuid,
        step_name: Option<String>,
        logs: ManagedProcessLogs,
    ) -> Result<(ManagedProcess, FlowEvent)> {
        let cwd = cwd.as_ref();
        let mut command = run_command(run, cwd);
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::from(logs.stdout))
            .stderr(Stdio::from(logs.stderr))
            .spawn()
            .with_context(|| format!("failed to spawn command: {}", run.display_command()))?;
        let pid = child
            .id()
            .context("spawned process did not expose a process id")?;
        let started = process_event(
            EventType::ProcessStarted,
            run_id,
            step_name.clone(),
            json!({
                "pid": pid,
                "command": run.command(),
                "args": run.args(),
                "working_directory": run.working_directory(),
                "legacy_shell": run.is_legacy_shell()
            }),
        );

        Ok((
            ManagedProcess {
                child,
                pid,
                command: run.display_command(),
                stdout_path: Some(logs.stdout_path),
                stderr_path: Some(logs.stderr_path),
                run_id,
                step_name,
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
        step_name: Option<String>,
        pid: u32,
        command: &str,
    ) -> Result<Vec<FlowEvent>> {
        kill_process_tree(pid).await?;
        Ok(vec![
            process_event(
                EventType::ProcessKilled,
                run_id,
                step_name.clone(),
                json!({ "pid": pid, "command": command }),
            ),
            process_event(
                EventType::ProcessTreeKilled,
                run_id,
                step_name,
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

    pub async fn wait_logged(mut self, limits: CommandLimits) -> Result<CommandOutput> {
        let status = self
            .child
            .wait()
            .await
            .with_context(|| format!("failed to wait for command: {}", self.command))?;
        let stdout = read_limited(self.stdout_path.as_deref(), limits.stdout_max)?;
        let stderr = read_limited(self.stderr_path.as_deref(), limits.stderr_max)?;
        Ok(CommandOutput {
            exit_code: status.code(),
            stdout: stdout.0,
            stderr: stderr.0,
            stdout_truncated: stdout.1,
            stderr_truncated: stderr.1,
        })
    }

    pub async fn kill_tree(&mut self) -> Result<Vec<FlowEvent>> {
        let events = ProcessSupervisor::kill_process_tree_events(
            self.run_id,
            self.step_name.clone(),
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

fn run_command(run: &crate::dag::RunDefinition, cwd: &Path) -> Command {
    match run {
        crate::dag::RunDefinition::LegacyShell(command) => {
            let mut command = shell_command(command);
            command.current_dir(cwd);
            command
        }
        crate::dag::RunDefinition::Command(run) => {
            let mut command = Command::new(&run.command);
            command.args(&run.args);
            command.current_dir(resolve_working_directory(
                cwd,
                run.working_directory.as_deref(),
            ));
            command
        }
    }
}

fn resolve_working_directory(cwd: &Path, working_directory: Option<&str>) -> PathBuf {
    let Some(working_directory) = working_directory else {
        return cwd.to_path_buf();
    };
    let path = Path::new(working_directory);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
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

fn read_limited(path: Option<&Path>, max: usize) -> Result<(String, bool)> {
    let Some(path) = path else {
        return Ok((String::new(), false));
    };
    let bytes = std::fs::read(path)
        .with_context(|| format!("failed to read command log {}", path.display()))?;
    Ok(truncate_bytes(bytes, max))
}

fn process_event(
    event_type: EventType,
    run_id: Uuid,
    step_name: Option<String>,
    payload: serde_json::Value,
) -> FlowEvent {
    let mut event = FlowEvent::new(event_type, run_id, payload);
    event.step_name = step_name;
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
