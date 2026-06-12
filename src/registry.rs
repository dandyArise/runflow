use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RegistryDocument {
    pub schema_version: u32,
    pub generated_at: DateTime<Utc>,
    pub registry_hash: String,
    pub plugins: Vec<RegistryPlugin>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RegistryPlugin {
    pub name: String,
    pub version: String,
    pub description: String,
    pub runtime: RegistryPluginRuntime,
    pub entrypoint: String,
    pub manifest_path: String,
    pub plugin_dir: String,
    #[serde(default)]
    pub inputs: BTreeMap<String, RegistryField>,
    #[serde(default)]
    pub outputs: BTreeMap<String, RegistryField>,
    #[serde(default)]
    pub permissions: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistryPluginRuntime {
    Python,
    Node,
    Shell,
    Binary,
}

impl RegistryPluginRuntime {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Node => "node",
            Self::Shell => "shell",
            Self::Binary => "binary",
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RegistryField {
    #[serde(rename = "type")]
    pub field_type: RegistryFieldType,
    #[serde(default)]
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(rename = "enum", skip_serializing_if = "Option::is_none")]
    pub enum_values: Option<Vec<Value>>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistryFieldType {
    String,
    Integer,
    Number,
    Boolean,
    Object,
    Array,
}

impl RegistryFieldType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Integer => "integer",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::Object => "object",
            Self::Array => "array",
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RegistryScanSummary {
    pub scanned_plugins: usize,
    pub valid_plugins: usize,
    pub invalid_plugins: usize,
    pub registry_path: PathBuf,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct RegistryLintDiagnostic {
    pub code: String,
    pub message: String,
    pub field: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RegistryPluginManifest {
    name: String,
    version: String,
    description: String,
    runtime: RegistryPluginRuntime,
    entrypoint: String,
    #[serde(default)]
    inputs: BTreeMap<String, RegistryField>,
    #[serde(default)]
    outputs: BTreeMap<String, RegistryField>,
    #[serde(default)]
    permissions: BTreeMap<String, Value>,
}

#[derive(Debug, Serialize)]
pub struct AgentRegistryContext {
    schema_version: u32,
    registry_hash: String,
    generated_at: DateTime<Utc>,
    plugins: Vec<AgentPlugin>,
    rules: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct AgentPlugin {
    name: String,
    description: String,
    inputs: BTreeMap<String, RegistryFieldType>,
    outputs: BTreeMap<String, RegistryFieldType>,
    permissions: BTreeMap<String, bool>,
}

pub fn scan_and_write(root: impl AsRef<Path>) -> Result<RegistryScanSummary> {
    let root = root.as_ref();
    let plugins = scan_plugins(root)?;
    let summary = RegistryScanSummary {
        scanned_plugins: plugins.len(),
        valid_plugins: plugins.len(),
        invalid_plugins: 0,
        registry_path: registry_path(root),
    };
    write_registry(root, plugins)?;
    Ok(summary)
}

pub fn scan_check(root: impl AsRef<Path>) -> Result<RegistryScanSummary> {
    let root = root.as_ref();
    let plugins = scan_plugins(root)?;
    let expected_hash = registry_hash(&plugins)?;
    let current = read_registry(root)?;
    if current.schema_version != 1
        || current.registry_hash != expected_hash
        || current.plugins != plugins
    {
        bail!(
            "registry is out of date: run `flow registry scan` and commit {}",
            registry_path(root).display()
        );
    }
    Ok(RegistryScanSummary {
        scanned_plugins: plugins.len(),
        valid_plugins: plugins.len(),
        invalid_plugins: 0,
        registry_path: registry_path(root),
    })
}

pub fn scan_plugins(root: impl AsRef<Path>) -> Result<Vec<RegistryPlugin>> {
    let root = root.as_ref();
    let mut plugins = Vec::new();
    let mut names = HashSet::new();

    for manifest_path in manifest_paths(root)? {
        let source = fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?;
        let manifest = parse_manifest_yaml(&source)
            .with_context(|| format!("invalid plugin manifest {}", manifest_path.display()))?;
        validate_manifest(root, &manifest_path, &manifest)?;
        if !names.insert(manifest.name.clone()) {
            bail!("duplicate plugin name: {}", manifest.name);
        }
        plugins.push(to_registry_plugin(root, &manifest_path, manifest)?);
    }

    plugins.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(plugins)
}

pub fn write_registry(
    root: impl AsRef<Path>,
    plugins: Vec<RegistryPlugin>,
) -> Result<RegistryDocument> {
    let root = root.as_ref();
    let document = RegistryDocument {
        schema_version: 1,
        generated_at: Utc::now(),
        registry_hash: registry_hash(&plugins)?,
        plugins,
    };
    let path = registry_path(root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(&path, serde_json::to_vec_pretty(&document)?)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(document)
}

pub fn read_registry(root: impl AsRef<Path>) -> Result<RegistryDocument> {
    let path = registry_path(root.as_ref());
    let source = fs::read_to_string(&path)
        .with_context(|| format!("registry missing: {}", path.display()))?;
    serde_json::from_str(&source).with_context(|| format!("failed to parse {}", path.display()))
}

pub fn registry_path(root: impl AsRef<Path>) -> PathBuf {
    root.as_ref()
        .join(".flow")
        .join("registry")
        .join("plugins.json")
}

pub fn export_for_agent(root: impl AsRef<Path>) -> Result<AgentRegistryContext> {
    let registry = read_registry(root)?;
    Ok(AgentRegistryContext {
        schema_version: registry.schema_version,
        registry_hash: registry.registry_hash,
        generated_at: registry.generated_at,
        plugins: registry
            .plugins
            .into_iter()
            .map(|plugin| AgentPlugin {
                name: plugin.name,
                description: plugin.description,
                inputs: field_type_map(plugin.inputs),
                outputs: field_type_map(plugin.outputs),
                permissions: agent_permissions(plugin.permissions),
            })
            .collect(),
        rules: vec![
            "Use only registered plugins.",
            "Do not invent tools.",
            "Return needs_tool if no plugin matches.",
            "Do not execute shell directly.",
        ],
    })
}

pub fn validate_workflow_plugins(
    root: impl AsRef<Path>,
    workflow: &crate::dag::WorkflowDefinition,
) -> Vec<RegistryLintDiagnostic> {
    let plugin_steps = workflow
        .steps
        .iter()
        .enumerate()
        .filter(|(_, step)| step.step_type == crate::dag::StepType::Plugin)
        .collect::<Vec<_>>();
    if plugin_steps.is_empty() {
        return Vec::new();
    }

    let registry = match read_registry(root) {
        Ok(registry) => registry,
        Err(error) => {
            return vec![RegistryLintDiagnostic {
                code: "registry_missing".to_owned(),
                message: format!("{error:#}"),
                field: "steps".to_owned(),
                suggestion: Some(
                    "Run `flow registry scan` before validating plugin workflows.".to_owned(),
                ),
            }];
        }
    };

    let registered = registry
        .plugins
        .iter()
        .map(|plugin| (plugin.name.as_str(), plugin))
        .collect::<BTreeMap<_, _>>();
    let suggestions = registry
        .plugins
        .iter()
        .map(|plugin| plugin.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    let mut diagnostics = Vec::new();
    for (index, step) in plugin_steps {
        let Some(plugin_id) = step.plugin_id.as_deref() else {
            diagnostics.push(RegistryLintDiagnostic {
                code: "unknown_plugin".to_owned(),
                message: "Plugin step has no plugin_id.".to_owned(),
                field: format!("steps[{index}].plugin_id"),
                suggestion: if suggestions.is_empty() {
                    None
                } else {
                    Some(format!("Use one of: {suggestions}"))
                },
            });
            continue;
        };
        let Some(plugin) = registered.get(plugin_id) else {
            diagnostics.push(RegistryLintDiagnostic {
                code: "unknown_plugin".to_owned(),
                message: format!("Plugin {plugin_id} is not registered."),
                field: format!("steps[{index}].plugin_id"),
                suggestion: if suggestions.is_empty() {
                    None
                } else {
                    Some(format!("Use one of: {suggestions}"))
                },
            });
            continue;
        };

        validate_step_entrypoint(index, step, plugin, &mut diagnostics);
        validate_step_input(index, step.input.as_ref(), plugin, &mut diagnostics);
    }

    diagnostics
}

fn parse_manifest_yaml(source: &str) -> Result<RegistryPluginManifest> {
    serde_yaml::from_str(source).context("failed to parse plugin.yml")
}

fn validate_manifest(
    root: &Path,
    manifest_path: &Path,
    manifest: &RegistryPluginManifest,
) -> Result<()> {
    if manifest.name.trim().is_empty() {
        bail!("name is required");
    }
    if manifest.description.trim().is_empty() {
        bail!("description is required");
    }
    if !is_semver(&manifest.version) {
        bail!("version must be SemVer: {}", manifest.version);
    }
    for (name, field) in manifest.inputs.iter().chain(manifest.outputs.iter()) {
        validate_field(name, field)?;
    }
    let plugin_dir = manifest_path
        .parent()
        .context("plugin manifest path has no parent")?;
    let entrypoint = plugin_dir.join(&manifest.entrypoint);
    if !entrypoint.exists() {
        bail!("entrypoint missing: {}", entrypoint.display());
    }
    if entrypoint.strip_prefix(root).is_err() {
        bail!("entrypoint must stay inside project root");
    }
    Ok(())
}

fn validate_field(name: &str, field: &RegistryField) -> Result<()> {
    if let Some(default) = &field.default
        && !value_matches_type(default, field.field_type)
    {
        bail!("default for {name} does not match declared type");
    }
    if let Some(values) = &field.enum_values {
        for value in values {
            if !value_matches_type(value, field.field_type) {
                bail!("enum value for {name} does not match declared type");
            }
        }
    }
    Ok(())
}

fn to_registry_plugin(
    root: &Path,
    manifest_path: &Path,
    manifest: RegistryPluginManifest,
) -> Result<RegistryPlugin> {
    let plugin_dir = manifest_path
        .parent()
        .context("plugin manifest path has no parent")?;
    let entrypoint = plugin_dir.join(&manifest.entrypoint);
    Ok(RegistryPlugin {
        name: manifest.name,
        version: manifest.version,
        description: manifest.description,
        runtime: manifest.runtime,
        entrypoint: normalize_path(
            entrypoint
                .strip_prefix(root)
                .context("entrypoint must be inside project root")?,
        ),
        manifest_path: normalize_path(
            manifest_path
                .strip_prefix(root)
                .context("manifest must be inside project root")?,
        ),
        plugin_dir: normalize_path(
            plugin_dir
                .strip_prefix(root)
                .context("plugin dir must be inside project root")?,
        ),
        inputs: manifest.inputs,
        outputs: manifest.outputs,
        permissions: manifest.permissions,
    })
}

fn manifest_paths(root: &Path) -> Result<Vec<PathBuf>> {
    let plugins_dir = root.join("plugins");
    if !plugins_dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for entry in fs::read_dir(&plugins_dir)
        .with_context(|| format!("failed to read {}", plugins_dir.display()))?
    {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let manifest = entry.path().join("plugin.yml");
            if manifest.exists() {
                paths.push(manifest);
            }
        }
    }
    paths.sort();
    Ok(paths)
}

fn registry_hash(plugins: &[RegistryPlugin]) -> Result<String> {
    let mut sorted_plugins = plugins.to_vec();
    sorted_plugins.sort_by(|left, right| left.name.cmp(&right.name));
    let value = canonicalize_value(serde_json::to_value(sorted_plugins)?);
    let bytes = serde_json::to_vec(&value)?;
    let digest = Sha256::digest(bytes);
    Ok(format!("sha256:{}", hex_lower(&digest)))
}

fn canonicalize_value(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(canonicalize_value).collect()),
        Value::Object(map) => {
            let sorted = map
                .into_iter()
                .map(|(key, value)| (key, canonicalize_value(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(sorted.into_iter().collect::<Map<_, _>>())
        }
        value => value,
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn normalize_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn is_semver(version: &str) -> bool {
    let parts = version.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.parse::<u64>().is_ok())
}

fn value_matches_type(value: &Value, field_type: RegistryFieldType) -> bool {
    match field_type {
        RegistryFieldType::String => value.is_string(),
        RegistryFieldType::Integer => value.as_i64().is_some(),
        RegistryFieldType::Number => value.is_number(),
        RegistryFieldType::Boolean => value.is_boolean(),
        RegistryFieldType::Object => value.is_object(),
        RegistryFieldType::Array => value.is_array(),
    }
}

fn field_type_map(fields: BTreeMap<String, RegistryField>) -> BTreeMap<String, RegistryFieldType> {
    fields
        .into_iter()
        .map(|(name, field)| (name, field.field_type))
        .collect()
}

fn agent_permissions(permissions: BTreeMap<String, Value>) -> BTreeMap<String, bool> {
    permissions
        .into_iter()
        .filter_map(|(name, value)| {
            value
                .get("required")
                .and_then(Value::as_bool)
                .map(|required| (format!("{name}_required"), required))
        })
        .collect()
}

fn validate_step_input(
    step_index: usize,
    input: Option<&Value>,
    plugin: &RegistryPlugin,
    diagnostics: &mut Vec<RegistryLintDiagnostic>,
) {
    let empty_input = Value::Object(Default::default());
    let input = input.unwrap_or(&empty_input);
    let Some(input_object) = input.as_object() else {
        diagnostics.push(RegistryLintDiagnostic {
            code: "invalid_input_type".to_owned(),
            message: "Plugin input must be an object.".to_owned(),
            field: format!("steps[{step_index}].input"),
            suggestion: None,
        });
        return;
    };

    for (name, field) in &plugin.inputs {
        if field.required && !input_object.contains_key(name) && field.default.is_none() {
            diagnostics.push(RegistryLintDiagnostic {
                code: "missing_required_input".to_owned(),
                message: format!("Missing required input {name}."),
                field: format!("steps[{step_index}].input.{name}"),
                suggestion: None,
            });
        }
    }

    for (name, value) in input_object {
        let Some(field) = plugin.inputs.get(name) else {
            diagnostics.push(RegistryLintDiagnostic {
                code: "unknown_input".to_owned(),
                message: format!("Input {name} is not declared by plugin {}.", plugin.name),
                field: format!("steps[{step_index}].input.{name}"),
                suggestion: None,
            });
            continue;
        };
        if !value_matches_type(value, field.field_type) {
            diagnostics.push(RegistryLintDiagnostic {
                code: "invalid_input_type".to_owned(),
                message: format!("Input {name} must be {}.", field.field_type.as_str()),
                field: format!("steps[{step_index}].input.{name}"),
                suggestion: None,
            });
        }
    }
}

fn validate_step_entrypoint(
    step_index: usize,
    step: &crate::dag::StepDefinition,
    plugin: &RegistryPlugin,
    diagnostics: &mut Vec<RegistryLintDiagnostic>,
) {
    let Some(run) = step.run.as_ref() else {
        diagnostics.push(RegistryLintDiagnostic {
            code: "entrypoint_mismatch".to_owned(),
            message: format!(
                "Plugin step must execute registered entrypoint {}.",
                plugin.entrypoint
            ),
            field: format!("steps[{step_index}].run"),
            suggestion: Some(format!("Reference {}", plugin.entrypoint)),
        });
        return;
    };

    let expected = normalize_registry_path(&plugin.entrypoint);
    let command = normalize_registry_path(run.command());
    let args = run
        .args()
        .iter()
        .map(|arg| normalize_registry_path(arg))
        .collect::<Vec<_>>();

    if command != expected && !args.iter().any(|arg| arg == &expected) {
        diagnostics.push(RegistryLintDiagnostic {
            code: "entrypoint_mismatch".to_owned(),
            message: format!(
                "Plugin {} must execute registered entrypoint {}.",
                plugin.name, plugin.entrypoint
            ),
            field: format!("steps[{step_index}].run"),
            suggestion: Some(format!("Use entrypoint {}", plugin.entrypoint)),
        });
    }
}

fn normalize_registry_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_owned()
}

pub fn invalid_payload(errors: Vec<RegistryLintDiagnostic>) -> Value {
    json!({
        "status": "invalid",
        "errors": errors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn scans_plugins_and_writes_stable_hash() {
        let root = temp_root("registry-scan");
        write_ssl_plugin(&root, "0.1.0");

        let first = write_registry(&root, scan_plugins(&root).unwrap()).unwrap();
        let second = write_registry(&root, scan_plugins(&root).unwrap()).unwrap();

        assert_eq!(first.registry_hash, second.registry_hash);
        assert_eq!(first.plugins[0].name, "ssl_check");
        assert_eq!(
            first.plugins[0].entrypoint,
            "plugins/ssl_check/check_ssl.py"
        );
        assert_eq!(
            first.plugins[0].manifest_path,
            "plugins/ssl_check/plugin.yml"
        );
        assert_eq!(first.plugins[0].plugin_dir, "plugins/ssl_check");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn writes_empty_registry_when_no_plugins_exist() {
        let root = temp_root("registry-empty");

        let document = write_registry(&root, scan_plugins(&root).unwrap()).unwrap();

        assert!(document.plugins.is_empty());
        assert!(document.registry_hash.starts_with("sha256:"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn check_fails_when_registry_is_out_of_date() {
        let root = temp_root("registry-check");
        write_ssl_plugin(&root, "0.1.0");
        scan_and_write(&root).unwrap();
        write_ssl_plugin(&root, "0.2.0");

        let error = scan_check(&root).unwrap_err();

        assert!(error.to_string().contains("registry is out of date"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn registry_hash_changes_when_manifest_changes() {
        let root = temp_root("registry-hash");
        write_ssl_plugin(&root, "0.1.0");
        let first = write_registry(&root, scan_plugins(&root).unwrap()).unwrap();
        write_ssl_plugin(&root, "0.2.0");
        let second = write_registry(&root, scan_plugins(&root).unwrap()).unwrap();

        assert_ne!(first.registry_hash, second.registry_hash);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rejects_missing_entrypoint() {
        let root = temp_root("registry-entrypoint");
        let plugin_dir = root.join("plugins").join("ssl_check");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(plugin_dir.join("plugin.yml"), manifest("0.1.0")).unwrap();

        let error = scan_plugins(&root).unwrap_err();

        assert!(error.to_string().contains("entrypoint missing"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn validates_workflow_plugin_inputs() {
        let root = temp_root("registry-lint");
        write_ssl_plugin(&root, "0.1.0");
        write_registry(&root, scan_plugins(&root).unwrap()).unwrap();
        let workflow = crate::dag::WorkflowDefinition::from_yaml(
            r#"
name: ssl-monitor
steps:
  - name: check
    type: plugin
    run:
      command: python
      args: ["plugins/ssl_check/check_ssl.py"]
    plugin_id: ssl_check
    input:
      port: "443"
"#,
        )
        .unwrap();

        let diagnostics = validate_workflow_plugins(&root, &workflow);

        assert!(
            diagnostics
                .iter()
                .any(|item| item.code == "missing_required_input")
        );
        assert!(
            diagnostics
                .iter()
                .any(|item| item.code == "invalid_input_type")
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn accepts_plugin_step_using_registered_entrypoint() {
        let root = temp_root("registry-entrypoint-valid");
        write_ssl_plugin(&root, "0.1.0");
        write_registry(&root, scan_plugins(&root).unwrap()).unwrap();
        let workflow = crate::dag::WorkflowDefinition::from_yaml(
            r#"
name: ssl-monitor
steps:
  - name: check
    type: plugin
    run:
      command: python
      args: ["plugins/ssl_check/check_ssl.py"]
    plugin_id: ssl_check
    input:
      host: api.example.com
      port: 443
"#,
        )
        .unwrap();

        let diagnostics = validate_workflow_plugins(&root, &workflow);

        assert!(diagnostics.is_empty());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rejects_plugin_step_using_wrong_entrypoint() {
        let root = temp_root("registry-entrypoint-invalid");
        write_ssl_plugin(&root, "0.1.0");
        write_registry(&root, scan_plugins(&root).unwrap()).unwrap();
        let workflow = crate::dag::WorkflowDefinition::from_yaml(
            r#"
name: ssl-monitor
steps:
  - name: check
    type: plugin
    run:
      command: python
      args: ["plugins/ssl_check/other.py"]
    plugin_id: ssl_check
    input:
      host: api.example.com
      port: 443
"#,
        )
        .unwrap();

        let diagnostics = validate_workflow_plugins(&root, &workflow);

        assert!(
            diagnostics
                .iter()
                .any(|item| item.code == "entrypoint_mismatch")
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn exports_stable_agent_context() {
        let root = temp_root("registry-agent-export");
        write_ssl_plugin(&root, "0.1.0");
        let plugins = scan_plugins(&root).unwrap();
        let document = RegistryDocument {
            schema_version: 1,
            generated_at: DateTime::parse_from_rfc3339("2026-06-12T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            registry_hash: registry_hash(&plugins).unwrap(),
            plugins,
        };
        let path = registry_path(&root);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();

        let context = export_for_agent(&root).unwrap();
        let snapshot = serde_json::to_string_pretty(&context).unwrap();

        assert_eq!(
            snapshot,
            r#"{
  "schema_version": 1,
  "registry_hash": "sha256:2b72dfd34b9962ea3a96d1364ae89f1476e185cca310b22398cb72f8cbd35c05",
  "generated_at": "2026-06-12T10:00:00Z",
  "plugins": [
    {
      "name": "ssl_check",
      "description": "Check SSL/TLS certificate expiration.",
      "inputs": {
        "host": "string",
        "port": "integer"
      },
      "outputs": {
        "status": "string"
      },
      "permissions": {
        "network_required": true
      }
    }
  ],
  "rules": [
    "Use only registered plugins.",
    "Do not invent tools.",
    "Return needs_tool if no plugin matches.",
    "Do not execute shell directly."
  ]
}"#
        );
        fs::remove_dir_all(root).ok();
    }

    fn temp_root(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!("{prefix}-{}", Uuid::new_v4()))
    }

    fn write_ssl_plugin(root: &Path, version: &str) {
        let plugin_dir = root.join("plugins").join("ssl_check");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(plugin_dir.join("plugin.yml"), manifest(version)).unwrap();
        fs::write(plugin_dir.join("check_ssl.py"), "print('{}')\n").unwrap();
    }

    fn manifest(version: &str) -> String {
        format!(
            r#"name: ssl_check
version: {version}
description: Check SSL/TLS certificate expiration.
runtime: python
entrypoint: check_ssl.py
inputs:
  host:
    type: string
    required: true
  port:
    type: integer
    default: 443
outputs:
  status:
    type: string
    enum: [ok, warning, expired, error]
permissions:
  network:
    required: true
    allow_ports: [443]
"#
        )
    }
}
