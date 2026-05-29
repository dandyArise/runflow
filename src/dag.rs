use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use petgraph::algo::toposort;
use petgraph::graph::{DiGraph, NodeIndex};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailurePolicy {
    #[default]
    Stop,
    Continue,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
pub struct WorkflowDefinition {
    pub id: String,
    pub version: u32,
    pub schema_version: u32,
    pub schedule: Option<String>,
    #[serde(default)]
    pub failure_policy: FailurePolicy,
    pub concurrency: Option<ConcurrencyDefinition>,
    pub steps: Vec<StepDefinition>,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
pub struct ConcurrencyDefinition {
    pub policy: crate::scheduler::ConcurrencyPolicy,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
pub struct StepDefinition {
    pub id: String,
    #[serde(rename = "type")]
    pub step_type: StepType,
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub run: Option<String>,
    pub plugin_id: Option<String>,
    pub duration: Option<String>,
    pub command: Option<String>,
    pub interval: Option<String>,
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

        serde_yaml::from_str(source).context("failed to parse workflow YAML")
    }
}

impl WorkflowGraph {
    pub fn build(workflow: &WorkflowDefinition) -> Result<Self> {
        let mut graph = DiGraph::<String, ()>::new();
        let mut nodes = HashMap::new();

        for step in &workflow.steps {
            if nodes.contains_key(&step.id) {
                bail!("duplicate step id: {}", step.id);
            }
            let index = graph.add_node(step.id.clone());
            nodes.insert(step.id.clone(), index);
        }

        for step in &workflow.steps {
            let step_index = nodes
                .get(&step.id)
                .copied()
                .context("step node missing after graph initialization")?;
            for dependency in &step.depends_on {
                let dependency_index = nodes.get(dependency).copied().with_context(|| {
                    format!("unknown dependency {dependency} for step {}", step.id)
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
id: backup-db
version: 1
schema_version: 1
failure_policy: continue
concurrency:
  policy: queue
steps:
  - id: dump
    type: command
    run: pg_dump app > backup.sql
  - id: compress
    type: command
    depends_on: [dump]
    run: gzip backup.sql
"#,
        )
        .unwrap();

        let graph = WorkflowGraph::build(&workflow).unwrap();
        let ordered = graph.ordered_steps().unwrap();

        assert_eq!(workflow.failure_policy, FailurePolicy::Continue);
        assert_eq!(
            workflow.concurrency.unwrap().policy,
            crate::scheduler::ConcurrencyPolicy::Queue
        );
        assert_eq!(graph.step_count(), 2);
        assert!(
            ordered.iter().position(|id| id == "dump")
                < ordered.iter().position(|id| id == "compress")
        );
    }

    #[test]
    fn rejects_cycles() {
        let workflow = WorkflowDefinition::from_yaml(
            r#"
id: cyclic
version: 1
schema_version: 1
steps:
  - id: a
    type: command
    depends_on: [b]
    run: echo a
  - id: b
    type: command
    depends_on: [a]
    run: echo b
"#,
        )
        .unwrap();

        assert!(WorkflowGraph::build(&workflow).is_err());
    }
}
