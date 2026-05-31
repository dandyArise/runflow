use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunRequest {
    pub run_id: Uuid,
    pub job_name: String,
    pub enqueued_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub pid: u32,
    pub started_at: DateTime<Utc>,
    pub heartbeat_at: DateTime<Utc>,
    pub state: DaemonState,
    pub active_run: Option<Uuid>,
    pub queued_runs: usize,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DaemonState {
    Starting,
    Idle,
    Running,
    Stopping,
    Stopped,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActiveProcess {
    pub run_id: Uuid,
    pub step_name: String,
    pub pid: u32,
    pub command: String,
    pub started_at: DateTime<Utc>,
}

pub fn start_daemon(root: &Path) -> Result<u32> {
    if is_daemon_running(root) {
        bail!("daemon already running");
    }
    let paths = DaemonPaths::new(root);
    fs::create_dir_all(&paths.daemon_dir)
        .with_context(|| format!("failed to create {}", paths.daemon_dir.display()))?;
    remove_file_if_exists(&paths.stop_file)?;

    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.log_file())
        .context("failed to open daemon log")?;
    let err = log.try_clone().context("failed to clone daemon log")?;
    let child = Command::new(exe)
        .arg("--root")
        .arg(root)
        .arg("daemon")
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(err))
        .spawn()
        .context("failed to start daemon process")?;

    std::thread::sleep(Duration::from_millis(300));
    Ok(child.id())
}

pub fn is_daemon_running(root: &Path) -> bool {
    let paths = DaemonPaths::new(root);
    paths.pid_file.exists() && paths.status_file.exists() && !paths.stop_file.exists()
}

pub fn request_stop(root: &Path) -> Result<()> {
    let paths = DaemonPaths::new(root);
    fs::create_dir_all(&paths.daemon_dir)?;
    fs::write(paths.stop_file, Utc::now().to_rfc3339()).context("failed to write stop request")
}

pub fn stop_requested(root: &Path) -> bool {
    DaemonPaths::new(root).stop_file.exists()
}

pub fn write_status(root: &Path, status: &DaemonStatus) -> Result<()> {
    let paths = DaemonPaths::new(root);
    fs::create_dir_all(&paths.daemon_dir)?;
    fs::write(paths.pid_file, status.pid.to_string()).context("failed to write daemon pid")?;
    fs::write(
        paths.status_file,
        serde_json::to_vec_pretty(status).context("failed to serialize daemon status")?,
    )
    .context("failed to write daemon status")
}

pub fn read_status(root: &Path) -> Result<Option<DaemonStatus>> {
    let paths = DaemonPaths::new(root);
    if !paths.status_file.exists() {
        return Ok(None);
    }
    let source = fs::read_to_string(paths.status_file).context("failed to read daemon status")?;
    serde_json::from_str(&source)
        .map(Some)
        .context("failed to parse daemon status")
}

pub fn clear_daemon_files(root: &Path) -> Result<()> {
    let paths = DaemonPaths::new(root);
    let stopped = DaemonStatus {
        pid: std::process::id(),
        started_at: Utc::now(),
        heartbeat_at: Utc::now(),
        state: DaemonState::Stopped,
        active_run: None,
        queued_runs: queued_runs(root).unwrap_or(0),
    };
    write_status(root, &stopped)?;
    remove_file_if_exists(&paths.pid_file)?;
    remove_file_if_exists(&paths.stop_file)
}

pub fn enqueue_run(root: &Path, request: &RunRequest) -> Result<PathBuf> {
    let paths = DaemonPaths::new(root);
    fs::create_dir_all(&paths.queue_dir)?;
    let path = paths.queue_file(request.run_id);
    fs::write(
        &path,
        serde_json::to_vec_pretty(request).context("failed to serialize run request")?,
    )
    .with_context(|| format!("failed to enqueue run {}", request.run_id))?;
    Ok(path)
}

pub fn next_run_request(root: &Path) -> Result<Option<RunRequest>> {
    let Some(path) = next_queue_file(root)? else {
        return Ok(None);
    };
    let source = fs::read_to_string(&path)
        .with_context(|| format!("failed to read queue file {}", path.display()))?;
    serde_json::from_str(&source)
        .map(Some)
        .with_context(|| format!("failed to parse queue file {}", path.display()))
}

pub fn remove_queued_run(root: &Path, run_id: Uuid) -> Result<bool> {
    let path = DaemonPaths::new(root).queue_file(run_id);
    if path.exists() {
        fs::remove_file(path).context("failed to remove queued run")?;
        Ok(true)
    } else {
        Ok(false)
    }
}

pub fn pop_run_request(root: &Path, run_id: Uuid) -> Result<()> {
    let path = DaemonPaths::new(root).queue_file(run_id);
    remove_file_if_exists(&path)
}

pub fn queued_runs(root: &Path) -> Result<usize> {
    let paths = DaemonPaths::new(root);
    if !paths.queue_dir.exists() {
        return Ok(0);
    }
    Ok(fs::read_dir(paths.queue_dir)?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().and_then(|value| value.to_str()) == Some("json"))
        .count())
}

pub fn write_active_process(root: &Path, active: &ActiveProcess) -> Result<()> {
    let paths = DaemonPaths::new(root);
    fs::create_dir_all(&paths.process_dir)?;
    fs::write(
        paths.process_file(active.run_id),
        serde_json::to_vec_pretty(active).context("failed to serialize active process")?,
    )
    .context("failed to write active process")
}

pub fn read_active_process(root: &Path, run_id: Uuid) -> Result<Option<ActiveProcess>> {
    let path = DaemonPaths::new(root).process_file(run_id);
    if !path.exists() {
        return Ok(None);
    }
    let source = fs::read_to_string(&path)
        .with_context(|| format!("failed to read active process {}", path.display()))?;
    serde_json::from_str(&source)
        .map(Some)
        .with_context(|| format!("failed to parse active process {}", path.display()))
}

pub fn clear_active_process(root: &Path, run_id: Uuid) -> Result<()> {
    remove_file_if_exists(&DaemonPaths::new(root).process_file(run_id))
}

pub fn request_cancel(root: &Path, run_id: Uuid) -> Result<()> {
    let paths = DaemonPaths::new(root);
    fs::create_dir_all(&paths.cancel_dir)?;
    fs::write(paths.cancel_file(run_id), Utc::now().to_rfc3339())
        .context("failed to write cancel request")
}

pub fn cancel_requested(root: &Path, run_id: Uuid) -> bool {
    DaemonPaths::new(root).cancel_file(run_id).exists()
}

pub fn clear_cancel(root: &Path, run_id: Uuid) -> Result<()> {
    remove_file_if_exists(&DaemonPaths::new(root).cancel_file(run_id))
}

fn next_queue_file(root: &Path) -> Result<Option<PathBuf>> {
    let paths = DaemonPaths::new(root);
    if !paths.queue_dir.exists() {
        return Ok(None);
    }
    let mut entries = fs::read_dir(paths.queue_dir)?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().and_then(|value| value.to_str()) == Some("json"))
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, entry.path()))
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    Ok(entries.into_iter().next().map(|(_, path)| path))
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

struct DaemonPaths {
    daemon_dir: PathBuf,
    queue_dir: PathBuf,
    process_dir: PathBuf,
    cancel_dir: PathBuf,
    pid_file: PathBuf,
    status_file: PathBuf,
    stop_file: PathBuf,
}

impl DaemonPaths {
    fn new(root: &Path) -> Self {
        let flow = root.join(".flow");
        let daemon_dir = flow.join("daemon");
        Self {
            queue_dir: daemon_dir.join("queue"),
            process_dir: daemon_dir.join("processes"),
            cancel_dir: daemon_dir.join("cancel"),
            pid_file: daemon_dir.join("daemon.pid"),
            status_file: daemon_dir.join("status.json"),
            stop_file: daemon_dir.join("stop"),
            daemon_dir,
        }
    }

    fn queue_file(&self, run_id: Uuid) -> PathBuf {
        self.queue_dir.join(format!("{run_id}.json"))
    }

    fn process_file(&self, run_id: Uuid) -> PathBuf {
        self.process_dir.join(format!("{run_id}.json"))
    }

    fn cancel_file(&self, run_id: Uuid) -> PathBuf {
        self.cancel_dir.join(format!("{run_id}.cancel"))
    }

    fn log_file(&self) -> PathBuf {
        self.daemon_dir.join("daemon.log")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enqueues_and_reads_run_requests() {
        let root = std::env::temp_dir().join(format!("runflow-daemon-{}", Uuid::new_v4()));
        let request = RunRequest {
            run_id: Uuid::new_v4(),
            job_name: "demo".to_owned(),
            enqueued_at: Utc::now(),
        };

        enqueue_run(&root, &request).unwrap();
        assert_eq!(queued_runs(&root).unwrap(), 1);
        assert_eq!(next_run_request(&root).unwrap(), Some(request.clone()));
        pop_run_request(&root, request.run_id).unwrap();
        assert_eq!(queued_runs(&root).unwrap(), 0);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn tracks_cancel_and_active_process_files() {
        let root = std::env::temp_dir().join(format!("runflow-daemon-{}", Uuid::new_v4()));
        let run_id = Uuid::new_v4();
        let active = ActiveProcess {
            run_id,
            step_name: "step".to_owned(),
            pid: 123,
            command: "echo hi".to_owned(),
            started_at: Utc::now(),
        };

        write_active_process(&root, &active).unwrap();
        request_cancel(&root, run_id).unwrap();

        assert_eq!(read_active_process(&root, run_id).unwrap(), Some(active));
        assert!(cancel_requested(&root, run_id));

        clear_active_process(&root, run_id).unwrap();
        clear_cancel(&root, run_id).unwrap();
        assert!(read_active_process(&root, run_id).unwrap().is_none());
        assert!(!cancel_requested(&root, run_id));

        fs::remove_dir_all(root).ok();
    }
}
