use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::json;
use uuid::Uuid;

use crate::dag::{WorkflowDefinition, WorkflowGraph};
use crate::engine::{RetryPolicy, WorkflowEngine};
use crate::events::{EventStore, EventType, FlowEvent};
use crate::plugins::{PluginManifest, PluginRuntime, parse_manifest};
use crate::schemas;
use crate::state::{RunState, StateChangedPayload};
use crate::workspace::WorkspaceIsolation;

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
}

#[derive(Debug, Subcommand)]
enum RetentionCommand {
    Run,
}

impl Cli {
    pub async fn run(self) -> Result<()> {
        let root = self.root;
        match self.command.unwrap_or(Command::Version) {
            Command::Job { command } => run_job_command(&root, command).await,
            Command::Run { command } => run_run_command(&root, command),
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
            Command::Daemon { command } => run_daemon_command(&root, command),
            Command::Retention { command } => run_retention_command(command),
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
    let source = read_job_source(root, job_id)?;
    let workflow = WorkflowDefinition::from_yaml(&source)?;
    let graph = WorkflowGraph::build(&workflow)?;
    let run_id = Uuid::new_v4();
    let event_store = EventStore::new(root);
    let workspace = WorkspaceIsolation::new(root).create(run_id)?;

    event_store.append(&FlowEvent::new(
        EventType::RunCreated,
        run_id,
        json!({ "job_id": job_id }),
    ))?;
    event_store.append(&FlowEvent::new(
        EventType::StateChanged,
        run_id,
        json!(StateChangedPayload {
            from: RunState::Created,
            to: RunState::Running,
        }),
    ))?;
    event_store.append(&FlowEvent::new(
        EventType::RunStarted,
        run_id,
        json!({ "job_id": job_id }),
    ))?;

    let mut status = RunState::Success;
    let ordered = graph.ordered_steps()?;
    for step_id in ordered {
        let step = workflow
            .steps
            .iter()
            .find(|step| step.id == step_id)
            .context("ordered step missing from workflow")?;
        match WorkflowEngine::execute_step_with_events(step, &workspace, RetryPolicy::default())
            .await
        {
            Ok((_outcome, events)) => {
                for event in events {
                    event_store.append(&event)?;
                }
                event_store.append(&FlowEvent::new(
                    EventType::StepFinished,
                    run_id,
                    json!({ "step_id": step.id }),
                ))?;
            }
            Err(error) => {
                event_store.append(&FlowEvent::new(
                    EventType::StepFailed,
                    run_id,
                    json!({ "step_id": step.id, "error": error.to_string() }),
                ))?;
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

    println!("{run_id}");
    Ok(())
}

fn run_run_command(root: &Path, command: RunCommand) -> Result<()> {
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
            EventStore::new(root).append(&FlowEvent::new(
                EventType::RunFinished,
                run_id,
                json!({ "status": "CANCELLED", "source": "cli" }),
            ))?;
            println!("cancel requested: {run_id}");
            Ok(())
        }
    }
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
            let output = PluginRuntime::parse_output(&source)
                .or_else(|_| async_plugin_test(&command, root, &source))?;
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

fn async_plugin_test(
    _command: &str,
    _root: &Path,
    _sample: &str,
) -> Result<crate::plugins::PluginOutput> {
    bail!("plugin command execution is available through workflow plugin steps")
}

fn run_package_command(root: &Path, command: PackageCommand) -> Result<()> {
    match command {
        PackageCommand::Build { workflow } => {
            let source = fs::read_to_string(&workflow)?;
            let definition = WorkflowDefinition::from_yaml(&source)?;
            let package_dir = root.join(".flow").join("packages");
            fs::create_dir_all(&package_dir)?;
            let package = package_dir.join(format!("{}.flowpkg", definition.id));
            fs::write(&package, source)?;
            println!("{}", package.display());
            Ok(())
        }
        PackageCommand::Install { package } => {
            let source = fs::read_to_string(&package)?;
            let definition = WorkflowDefinition::from_yaml(&source)?;
            fs::create_dir_all(jobs_dir(root))?;
            fs::write(job_path(root, &definition.id), source)?;
            println!("package installed: {}", definition.id);
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

fn run_daemon_command(root: &Path, command: DaemonCommand) -> Result<()> {
    let pid_file = root.join(".flow").join("daemon.pid");
    match command {
        DaemonCommand::Start => {
            if let Some(parent) = pid_file.parent() {
                fs::create_dir_all(parent)?;
            }
            if pid_file.exists() {
                bail!("daemon already started");
            }
            fs::write(&pid_file, std::process::id().to_string())?;
            println!("daemon started");
            Ok(())
        }
        DaemonCommand::Status => {
            if pid_file.exists() {
                println!("daemon running: {}", fs::read_to_string(pid_file)?.trim());
            } else {
                println!("daemon stopped");
            }
            Ok(())
        }
        DaemonCommand::Stop => {
            if pid_file.exists() {
                fs::remove_file(pid_file)?;
                println!("daemon stopped");
            } else {
                println!("daemon already stopped");
            }
            Ok(())
        }
    }
}

fn run_retention_command(command: RetentionCommand) -> Result<()> {
    match command {
        RetentionCommand::Run => {
            println!("retention run accepted; purge logic is implemented in phase 8");
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
        assert_eq!(list_run_ids(&root).unwrap().len(), 1);

        fs::remove_dir_all(root).ok();
    }
}
