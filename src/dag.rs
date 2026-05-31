use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use petgraph::algo::toposort;
use petgraph::graph::{DiGraph, NodeIndex};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailurePolicy {
    #[default]
    Stop,
    Continue,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
pub struct WorkflowDefinition {
    pub name: String,
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub schedule: Option<ScheduleDefinition>,
    #[serde(default)]
    pub failure_policy: FailurePolicy,
    pub concurrency: Option<ConcurrencyDefinition>,
    #[serde(default)]
    pub steps: Vec<StepDefinition>,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
pub struct ConcurrencyDefinition {
    pub policy: crate::scheduler::ConcurrencyPolicy,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum ScheduleDefinition {
    Cron(String),
    Detailed(ScheduleConfig),
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
pub struct ScheduleConfig {
    pub cron: String,
    #[serde(default = "default_schedule_timezone")]
    pub timezone: String,
    #[serde(default = "default_schedule_enabled")]
    pub enabled: bool,
}

impl ScheduleDefinition {
    pub fn cron(&self) -> &str {
        match self {
            Self::Cron(cron) => cron,
            Self::Detailed(config) => &config.cron,
        }
    }

    pub fn timezone(&self) -> &str {
        match self {
            Self::Cron(_) => "UTC",
            Self::Detailed(config) => &config.timezone,
        }
    }

    pub fn enabled(&self) -> bool {
        match self {
            Self::Cron(_) => true,
            Self::Detailed(config) => config.enabled,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
pub struct StepDefinition {
    pub name: String,
    #[serde(rename = "type")]
    pub step_type: StepType,
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub run: Option<RunDefinition>,
    pub plugin_id: Option<String>,
    pub duration: Option<String>,
    pub command: Option<String>,
    pub interval: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RunDefinition {
    LegacyShell(String),
    Command(RunCommandDefinition),
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunCommandDefinition {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub working_directory: Option<String>,
}

impl RunDefinition {
    pub fn command(&self) -> &str {
        match self {
            Self::LegacyShell(command) => command,
            Self::Command(command) => &command.command,
        }
    }

    pub fn args(&self) -> &[String] {
        match self {
            Self::LegacyShell(_) => &[],
            Self::Command(command) => &command.args,
        }
    }

    pub fn working_directory(&self) -> Option<&str> {
        match self {
            Self::LegacyShell(_) => None,
            Self::Command(command) => command.working_directory.as_deref(),
        }
    }

    pub fn is_legacy_shell(&self) -> bool {
        matches!(self, Self::LegacyShell(_))
    }

    pub fn display_command(&self) -> String {
        match self {
            Self::LegacyShell(command) => command.clone(),
            Self::Command(command) if command.args.is_empty() => command.command.clone(),
            Self::Command(command) => {
                let args = command.args.join(" ");
                format!("{} {args}", command.command)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepType {
    Command,
    Plugin,
    Sleep,
    WaitUntil,
}

#[derive(Debug)]
pub struct WorkflowGraph {
    graph: DiGraph<String, ()>,
    nodes: HashMap<String, NodeIndex>,
}

impl WorkflowDefinition {
    pub fn from_yaml(source: &str) -> Result<Self> {
        crate::schemas::validate_workflow_yaml(source).and_then(|diagnostics| {
            if diagnostics.is_empty() {
                Ok(())
            } else {
                let messages = diagnostics
                    .into_iter()
                    .map(|diagnostic| format!("{}: {}", diagnostic.path, diagnostic.message))
                    .collect::<Vec<_>>()
                    .join("; ");
                bail!("invalid workflow schema: {messages}");
            }
        })?;

        let workflow: Self =
            serde_yaml::from_str(source).context("failed to parse workflow YAML")?;
        if let Some(schedule) = &workflow.schedule {
            crate::scheduler::Scheduler::parse(schedule.cron())?;
        }

        Ok(workflow)
    }
}

fn default_version() -> u32 {
    1
}

fn default_schema_version() -> u32 {
    1
}

fn default_schedule_timezone() -> String {
    "UTC".to_owned()
}

fn default_schedule_enabled() -> bool {
    true
}

impl WorkflowGraph {
    pub fn build(workflow: &WorkflowDefinition) -> Result<Self> {
        let mut graph = DiGraph::<String, ()>::new();
        let mut nodes = HashMap::new();

        for step in &workflow.steps {
            if nodes.contains_key(&step.name) {
                bail!("duplicate step name: {}", step.name);
            }
            let index = graph.add_node(step.name.clone());
            nodes.insert(step.name.clone(), index);
        }

        for step in &workflow.steps {
            let step_index = nodes
                .get(&step.name)
                .copied()
                .context("step node missing after graph initialization")?;
            for dependency in &step.depends_on {
                let dependency_index = nodes.get(dependency).copied().with_context(|| {
                    format!("unknown dependency {dependency} for step {}", step.name)
                })?;
                graph.add_edge(dependency_index, step_index, ());
            }
        }

        toposort(&graph, None).map_err(|cycle| {
            let step = &graph[cycle.node_id()];
            anyhow::anyhow!("workflow dependency cycle detected near step {step}")
        })?;

        Ok(Self { graph, nodes })
    }

    pub fn ordered_steps(&self) -> Result<Vec<String>> {
        let order = toposort(&self.graph, None).map_err(|cycle| {
            let step = &self.graph[cycle.node_id()];
            anyhow::anyhow!("workflow dependency cycle detected near step {step}")
        })?;
        Ok(order
            .into_iter()
            .map(|index| self.graph[index].clone())
            .collect())
    }

    pub fn step_count(&self) -> usize {
        self.nodes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_yaml_and_orders_dependencies() {
        let workflow = WorkflowDefinition::from_yaml(
            r#"
name: backup-db
version: 1
schema_version: 1
schedule: "0 */5 * * * * *"
failure_policy: continue
concurrency:
  policy: queue
steps:
  - name: dump
    type: command
    run:
      command: pg_dump
      args: ["app"]
  - name: compress
    type: command
    depends_on: [dump]
    run:
      command: gzip
      args: ["backup.sql"]
"#,
        )
        .unwrap();

        let graph = WorkflowGraph::build(&workflow).unwrap();
        let ordered = graph.ordered_steps().unwrap();

        assert_eq!(workflow.failure_policy, FailurePolicy::Continue);
        let schedule = workflow.schedule.as_ref().unwrap();
        assert_eq!(schedule.cron(), "0 */5 * * * * *");
        assert_eq!(schedule.timezone(), "UTC");
        assert!(schedule.enabled());
        assert_eq!(
            workflow.concurrency.unwrap().policy,
            crate::scheduler::ConcurrencyPolicy::Queue
        );
        assert_eq!(graph.step_count(), 2);
        assert!(
            ordered.iter().position(|name| name == "dump")
                < ordered.iter().position(|name| name == "compress")
        );
    }

    #[test]
    fn parses_draft_workflow_with_name_only() {
        let workflow = WorkflowDefinition::from_yaml("name: draft-workflow\n").unwrap();

        assert_eq!(workflow.name, "draft-workflow");
        assert_eq!(workflow.version, 1);
        assert_eq!(workflow.schema_version, 1);
        assert!(workflow.steps.is_empty());
    }

    #[test]
    fn parses_detailed_schedule() {
        let workflow = WorkflowDefinition::from_yaml(
            r#"
name: scheduled-job
schedule:
  cron: "0 */10 * * * * *"
  timezone: Europe/Paris
  enabled: false
"#,
        )
        .unwrap();
        let schedule = workflow.schedule.as_ref().unwrap();

        assert_eq!(schedule.cron(), "0 */10 * * * * *");
        assert_eq!(schedule.timezone(), "Europe/Paris");
        assert!(!schedule.enabled());
    }

    #[test]
    fn detailed_schedule_defaults_to_utc_and_enabled() {
        let workflow = WorkflowDefinition::from_yaml(
            r#"
name: scheduled-job
schedule:
  cron: "0 0 * * * * *"
"#,
        )
        .unwrap();
        let schedule = workflow.schedule.as_ref().unwrap();

        assert_eq!(schedule.timezone(), "UTC");
        assert!(schedule.enabled());
    }

    #[test]
    fn parses_legacy_shell_run_for_compatibility() {
        let workflow = WorkflowDefinition::from_yaml(
            r#"
name: legacy
version: 1
schema_version: 1
steps:
  - name: echo
    type: command
    run: echo hello
"#,
        )
        .unwrap();

        assert_eq!(
            workflow.steps[0].run.as_ref().unwrap().command(),
            "echo hello"
        );
        assert!(workflow.steps[0].run.as_ref().unwrap().is_legacy_shell());
    }

    #[test]
    fn rejects_cycles() {
        let workflow = WorkflowDefinition::from_yaml(
            r#"
name: cyclic
version: 1
schema_version: 1
steps:
  - name: a
    type: command
    depends_on: [b]
    run:
      command: echo
      args: ["a"]
  - name: b
    type: command
    depends_on: [a]
    run:
      command: echo
      args: ["b"]
"#,
        )
        .unwrap();

        assert!(WorkflowGraph::build(&workflow).is_err());
    }

    #[test]
    fn rejects_invalid_cron_schedule() {
        let err = WorkflowDefinition::from_yaml(
            r#"
name: bad-schedule
version: 1
schema_version: 1
schedule: "not cron"
steps:
  - name: echo
    type: command
    run:
      command: echo
"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("invalid cron expression"));
    }
}
