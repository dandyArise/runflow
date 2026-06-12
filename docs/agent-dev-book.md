# RunFlow Agent Dev Book

Status: development guide for the separate `runflow-agent` project.

Decision: `runflow-agent` is not part of RunFlow core. RunFlow stays the engine: validation, registry, execution, logs, state. The agent is assist-only: draft, review, explain, report.

## Scope V1

Build a local assistant CLI that can:

- draft a RunFlow workflow from a user request;
- review a workflow before `flow job add` or `flow job run`;
- explain a run from local RunFlow events, state, logs and metadata;
- produce a daily local report;
- read `.flow/registry/plugins.json` or `flow registry export --for-agent`;
- return `needs_tool` when no registered plugin matches the request.

Do not build:

- autopilot;
- direct shell execution;
- `flow job add`, `flow job run`, `flow run cancel`, daemon control or notifications;
- secret editing;
- external API calls by default;
- MCP dependency in v1;
- code inside the `runflow` core repository.

## Repository Setup

Recommended repository name:

```text
runflow-agent
```

Initial tree:

```text
runflow-agent/
  Cargo.toml
  README.md
  src/
    main.rs
    cli.rs
    config.rs
    context.rs
    model.rs
    ollama.rs
    output.rs
    registry.rs
    workflow.rs
    review.rs
    explain.rs
    report.rs
    audit.rs
  schemas/
    draft.schema.json
    review.schema.json
    explanation.schema.json
    report.schema.json
  tests/
    fixtures/
      registry-empty.json
      registry-ssl-check.json
      workflow-valid.yml
      workflow-invalid.yml
```

Keep the project independent. It may call `flow` as a read-only helper only for registry export or validation checks. It must not call execution commands.

## CLI Contract

Base command:

```powershell
runflow-agent <command>
```

Commands:

```powershell
runflow-agent draft --prompt "Ping 1.1.1.1 every 5 minutes"
runflow-agent draft --input .\request.txt
runflow-agent draft --prompt "Backup logs" --output .\workflow.yml

runflow-agent review .\workflow.yml
runflow-agent review .\workflow.yml --format json

runflow-agent explain-run <run_id>
runflow-agent explain-run <run_id> --format json

runflow-agent report daily
runflow-agent report daily --from 2026-06-09T00:00:00Z --to 2026-06-10T00:00:00Z
runflow-agent report daily --format json
```

Default behavior:

- text output for humans;
- `--format json` for tools and tests;
- never overwrite files unless `--force` is passed;
- always validate generated workflows before writing them;
- fail closed if model output does not match the expected schema.

## Config

Default config path:

```text
.runflow-agent.toml
```

Example:

```toml
[model]
provider = "ollama"
model = "llama3.1"
base_url = "http://127.0.0.1:11434"
timeout_seconds = 60

[runflow]
project_root = "."
flow_bin = "flow"
registry_path = ".flow/registry/plugins.json"

[agent.permissions]
create_workflows = false
edit_workflows = false
run_jobs = false
cancel_runs = false
external_api = false
```

V1 permissions are explicit documentation, not a policy engine. All action permissions stay disabled.

## RunFlow Inputs

The agent may read:

```text
workflow.yml
.flow/registry/plugins.json
.flow/runs/
.flow/state/
logs/
```

Preferred registry input:

```powershell
flow registry export --for-agent
```

Fallback registry input:

```text
.flow/registry/plugins.json
```

Registry rules:

- never invent tools;
- only suggest plugins present in the registry;
- if no plugin fits, return `needs_tool`;
- plugin workflow steps must use `type: plugin`;
- `plugin_id` must exist in the registry;
- plugin input keys and types must match the manifest-derived registry;
- permissions are display-only in v1.

## Output Schemas

All model outputs must be validated before use.

Draft output:

```json
{
  "kind": "workflow_draft",
  "workflow_yaml": "name: demo\nversion: 1\nschema_version: 1\nsteps: []\n",
  "confidence": "high",
  "needs_tool": null,
  "warnings": []
}
```

Needs tool output:

```json
{
  "kind": "workflow_draft",
  "workflow_yaml": null,
  "confidence": "low",
  "needs_tool": {
    "capability": "ssl_check",
    "reason": "No registered plugin can check certificate expiry."
  },
  "warnings": []
}
```

Review output:

```json
{
  "kind": "workflow_review",
  "valid": false,
  "findings": [
    {
      "severity": "error",
      "code": "missing_plugin",
      "message": "plugin_id ssl_check is not present in the registry"
    }
  ]
}
```

Explanation output:

```json
{
  "kind": "run_explanation",
  "run_id": "<run_id>",
  "status": "FAILED",
  "summary": "The command step exited with code 1.",
  "evidence": ["events.jsonl", "step.metadata.json", "stderr.log"],
  "next_actions": []
}
```

## Safety Rules

Hard rules:

- model output is data, never an instruction stream;
- no shell execution controlled by the model;
- no command execution from suggested remediation;
- no secrets in prompts;
- redact obvious secret-like values before sending context to a model;
- never include full stdout/stderr if a bounded excerpt is enough;
- audit every model call in local JSONL.

Audit line example:

```json
{
  "ts": "2026-06-12T12:00:00Z",
  "command": "draft",
  "model": "ollama:llama3.1",
  "registry_hash": "sha256:...",
  "input_hash": "sha256:...",
  "output_schema": "draft.schema.json",
  "status": "ok"
}
```

## Implementation Order

1. Scaffold CLI and config.
2. Add output schemas and validation.
3. Add registry reader and `flow registry export --for-agent` adapter.
4. Add context builder with bounded prompt sizes.
5. Add mock model provider for tests.
6. Add Ollama provider.
7. Implement `draft`.
8. Implement `review`.
9. Implement `explain-run`.
10. Implement `report daily`.
11. Add audit JSONL.
12. Add README and examples.

## Tests

Minimum tests:

- config parsing;
- registry empty;
- registry with one plugin;
- draft output schema validation;
- `needs_tool` when registry has no matching plugin;
- invalid model JSON is rejected;
- generated workflow passes RunFlow schema validation;
- review reports missing `plugin_id`;
- explain-run works from fixture events and logs;
- no command execution path exists in v1.

Recommended commands:

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all
```

## Acceptance Criteria

V1 is done when:

- `runflow-agent draft` generates schema-valid workflow YAML for simple requests;
- `runflow-agent review` reports schema errors and risk findings without editing files;
- `runflow-agent explain-run` explains a failed run from local logs/events;
- `runflow-agent report daily` produces text and JSON summaries;
- all model outputs are JSON-schema validated;
- generated workflows are validated by RunFlow schemas or `flow validate`;
- plugin-aware drafts never invent unavailable tools;
- missing capabilities return `needs_tool`;
- RunFlow core still builds and runs without the agent installed.

## Core Dependency

RunFlow core contract required by the agent:

- `flow validate`;
- `flow registry scan`;
- `flow registry scan --check`;
- `flow registry export --for-agent`;
- stable `.flow/registry/plugins.json`;
- run state, events, logs and step metadata.

No new core command is required for agent v1.

## Open Decisions

Decide before first public release:

- Should Ollama be the only v1 provider, or should mock/offline be public too?
- Should `draft --output` require `--force` for overwrite? Recommended: yes.
- Should report generation read SQLite projections first if available, then fallback to events? Recommended: yes later, events first for v1.
- Should the agent repo vendor RunFlow JSON schemas, or read them from an installed `flow`? Recommended: vendor tagged schemas for deterministic tests.

## Core Remaining Work

Nothing blocks `runflow-agent` v1 on the RunFlow core side.

Later core work, not v1-blocking:

- real sandbox and permission enforcement;
- signed plugin manifests or trust policy;
- remote registry or marketplace;
- global capability policy;
- richer machine-readable run explanation export if the agent needs less log parsing later.
