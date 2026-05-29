use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::json;
use tokio::time::{Duration, sleep};
use uuid::Uuid;

use crate::daemon::{self, ActiveProcess, DaemonState, DaemonStatus, RunRequest};
use crate::dag::{StepDefinition, StepType, WorkflowDefinition, WorkflowGraph};
use crate::engine::{RetryPolicy, WorkflowEngine};
use crate::events::{EventStore, EventType, FlowEvent};
use crate::manifest::write_run_manifest;
use crate::packages::{
    build_package_from_workflow_file, read_package_or_legacy_workflow, write_package,
};
use crate::plugins::{PluginInput, PluginManifest, PluginRuntime, parse_manifest};
use crate::retention::{RetentionPolicy, run_retention};
use crate::schemas;
use crate::state::{RunState, StateChangedPayload};
use crate::supervisor::{CommandLimits, ProcessSupervisor};
use crate::workspace::{RunWorkspace, WorkspaceIsolation};

#[derive(Debug, Parser)]
#[command(name = "flow")]
#[command(about = "RunFlow workflow runner")]
pub struct Cli {
    #[arg(long, global = true, default_value = ".")]
    root: PathBuf,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    Job {
        #[command(subcommand)]
        command: JobCommand,
    },
    Run {
        #[command(subcommand)]
        command: RunCommand,
    },
    Step {
        #[command(subcommand)]
        command: StepCommand,
    },
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },
    Package {
        #[command(subcommand)]
        command: PackageCommand,
    },
    Test {
        job_id: String,
        #[arg(long)]
        verbose: bool,
    },
    Validate {
        workflow: PathBuf,
    },
    Migrate {
        workflow: PathBuf,
    },
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    Retention {
        #[command(subcommand)]
        command: RetentionCommand,
    },
    Version,
}

#[derive(Debug, Subcommand)]
enum JobCommand {
    Add { workflow: PathBuf },
    List,
    Show { job_id: String },
    Run { job_id: String },
}

#[derive(Debug, Subcommand)]
enum RunCommand {
    List,
    Show { run_id: Uuid },
    Logs { run_id: Uuid },
    Cancel { run_id: Uuid },
}

#[derive(Debug, Subcommand)]
enum StepCommand {
    Retry { run_id: Uuid, step_id: String },
    Restart { run_id: Uuid, step_id: String },
    Reset { run_id: Uuid, step_id: String },
    Skip { run_id: Uuid, step_id: String },
    RerunFrom { run_id: Uuid, step_id: String },
}

#[derive(Debug, Subcommand)]
enum PluginCommand {
    Validate {
        manifest: PathBuf,
    },
    Inspect {
        manifest: PathBuf,
    },
    Test {
        command: String,
        sample: PathBuf,
    },
    List,
    Init {
        language: PluginLanguage,
        path: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum PluginLanguage {
    Rust,
    Java,
    Python,
    Node,
}

#[derive(Debug, Subcommand)]
enum PackageCommand {
    Build { workflow: PathBuf },
    Install { package: PathBuf },
}

#[derive(Debug, Subcommand)]
enum DaemonCommand {
    Start,
    Status,
    Stop,
    Restart,
    #[command(hide = true)]
    Serve,
}

#[derive(Debug, Subcommand)]
enum RetentionCommand {
    Run {
        #[arg(long)]
        keep_runs: Option<usize>,
        #[arg(long)]
        older_than_days: Option<u64>,
        #[arg(long)]
        dry_run: bool,
    },
}

impl Cli {
    pub async fn run(self) -> Result<()> {
        let root = self.root;
        match self.command.unwrap_or(Command::Version) {
            Command::Job { command } => run_job_command(&root, command).await,
            Command::Run { command } => run_run_command(&root, command).await,
            Command::Step { command } => run_step_command(&root, command),
            Command::Plugin { command } => run_plugin_command(&root, command).await,
            Command::Package { command } => run_package_command(&root, command),
            Command::Test { job_id, verbose } => run_test_command(&root, &job_id, verbose),
            Command::Validate { workflow } => validate_workflow(&workflow),
            Command::Migrate { workflow } => {
                validate_workflow(&workflow)?;
                println!("migration not required: {}", workflow.display());
                Ok(())
            }
            Command::Daemon { command } => run_daemon_command(&root, command).await,
            Command::Retention { command } => run_retention_command(&root, command),
            Command::Version => {
                println!("runflow {}", env!("CARGO_PKG_VERSION"));
                Ok(())
            }
        }
    }
}

async fn run_job_command(root: &Path, command: JobCommand) -> Result<()> {
    match command {
        JobCommand::Add { workflow } => {
            let source = fs::read_to_string(&workflow)
                .with_context(|| format!("failed to read {}", workflow.display()))?;
            let definition = WorkflowDefinition::from_yaml(&source)?;
            fs::create_dir_all(jobs_dir(root)).context("failed to create jobs directory")?;
            fs::write(job_path(root, &definition.id), source)
                .with_context(|| format!("failed to store job {}", definition.id))?;
            println!("job added: {}", definition.id);
            Ok(())
        }
        JobCommand::List => {
            for job_id in list_job_ids(root)? {
                println!("{job_id}");
            }
            Ok(())
        }
        JobCommand::Show { job_id } => {
            print!("{}", read_job_source(root, &job_id)?);
            Ok(())
        }
        JobCommand::Run { job_id } => run_job(root, &job_id).await,
    }
}

async fn run_job(root: &Path, job_id: &str) -> Result<()> {
    if daemon::is_daemon_running(root) {
        let run_id = enqueue_job_run(root, job_id)?;
        println!("{run_id}");
        return Ok(());
    }

    let run_id = Uuid::new_v4();
    run_job_direct(root, job_id, run_id, RunState::Created, true, false).await?;
    println!("{run_id}");
    Ok(())
}

fn enqueue_job_run(root: &Path, job_id: &str) -> Result<Uuid> {
    read_job_source(root, job_id)?;
    let run_id = Uuid::new_v4();
    let event_store = EventStore::new(root);
    event_store.append(&FlowEvent::new(
        EventType::RunCreated,
        run_id,
        json!({ "job_id": job_id, "source": "daemon_queue" }),
    ))?;
    event_store.append(&FlowEvent::new(
        EventType::StateChanged,
        run_id,
        json!(StateChangedPayload {
            from: RunState::Created,
            to: RunState::Queued,
        }),
    ))?;
    daemon::enqueue_run(
        root,
        &RunRequest {
            run_id,
            job_id: job_id.to_owned(),
            enqueued_at: Utc::now(),
        },
    )?;
    Ok(run_id)
}

async fn run_job_direct(
    root: &Path,
    job_id: &str,
    run_id: Uuid,
    from_state: RunState,
    create_event: bool,
    daemon_mode: bool,
) -> Result<RunState> {
    let source = read_job_source(root, job_id)?;
    let workflow = WorkflowDefinition::from_yaml(&source)?;
    let graph = WorkflowGraph::build(&workflow)?;
    let started_at = Utc::now();
    let event_store = EventStore::new(root);
    let workspace = WorkspaceIsolation::new(root).create(run_id)?;

    if create_event {
        event_store.append(&FlowEvent::new(
            EventType::RunCreated,
            run_id,
            json!({ "job_id": job_id }),
        ))?;
    }
    event_store.append(&FlowEvent::new(
        EventType::StateChanged,
        run_id,
        json!(StateChangedPayload {
            from: from_state,
            to: RunState::Running,
        }),
    ))?;
    event_store.append(&FlowEvent::new(
        EventType::RunStarted,
        run_id,
        json!({ "job_id": job_id }),
    ))?;

    let mut status = RunState::Success;
    let mut failed_step_count = 0;
    let ordered = graph.ordered_steps()?;
    for step_id in ordered {
        if daemon_mode && daemon::cancel_requested(root, run_id) {
            status = RunState::Cancelled;
            break;
        }
        let step = workflow
            .steps
            .iter()
            .find(|step| step.id == step_id)
            .context("ordered step missing from workflow")?;
        match execute_step_for_run(root, run_id, step, &workspace, daemon_mode).await {
            Ok((_outcome, events)) => {
                for event in events {
                    event_store.append(&event)?;
                }
                if daemon_mode && daemon::cancel_requested(root, run_id) {
                    status = RunState::Cancelled;
                    break;
                }
                event_store.append(&FlowEvent::new(
                    EventType::StepFinished,
                    run_id,
                    json!({ "step_id": step.id }),
                ))?;
            }
            Err(error) => {
                if daemon_mode && daemon::cancel_requested(root, run_id) {
                    status = RunState::Cancelled;
                    break;
                }
                event_store.append(&FlowEvent::new(
                    EventType::StepFailed,
                    run_id,
                    json!({ "step_id": step.id, "error": error.to_string() }),
                ))?;
                failed_step_count += 1;
                status = RunState::Failed;
                break;
            }
        }
    }

    event_store.append(&FlowEvent::new(
        EventType::StateChanged,
        run_id,
        json!(StateChangedPayload {
            from: RunState::Running,
            to: status,
        }),
    ))?;
    event_store.append(&FlowEvent::new(
        EventType::RunFinished,
        run_id,
        json!({ "status": format!("{status:?}") }),
    ))?;
    write_run_manifest(
        &workspace.run_dir,
        &workflow,
        run_id,
        started_at,
        Utc::now(),
        status,
        failed_step_count,
    )?;

    if daemon_mode {
        daemon::clear_cancel(root, run_id)?;
    }
    Ok(status)
}

async fn execute_step_for_run(
    root: &Path,
    run_id: Uuid,
    step: &StepDefinition,
    workspace: &RunWorkspace,
    daemon_mode: bool,
) -> Result<(crate::engine::StepOutcome, Vec<FlowEvent>)> {
    if daemon_mode && step.step_type == StepType::Command {
        let command = step.run.as_deref().context("command step missing run")?;
        let (process, started) = ProcessSupervisor::spawn_managed(
            command,
            &workspace.work_dir,
            run_id,
            Some(step.id.clone()),
        )?;
        daemon::write_active_process(
            root,
            &ActiveProcess {
                run_id,
                step_id: step.id.clone(),
                pid: process.pid(),
                command: command.to_owned(),
                started_at: Utc::now(),
            },
        )?;
        let output = process.wait(CommandLimits::default()).await;
        daemon::clear_active_process(root, run_id)?;
        let output = output?;
        if output.exit_code != Some(0) {
            bail!(
                "command step {} failed with {:?}",
                step.id,
                output.exit_code
            );
        }
        return Ok((crate::engine::StepOutcome::Command(output), vec![started]));
    }

    WorkflowEngine::execute_step_with_events(step, workspace, RetryPolicy::default()).await
}

async fn run_run_command(root: &Path, command: RunCommand) -> Result<()> {
    match command {
        RunCommand::List => {
            for run_id in list_run_ids(root)? {
                println!("{run_id}");
            }
            Ok(())
        }
        RunCommand::Show { run_id } | RunCommand::Logs { run_id } => {
            for event in EventStore::new(root).read_run(run_id)? {
                println!("{}", serde_json::to_string(&event)?);
            }
            Ok(())
        }
        RunCommand::Cancel { run_id } => {
            cancel_run(root, run_id).await?;
            Ok(())
        }
    }
}

async fn cancel_run(root: &Path, run_id: Uuid) -> Result<()> {
    daemon::request_cancel(root, run_id)?;
    let store = EventStore::new(root);

    if let Some(active) = daemon::read_active_process(root, run_id)? {
        for event in ProcessSupervisor::kill_process_tree_events(
            run_id,
            Some(active.step_id),
            active.pid,
            &active.command,
        )
        .await?
        {
            store.append(&event)?;
        }
        println!("cancel requested and process killed: {run_id}");
        return Ok(());
    }

    if daemon::remove_queued_run(root, run_id)? {
        store.append(&FlowEvent::new(
            EventType::StateChanged,
            run_id,
            json!(StateChangedPayload {
                from: RunState::Queued,
                to: RunState::Cancelled,
            }),
        ))?;
        store.append(&FlowEvent::new(
            EventType::RunFinished,
            run_id,
            json!({ "status": "CANCELLED", "source": "cli", "queued": true }),
        ))?;
        daemon::clear_cancel(root, run_id)?;
        println!("queued run cancelled: {run_id}");
        return Ok(());
    }

    store.append(&FlowEvent::new(
        EventType::RunFinished,
        run_id,
        json!({ "status": "CANCELLED", "source": "cli" }),
    ))?;
    println!("cancel requested: {run_id}");
    Ok(())
}

fn run_step_command(root: &Path, command: StepCommand) -> Result<()> {
    let (run_id, step_id, action) = match command {
        StepCommand::Retry { run_id, step_id } => (run_id, step_id, "retry"),
        StepCommand::Restart { run_id, step_id } => (run_id, step_id, "restart"),
        StepCommand::Reset { run_id, step_id } => (run_id, step_id, "reset"),
        StepCommand::Skip { run_id, step_id } => (run_id, step_id, "skip"),
        StepCommand::RerunFrom { run_id, step_id } => (run_id, step_id, "rerun-from"),
    };
    EventStore::new(root).append(&FlowEvent::new(
        EventType::ManualRestart,
        run_id,
        json!({ "step_id": step_id, "action": action }),
    ))?;
    println!("step action recorded: {action}");
    Ok(())
}

async fn run_plugin_command(root: &Path, command: PluginCommand) -> Result<()> {
    match command {
        PluginCommand::Validate { manifest } => {
            parse_manifest(&fs::read_to_string(&manifest)?)?;
            println!("valid: {}", manifest.display());
            Ok(())
        }
        PluginCommand::Inspect { manifest } => {
            let manifest = parse_manifest(&fs::read_to_string(&manifest)?)?;
            println!("{}", serde_json::to_string_pretty(&manifest)?);
            Ok(())
        }
        PluginCommand::Test { command, sample } => {
            let source = fs::read_to_string(sample)?;
            let output = match PluginRuntime::parse_output(&source) {
                Ok(output) => output,
                Err(_) => plugin_test(&command, root, &source).await?,
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
            Ok(())
        }
        PluginCommand::List => {
            let plugins = root.join(".flow").join("plugins");
            if plugins.exists() {
                for entry in fs::read_dir(plugins)? {
                    println!("{}", entry?.file_name().to_string_lossy());
                }
            }
            Ok(())
        }
        PluginCommand::Init { language, path } => {
            fs::create_dir_all(path.join("plugin").join("src"))?;
            fs::create_dir_all(path.join("plugin").join("tests"))?;
            fs::write(
                path.join("plugin").join("manifest.json"),
                plugin_manifest(language)?,
            )?;
            fs::write(path.join("plugin").join("README.md"), "# RunFlow plugin\n")?;
            println!("plugin scaffold created: {}", path.display());
            Ok(())
        }
    }
}

async fn plugin_test(
    command: &str,
    root: &Path,
    sample: &str,
) -> Result<crate::plugins::PluginOutput> {
    let mut input = serde_json::from_str::<PluginInput>(sample)
        .context("plugin test sample must be a plugin input JSON or plugin output JSON")?;
    let test_id = Uuid::new_v4();
    let workspace = root
        .join(".flow")
        .join("plugin-tests")
        .join(test_id.to_string())
        .join("workspace");
    fs::create_dir_all(&workspace).with_context(|| {
        format!(
            "failed to create plugin test workspace {}",
            workspace.display()
        )
    })?;
    input.flow.run_id = test_id;
    input.paths.work_dir = workspace.display().to_string();
    PluginRuntime::run(command, &workspace, &input).await
}

fn run_package_command(root: &Path, command: PackageCommand) -> Result<()> {
    match command {
        PackageCommand::Build { workflow } => {
            let package_data = build_package_from_workflow_file(&workflow)?;
            let package_dir = root.join(".flow").join("packages");
            let package = package_dir.join(format!("{}.flowpkg", package_data.job_id));
            write_package(&package_data, &package)?;
            println!("{}", package.display());
            Ok(())
        }
        PackageCommand::Install { package } => {
            let (package_data, legacy) = read_package_or_legacy_workflow(&package)?;
            fs::create_dir_all(jobs_dir(root))?;
            fs::write(job_path(root, &package_data.job_id), package_data.workflow)?;
            if legacy {
                println!("legacy package installed: {}", package_data.job_id);
            } else {
                println!("package installed: {}", package_data.job_id);
            }
            Ok(())
        }
    }
}

fn run_test_command(root: &Path, job_id: &str, verbose: bool) -> Result<()> {
    let source = read_job_source(root, job_id)?;
    let workflow = WorkflowDefinition::from_yaml(&source)?;
    WorkflowGraph::build(&workflow)?;
    if verbose {
        println!("steps: {}", workflow.steps.len());
    }
    println!("test passed: {job_id}");
    Ok(())
}

async fn run_daemon_command(root: &Path, command: DaemonCommand) -> Result<()> {
    match command {
        DaemonCommand::Start => {
            let pid = daemon::start_daemon(root)?;
            println!("daemon starting: {pid}");
            Ok(())
        }
        DaemonCommand::Status => {
            match daemon::read_status(root)? {
                Some(status) if daemon::is_daemon_running(root) => {
                    println!("{}", serde_json::to_string_pretty(&status)?);
                }
                Some(status) => {
                    println!("{}", serde_json::to_string_pretty(&status)?);
                }
                None => println!("daemon stopped"),
            }
            Ok(())
        }
        DaemonCommand::Stop => {
            daemon::request_stop(root)?;
            println!("daemon stop requested");
            Ok(())
        }
        DaemonCommand::Restart => {
            daemon::request_stop(root).ok();
            wait_daemon_stopped(root, Duration::from_secs(10)).await?;
            let pid = daemon::start_daemon(root)?;
            println!("daemon restarted: {pid}");
            Ok(())
        }
        DaemonCommand::Serve => serve_daemon(root).await,
    }
}

async fn serve_daemon(root: &Path) -> Result<()> {
    let started_at = Utc::now();
    write_daemon_status(root, started_at, DaemonState::Starting, None)?;

    loop {
        if daemon::stop_requested(root) {
            write_daemon_status(root, started_at, DaemonState::Stopping, None)?;
            break;
        }

        match daemon::next_run_request(root)? {
            Some(request) => {
                daemon::pop_run_request(root, request.run_id)?;
                if daemon::cancel_requested(root, request.run_id) {
                    finish_queued_cancel(root, request.run_id)?;
                    continue;
                }
                write_daemon_status(root, started_at, DaemonState::Running, Some(request.run_id))?;
                let _ = run_job_direct(
                    root,
                    &request.job_id,
                    request.run_id,
                    RunState::Queued,
                    false,
                    true,
                )
                .await;
                write_daemon_status(root, started_at, DaemonState::Idle, None)?;
            }
            None => {
                write_daemon_status(root, started_at, DaemonState::Idle, None)?;
                sleep(Duration::from_millis(250)).await;
            }
        }
    }

    daemon::clear_daemon_files(root)
}

fn write_daemon_status(
    root: &Path,
    started_at: chrono::DateTime<Utc>,
    state: DaemonState,
    active_run: Option<Uuid>,
) -> Result<()> {
    daemon::write_status(
        root,
        &DaemonStatus {
            pid: std::process::id(),
            started_at,
            heartbeat_at: Utc::now(),
            state,
            active_run,
            queued_runs: daemon::queued_runs(root)?,
        },
    )
}

async fn wait_daemon_stopped(root: &Path, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match daemon::read_status(root)? {
            Some(status) if status.state == DaemonState::Stopped => return Ok(()),
            None => return Ok(()),
            _ => sleep(Duration::from_millis(250)).await,
        }
    }
    anyhow::bail!("daemon did not stop before restart timeout")
}

fn finish_queued_cancel(root: &Path, run_id: Uuid) -> Result<()> {
    let store = EventStore::new(root);
    store.append(&FlowEvent::new(
        EventType::StateChanged,
        run_id,
        json!(StateChangedPayload {
            from: RunState::Queued,
            to: RunState::Cancelled,
        }),
    ))?;
    store.append(&FlowEvent::new(
        EventType::RunFinished,
        run_id,
        json!({ "status": "CANCELLED", "source": "daemon", "queued": true }),
    ))?;
    daemon::clear_cancel(root, run_id)
}

fn run_retention_command(root: &Path, command: RetentionCommand) -> Result<()> {
    match command {
        RetentionCommand::Run {
            keep_runs,
            older_than_days,
            dry_run,
        } => {
            let report = run_retention(
                root,
                RetentionPolicy {
                    keep_runs,
                    older_than_days,
                    dry_run,
                },
            )?;
            println!(
                "retention scanned={} removed_runs={} removed_files={} dry_run={}",
                report.scanned_runs, report.removed_runs, report.removed_files, report.dry_run
            );
            Ok(())
        }
    }
}

fn validate_workflow(workflow: &Path) -> Result<()> {
    let diagnostics = schemas::validate_workflow_file(workflow)?;
    if diagnostics.is_empty() {
        println!("valid: {}", workflow.display());
        Ok(())
    } else {
        eprintln!("{}", serde_json::to_string_pretty(&diagnostics)?);
        bail!("invalid workflow: {}", workflow.display());
    }
}

fn list_job_ids(root: &Path) -> Result<Vec<String>> {
    list_file_stems(&jobs_dir(root), "yml")
}

fn list_run_ids(root: &Path) -> Result<Vec<String>> {
    let runs = root.join(".flow").join("runs");
    if !runs.exists() {
        return Ok(Vec::new());
    }
    let mut ids = Vec::new();
    for entry in fs::read_dir(runs)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            ids.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    ids.sort();
    Ok(ids)
}

fn list_file_stems(dir: &Path, extension: &str) -> Result<Vec<String>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut ids = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) == Some(extension)
            && let Some(stem) = path.file_stem().and_then(|value| value.to_str())
        {
            ids.push(stem.to_owned());
        }
    }
    ids.sort();
    Ok(ids)
}

fn read_job_source(root: &Path, job_id: &str) -> Result<String> {
    fs::read_to_string(job_path(root, job_id)).with_context(|| format!("job not found: {job_id}"))
}

fn jobs_dir(root: &Path) -> PathBuf {
    root.join(".flow").join("jobs")
}

fn job_path(root: &Path, job_id: &str) -> PathBuf {
    jobs_dir(root).join(format!("{job_id}.yml"))
}

fn plugin_manifest(language: PluginLanguage) -> Result<String> {
    let language_id = match language {
        PluginLanguage::Rust => "rust",
        PluginLanguage::Java => "java",
        PluginLanguage::Python => "python",
        PluginLanguage::Node => "node",
    };
    let manifest = PluginManifest {
        id: format!("runflow-{language_id}-plugin"),
        version: "0.1.0".to_owned(),
        contract_version: "v1".to_owned(),
        author: "TODO".to_owned(),
        description: "TODO".to_owned(),
    };
    serde_json::to_string_pretty(&manifest).context("failed to serialize plugin manifest")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_top_level_commands() {
        for args in [
            vec!["flow", "job", "list"],
            vec!["flow", "run", "list"],
            vec![
                "flow",
                "step",
                "skip",
                "00000000-0000-0000-0000-000000000000",
                "s1",
            ],
            vec!["flow", "plugin", "list"],
            vec!["flow", "package", "build", "workflow.yml"],
            vec!["flow", "test", "demo"],
            vec!["flow", "validate", "workflow.yml"],
            vec!["flow", "migrate", "workflow.yml"],
            vec!["flow", "daemon", "status"],
            vec!["flow", "retention", "run"],
            vec!["flow", "version"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok());
        }
    }

    #[tokio::test]
    async fn job_add_and_run_command_workflow() {
        let root = std::env::temp_dir().join(format!("runflow-cli-{}", Uuid::new_v4()));
        let workflow = root.join("workflow.yml");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            &workflow,
            r#"
id: hello
version: 1
schema_version: 1
steps:
  - id: echo
    type: command
    run: echo hello
"#,
        )
        .unwrap();

        Cli::try_parse_from([
            "flow",
            "--root",
            root.to_str().unwrap(),
            "job",
            "add",
            workflow.to_str().unwrap(),
        ])
        .unwrap()
        .run()
        .await
        .unwrap();
        Cli::try_parse_from([
            "flow",
            "--root",
            root.to_str().unwrap(),
            "job",
            "run",
            "hello",
        ])
        .unwrap()
        .run()
        .await
        .unwrap();

        assert_eq!(list_job_ids(&root).unwrap(), vec!["hello".to_owned()]);
        let runs = list_run_ids(&root).unwrap();
        assert_eq!(runs.len(), 1);
        assert!(
            root.join(".flow")
                .join("runs")
                .join(&runs[0])
                .join("manifest.json")
                .exists()
        );

        fs::remove_dir_all(root).ok();
    }
}
