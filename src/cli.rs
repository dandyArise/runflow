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
use crate::logs;
use crate::manifest::write_run_manifest;
use crate::packages::{
    build_package_from_workflow_file, read_package_or_legacy_workflow, write_package,
};
use crate::plugins::{PluginInput, PluginManifest, PluginRuntime, parse_manifest};
use crate::retention::{RetentionPolicy, run_retention};
use crate::schemas;
use crate::state::{RunState, StateChangedPayload};
use crate::supervisor::{CommandLimits, ManagedProcessLogs, ProcessSupervisor};
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
        job_name: String,
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
    /// Add a job (upsert: creates or replaces if already exists)
    Add {
        workflow: PathBuf,
    },
    /// Update an existing job (error if not found)
    Update {
        workflow: PathBuf,
    },
    /// Delete a job by id
    Delete {
        job_name: String,
    },
    /// Delete all jobs
    Clear,
    List,
    Show {
        job_name: String,
    },
    Run {
        job_name: String,
    },
}

#[derive(Debug, Subcommand)]
enum RunCommand {
    List,
    Show {
        run_id: Uuid,
    },
    Logs {
        run_id: Uuid,
    },
    Summary {
        run_id: Uuid,
    },
    Output {
        run_id: Uuid,
        step_name: String,
        #[arg(long)]
        stderr: bool,
    },
    Cancel {
        run_id: Uuid,
    },
}

#[derive(Debug, Subcommand)]
enum StepCommand {
    Retry { run_id: Uuid, step_name: String },
    Restart { run_id: Uuid, step_name: String },
    Reset { run_id: Uuid, step_name: String },
    Skip { run_id: Uuid, step_name: String },
    RerunFrom { run_id: Uuid, step_name: String },
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
            Command::Test { job_name, verbose } => run_test_command(&root, &job_name, verbose),
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
            let path = job_path(root, &definition.name);
            let exists = path.exists();
            fs::write(&path, source)
                .with_context(|| format!("failed to store job {}", definition.name))?;
            if exists {
                println!("job updated: {}", definition.name);
            } else {
                println!("job added: {}", definition.name);
            }
            Ok(())
        }
        JobCommand::Update { workflow } => {
            let source = fs::read_to_string(&workflow)
                .with_context(|| format!("failed to read {}", workflow.display()))?;
            let definition = WorkflowDefinition::from_yaml(&source)?;
            let path = job_path(root, &definition.name);
            if !path.exists() {
                bail!("job not found: {}", definition.name);
            }
            fs::write(&path, source)
                .with_context(|| format!("failed to update job {}", definition.name))?;
            println!("job updated: {}", definition.name);
            Ok(())
        }
        JobCommand::Delete { job_name } => {
            let path = job_path(root, &job_name);
            if !path.exists() {
                bail!("job not found: {job_name}");
            }
            fs::remove_file(&path).with_context(|| format!("failed to delete job {job_name}"))?;
            println!("job deleted: {job_name}");
            Ok(())
        }
        JobCommand::Clear => {
            let job_names = list_job_names(root)?;
            let count = job_names.len();
            for job_name in &job_names {
                fs::remove_file(job_path(root, job_name))
                    .with_context(|| format!("failed to delete job {job_name}"))?;
            }
            println!("jobs cleared: {count}");
            Ok(())
        }
        JobCommand::List => {
            for job_name in list_job_names(root)? {
                println!("{job_name}");
            }
            Ok(())
        }
        JobCommand::Show { job_name } => {
            print!("{}", read_job_source(root, &job_name)?);
            Ok(())
        }
        JobCommand::Run { job_name } => run_job(root, &job_name).await,
    }
}

async fn run_job(root: &Path, job_name: &str) -> Result<()> {
    if daemon::is_daemon_running(root) {
        let run_id = enqueue_job_run(root, job_name)?;
        println!("{run_id}");
        return Ok(());
    }

    let run_id = Uuid::new_v4();
    run_job_direct(root, job_name, run_id, RunState::Created, true, false).await?;
    println!("{run_id}");
    Ok(())
}

fn enqueue_job_run(root: &Path, job_name: &str) -> Result<Uuid> {
    read_job_source(root, job_name)?;
    let run_id = Uuid::new_v4();
    let event_store = EventStore::new(root);
    event_store.append(&FlowEvent::new(
        EventType::RunCreated,
        run_id,
        json!({ "job_name": job_name, "source": "daemon_queue" }),
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
            job_name: job_name.to_owned(),
            enqueued_at: Utc::now(),
        },
    )?;
    Ok(run_id)
}

async fn run_job_direct(
    root: &Path,
    job_name: &str,
    run_id: Uuid,
    from_state: RunState,
    create_event: bool,
    daemon_mode: bool,
) -> Result<RunState> {
    let source = read_job_source(root, job_name)?;
    let workflow = WorkflowDefinition::from_yaml(&source)?;
    let graph = WorkflowGraph::build(&workflow)?;
    let started_at = Utc::now();
    let event_store = EventStore::new(root);
    let workspace = WorkspaceIsolation::new(root).create(run_id)?;
    let mut workflow_log = logs::write_workflow_started(root, run_id, &workflow, started_at)?;

    if create_event {
        event_store.append(&FlowEvent::new(
            EventType::RunCreated,
            run_id,
            json!({ "job_name": job_name }),
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
        json!({ "job_name": job_name }),
    ))?;

    let mut status = RunState::Success;
    let mut failed_step_count = 0;
    let mut successful_step_count = 0;
    let ordered = graph.ordered_steps()?;
    for step_name in ordered {
        if daemon_mode && daemon::cancel_requested(root, run_id) {
            status = RunState::Cancelled;
            break;
        }
        let step = workflow
            .steps
            .iter()
            .find(|step| step.name == step_name)
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
                    json!({ "step_name": step.name }),
                ))?;
                successful_step_count += 1;
            }
            Err(error) => {
                if daemon_mode && daemon::cancel_requested(root, run_id) {
                    status = RunState::Cancelled;
                    break;
                }
                event_store.append(&FlowEvent::new(
                    EventType::StepFailed,
                    run_id,
                    json!({ "step_name": step.name, "error": error.to_string() }),
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
    workflow_log = logs::write_workflow_finished(
        root,
        run_id,
        workflow_log,
        Utc::now(),
        status,
        successful_step_count,
        failed_step_count as usize,
    )?;
    print_run_summary(root, run_id, &workflow_log, status)?;

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
        let run = step.run.as_ref().context("command step missing run")?;
        let mut prepared = logs::prepare_step_logs(
            root,
            run_id,
            &step.name,
            run,
            run.working_directory().map(str::to_owned),
            Utc::now(),
        )?;
        let stderr_path = prepared.stderr_path.clone();
        let (stdout_file, stderr_file) = prepared.take_stdio()?;
        let (process, started) = match ProcessSupervisor::spawn_managed_run(
            run,
            &workspace.work_dir,
            run_id,
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
        daemon::write_active_process(
            root,
            &ActiveProcess {
                run_id,
                step_name: step.name.clone(),
                pid: process.pid(),
                command: run.display_command(),
                started_at: Utc::now(),
            },
        )?;
        let output = process.wait_logged(CommandLimits::default()).await;
        daemon::clear_active_process(root, run_id)?;
        let output = output?;
        logs::write_step_finished(prepared, Utc::now(), output.exit_code, None)?;
        if output.exit_code != Some(0) {
            bail!(
                "command step {} failed with {:?}",
                step.name,
                output.exit_code
            );
        }
        return Ok((crate::engine::StepOutcome::Command(output), vec![started]));
    }

    WorkflowEngine::execute_step_with_events(step, workspace, RetryPolicy::default()).await
}

fn print_run_summary(
    root: &Path,
    run_id: Uuid,
    workflow_log: &logs::WorkflowLogMetadata,
    status: RunState,
) -> Result<()> {
    if status == RunState::Success {
        return Ok(());
    }
    println!("Workflow: {}", workflow_log.workflow_name);
    println!("Status: {:?}", status);
    println!("Log dir: {}", workflow_log.log_dir);

    let workflow_log_dir = logs::workflow_log_dir(root, run_id);
    let mut failed_steps = Vec::new();
    for entry in fs::read_dir(&workflow_log_dir)
        .with_context(|| format!("failed to read log dir {}", workflow_log_dir.display()))?
    {
        let path = entry?.path().join("step.metadata.json");
        if !path.exists() {
            continue;
        }
        let source = fs::read_to_string(&path)
            .with_context(|| format!("failed to read step metadata {}", path.display()))?;
        let value: serde_json::Value = serde_json::from_str(&source)
            .with_context(|| format!("failed to parse step metadata {}", path.display()))?;
        if value.get("status").and_then(|value| value.as_str()) == Some("FAILED") {
            failed_steps.push(value);
        }
    }

    if !failed_steps.is_empty() {
        println!();
        println!("Failed steps:");
        for step in failed_steps {
            let step_name = step
                .get("step_id")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown");
            let step_dir = format!("{}/{step_name}", workflow_log.log_dir);
            println!("- {step_name}");
            if let Some(command) = step.get("command").and_then(|value| value.as_str()) {
                println!("  Command: {command}");
            }
            if let Some(args) = step.get("args") {
                println!("  Args: {args}");
            }
            if let Some(exit_code) = step.get("exit_code") {
                println!("  Exit code: {exit_code}");
            }
            if let Some(spawn_error) = step.get("spawn_error").and_then(|value| value.as_str()) {
                println!("  Spawn error: {spawn_error}");
            }
            println!("  Stdout: {step_dir}/stdout.log");
            println!("  Stderr: {step_dir}/stderr.log");
        }
    }
    Ok(())
}

async fn run_run_command(root: &Path, command: RunCommand) -> Result<()> {
    match command {
        RunCommand::List => {
            let store = EventStore::new(root);
            for run_id_str in list_run_ids(root)? {
                let run_id: Uuid = run_id_str.parse()?;
                let (state, job_name) = resolve_run_state(&store, run_id);
                if let Some(name) = job_name {
                    println!("{run_id_str}  {state:<9} ({name})");
                } else {
                    println!("{run_id_str}  {state}");
                }
            }
            Ok(())
        }
        RunCommand::Show { run_id } | RunCommand::Logs { run_id } => {
            for event in EventStore::new(root).read_run(run_id)? {
                println!("{}", serde_json::to_string(&event)?);
            }
            Ok(())
        }
        RunCommand::Summary { run_id } => print_saved_run_summary(root, run_id),
        RunCommand::Output {
            run_id,
            step_name,
            stderr,
        } => print_step_output(root, run_id, &step_name, stderr),
        RunCommand::Cancel { run_id } => {
            cancel_run(root, run_id).await?;
            Ok(())
        }
    }
}

fn print_saved_run_summary(root: &Path, run_id: Uuid) -> Result<()> {
    let path = logs::workflow_log_dir(root, run_id).join("workflow.metadata.json");
    let source = fs::read_to_string(&path)
        .with_context(|| format!("run summary not found: {}", path.display()))?;
    let value: serde_json::Value =
        serde_json::from_str(&source).context("failed to parse workflow metadata")?;
    let workflow = value
        .get("workflow_name")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    let status = value
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or("UNKNOWN");
    let log_dir = value
        .get("log_dir")
        .and_then(|value| value.as_str())
        .unwrap_or("logs");
    println!("Workflow: {workflow}");
    println!("Status: {status}");
    println!("Log dir: {log_dir}");
    println!(
        "Steps: total={} success={} failed={}",
        json_number(&value, "total_steps"),
        json_number(&value, "successful_steps"),
        json_number(&value, "failed_steps")
    );
    Ok(())
}

fn print_step_output(root: &Path, run_id: Uuid, step_name: &str, stderr: bool) -> Result<()> {
    let file_name = if stderr { "stderr.log" } else { "stdout.log" };
    let path = logs::workflow_log_dir(root, run_id)
        .join(step_name)
        .join(file_name);
    let source =
        fs::read(&path).with_context(|| format!("step output not found: {}", path.display()))?;
    print!("{}", String::from_utf8_lossy(&source));
    Ok(())
}

fn json_number(value: &serde_json::Value, key: &str) -> u64 {
    value.get(key).and_then(|value| value.as_u64()).unwrap_or(0)
}

async fn cancel_run(root: &Path, run_id: Uuid) -> Result<()> {
    let store = EventStore::new(root);

    // Guard: refuse to cancel a run that already reached a terminal state.
    let (current_state, _) = resolve_run_state(&store, run_id);
    match current_state.as_str() {
        "SUCCESS" | "FAILED" | "CANCELLED" | "TIMEOUT" | "SKIPPED" => {
            bail!("run is already {current_state}: {run_id}");
        }
        _ => {}
    }

    daemon::request_cancel(root, run_id)?;

    if let Some(active) = daemon::read_active_process(root, run_id)? {
        for event in ProcessSupervisor::kill_process_tree_events(
            run_id,
            Some(active.step_name),
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

/// Replay the StateChanged events for a run and return its current state label and job_name.
/// Returns ("UNKNOWN", None) if no state events are found (e.g. no events file yet).
fn resolve_run_state(store: &EventStore, run_id: Uuid) -> (String, Option<String>) {
    use crate::events::EventType;
    let events = match store.read_run(run_id) {
        Ok(events) => events,
        Err(_) => return ("UNKNOWN".to_owned(), None),
    };
    let mut state = "UNKNOWN".to_owned();
    let mut job_name = None;
    for event in &events {
        if event.event_type == EventType::StateChanged {
            if let Some(to) = event.payload.get("to").and_then(|v| v.as_str()) {
                state = to.to_owned();
            }
        } else if event.event_type == EventType::RunCreated
            && let Some(name) = event.payload.get("job_name").and_then(|v| v.as_str())
        {
            job_name = Some(name.to_owned());
        }
    }
    (state, job_name)
}

fn run_step_command(root: &Path, command: StepCommand) -> Result<()> {
    let (run_id, step_name, action) = match command {
        StepCommand::Retry { run_id, step_name } => (run_id, step_name, "retry"),
        StepCommand::Restart { run_id, step_name } => (run_id, step_name, "restart"),
        StepCommand::Reset { run_id, step_name } => (run_id, step_name, "reset"),
        StepCommand::Skip { run_id, step_name } => (run_id, step_name, "skip"),
        StepCommand::RerunFrom { run_id, step_name } => (run_id, step_name, "rerun-from"),
    };
    EventStore::new(root).append(&FlowEvent::new(
        EventType::ManualRestart,
        run_id,
        json!({ "step_name": step_name, "action": action }),
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
            let package = package_dir.join(format!("{}.flowpkg", package_data.job_name));
            write_package(&package_data, &package)?;
            println!("{}", package.display());
            Ok(())
        }
        PackageCommand::Install { package } => {
            let (package_data, legacy) = read_package_or_legacy_workflow(&package)?;
            fs::create_dir_all(jobs_dir(root))?;
            fs::write(
                job_path(root, &package_data.job_name),
                package_data.workflow,
            )?;
            if legacy {
                println!("legacy package installed: {}", package_data.job_name);
            } else {
                println!("package installed: {}", package_data.job_name);
            }
            Ok(())
        }
    }
}

fn run_test_command(root: &Path, job_name: &str, verbose: bool) -> Result<()> {
    let source = read_job_source(root, job_name)?;
    let workflow = WorkflowDefinition::from_yaml(&source)?;
    WorkflowGraph::build(&workflow)?;
    if verbose {
        println!("steps: {}", workflow.steps.len());
    }
    println!("test passed: {job_name}");
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
                    &request.job_name,
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

fn list_job_names(root: &Path) -> Result<Vec<String>> {
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

fn read_job_source(root: &Path, job_name: &str) -> Result<String> {
    fs::read_to_string(job_path(root, job_name))
        .with_context(|| format!("job not found: {job_name}"))
}

fn jobs_dir(root: &Path) -> PathBuf {
    root.join(".flow").join("jobs")
}

fn job_path(root: &Path, job_name: &str) -> PathBuf {
    jobs_dir(root).join(format!("{job_name}.yml"))
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
            vec!["flow", "job", "add", "workflow.yml"],
            vec!["flow", "job", "update", "workflow.yml"],
            vec!["flow", "job", "delete", "ping-demo"],
            vec!["flow", "job", "clear"],
            vec!["flow", "run", "list"],
            vec![
                "flow",
                "run",
                "summary",
                "00000000-0000-0000-0000-000000000000",
            ],
            vec![
                "flow",
                "run",
                "output",
                "00000000-0000-0000-0000-000000000000",
                "step",
            ],
            vec![
                "flow",
                "run",
                "cancel",
                "00000000-0000-0000-0000-000000000000",
            ],
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
name: hello
version: 1
schema_version: 1
steps:
  - name: echo
    type: command
    run:
      command: echo
      args: ["hello"]
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

        assert_eq!(list_job_names(&root).unwrap(), vec!["hello".to_owned()]);
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
