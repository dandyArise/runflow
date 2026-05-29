use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::dag::WorkflowDefinition;

pub const PACKAGE_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct FlowPackage {
    pub format_version: u32,
    pub job_id: String,
    pub workflow_version: u32,
    pub schema_version: u32,
    pub workflow_checksum: String,
    pub workflow: String,
}

pub fn build_package_from_workflow_file(path: impl AsRef<Path>) -> Result<FlowPackage> {
    let source = fs::read_to_string(path.as_ref())
        .with_context(|| format!("failed to read workflow {}", path.as_ref().display()))?;
    build_package_from_workflow_source(&source)
}

pub fn build_package_from_workflow_source(source: &str) -> Result<FlowPackage> {
    let definition = WorkflowDefinition::from_yaml(source)?;
    Ok(FlowPackage {
        format_version: PACKAGE_FORMAT_VERSION,
        job_id: definition.id,
        workflow_version: definition.version,
        schema_version: definition.schema_version,
        workflow_checksum: workflow_checksum(source),
        workflow: source.to_owned(),
    })
}

pub fn write_package(package: &FlowPackage, path: impl AsRef<Path>) -> Result<()> {
    if let Some(parent) = path.as_ref().parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create package directory {}", parent.display()))?;
    }
    fs::write(path.as_ref(), serde_json::to_vec_pretty(package)?)
        .with_context(|| format!("failed to write package {}", path.as_ref().display()))
}

pub fn read_package_or_legacy_workflow(path: impl AsRef<Path>) -> Result<(FlowPackage, bool)> {
    let source = fs::read_to_string(path.as_ref())
        .with_context(|| format!("failed to read package {}", path.as_ref().display()))?;
    match serde_json::from_str::<FlowPackage>(&source) {
        Ok(package) => {
            validate_package(&package)?;
            Ok((package, false))
        }
        Err(_) => {
            let package = build_package_from_workflow_source(&source)
                .context("failed to parse package as FlowPackage or legacy workflow YAML")?;
            Ok((package, true))
        }
    }
}

pub fn validate_package(package: &FlowPackage) -> Result<()> {
    if package.format_version != PACKAGE_FORMAT_VERSION {
        bail!(
            "unsupported package format version: {}",
            package.format_version
        );
    }
    let definition = WorkflowDefinition::from_yaml(&package.workflow)?;
    if definition.id != package.job_id {
        bail!(
            "package job_id mismatch: manifest={}, workflow={}",
            package.job_id,
            definition.id
        );
    }
    if definition.version != package.workflow_version {
        bail!("package workflow_version mismatch");
    }
    if definition.schema_version != package.schema_version {
        bail!("package schema_version mismatch");
    }
    let expected = workflow_checksum(&package.workflow);
    if package.workflow_checksum != expected {
        bail!(
            "package workflow checksum mismatch: expected {}, got {}",
            expected,
            package.workflow_checksum
        );
    }
    Ok(())
}

fn workflow_checksum(source: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in source.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workflow_source() -> &'static str {
        r#"
id: package-demo
version: 1
schema_version: 1
steps:
  - id: hello
    type: command
    run: echo hello
"#
    }

    #[test]
    fn builds_structured_package_from_workflow() {
        let package = build_package_from_workflow_source(workflow_source()).unwrap();

        assert_eq!(package.format_version, PACKAGE_FORMAT_VERSION);
        assert_eq!(package.job_id, "package-demo");
        assert!(package.workflow_checksum.starts_with("fnv1a64:"));
    }

    #[test]
    fn rejects_tampered_package_checksum() {
        let mut package = build_package_from_workflow_source(workflow_source()).unwrap();
        package.workflow.push('\n');

        assert!(validate_package(&package).is_err());
    }

    #[test]
    fn reads_legacy_workflow_package() {
        let root = std::env::temp_dir().join(format!("runflow-package-{}", uuid::Uuid::new_v4()));
        let package_path = root.join("legacy.flowpkg");
        fs::create_dir_all(&root).unwrap();
        fs::write(&package_path, workflow_source()).unwrap();

        let (package, legacy) = read_package_or_legacy_workflow(&package_path).unwrap();

        assert!(legacy);
        assert_eq!(package.job_id, "package-demo");

        fs::remove_dir_all(root).ok();
    }
}
