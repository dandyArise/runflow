# RunFlow Agent MVP Spec

Status: external project draft
Target repository: `runflow-agent`
RunFlow core status: frozen; no LLM or Agent CLI lives in this repository.
Scope: Agent v1 assist-only, implemented outside `runflow`

## Decision

RunFlow Agent v1 is a separate assistant project, not an operator built into RunFlow core.

It can read RunFlow state, generate drafts, review workflows, explain runs, and produce reports. It must not execute, cancel, schedule, notify, edit secrets, call external APIs, or mutate project state without a future explicit feature.

## Goals

- Help users create valid RunFlow workflows from plain language.
- Review an existing workflow before registration or execution.
- Explain why a run failed using events, manifests, step metadata, stdout, and stderr.
- Produce local daily reports from structured RunFlow data.
- Keep all agent output deterministic enough to validate before showing it.
- Keep RunFlow usable without any model installed and without any LLM dependency.

## Non-goals

- No autopilot.
- No automatic remediation.
- No direct shell execution by the agent.
- No external API calls by default.
- No secret management.
- No automatic `job add`, `job run`, `run cancel`, webhook, email, or notification.
- No fine-tuning work in v1.
- No MCP server dependency for the external agent.

## Safety Model

The model never controls RunFlow directly.

```text
RunFlow data
  -> Agent context builder
  -> Model provider
  -> Strict structured output
  -> JSON schema validation
  -> RunFlow workflow schema validation when YAML is generated
  -> Policy check
  -> User-facing result
```

If any step fails, the command returns a clear error and no state is changed.

## Modes

Only one mode is implemented in v1:

```toml
[agent]
enabled = false
mode = "assist"
provider = "ollama"
model = "qwen2.5-coder:1.5b"
timeout_seconds = 30
```

Future modes such as `watch`, `oncall`, and `autopilot` stay out of v1.

## Default Permissions

All action permissions are disabled in v1.

```toml
[agent.permissions]
create_workflows = false
edit_workflows = false
run_workflows = false
cancel_runs = false
rerun_failed = false
send_reports = false
send_alerts = false
call_external_api = false
manage_secrets = false
execute_shell = false
```

The agent may propose a command, but it must not run it.

## CLI Scope

The CLI belongs to the future `runflow-agent` repository. Do not add this command group to `runflow` core.

```powershell
runflow-agent <command>
```

### `runflow-agent draft`

Generate a workflow draft from a user request.

```powershell
runflow-agent draft --prompt "Ping 1.1.1.1 every 5 minutes"
runflow-agent draft --input .\request.txt
runflow-agent draft --prompt "Backup logs" --output .\workflow.yml
```

Behavior:

- returns YAML only by default;
- validates generated YAML with the RunFlow workflow schema;
- rejects unknown fields;
- does not register the job;
- does not run the job;
- writes the file only when `--output` is provided.

Accepted workflow fields must match `src/schema_defs/v1/workflow.schema.json`.

### `runflow-agent review`

Review an existing workflow file.

```powershell
runflow-agent review .\workflow.yml
runflow-agent review .\workflow.yml --format json
```

Behavior:

- validates schema first;
- reports invalid YAML or schema diagnostics;
- detects risky patterns such as shell redirection, unbounded output, missing timeout, broad env usage, or suspicious commands;
- suggests changes without editing the file.

### `runflow-agent explain-run`

Explain a run from structured RunFlow data.

```powershell
runflow-agent explain-run <run_id>
runflow-agent explain-run <run_id> --format json
```

Inputs:

- `.flow/runs/<run_id>/events.jsonl`;
- `.flow/runs/<run_id>/manifest.json`;
- `logs/<run_id>/workflow.metadata.json`;
- `logs/<run_id>/<step>/step.metadata.json`;
- `logs/<run_id>/<step>/stdout.log`;
- `logs/<run_id>/<step>/stderr.log`.

Behavior:

- summarizes the failed step;
- includes relevant exit code and stderr excerpts;
- explains likely cause;
- suggests manual next steps;
- never retries or cancels anything.

### `runflow-agent report daily`

Generate a local daily report.

```powershell
runflow-agent report daily
runflow-agent report daily --from 2026-06-09T00:00:00Z --to 2026-06-10T00:00:00Z
runflow-agent report daily --format json
```

Inputs:

- event store;
- manifests;
- SQLite projections when available.

Behavior:

- counts runs, successes, failures, cancellations;
- lists unstable jobs;
- lists open or recent incidents;
- suggests manual improvements;
- does not send the report anywhere.

## Output Contracts

Agent output must be schema validated before display.

Recommended schema files:

```text
schemas/agent-draft.schema.json
schemas/agent-review.schema.json
schemas/agent-explain-run.schema.json
schemas/agent-report.schema.json
```

### Draft Response

```json
{
  "kind": "draft_workflow",
  "workflow_yaml": "name: ping-monitor\nsteps:\n  - name: ping\n    type: command\n    run:\n      command: ping\n      args: [\"-n\", \"4\", \"1.1.1.1\"]\n",
  "validation": {
    "valid": true,
    "errors": []
  },
  "warnings": []
}
```

### Review Response

```json
{
  "kind": "workflow_review",
  "valid": true,
  "findings": [
    {
      "severity": "warning",
      "path": "/steps/0/run",
      "message": "Command has no timeout.",
      "suggestion": "Add timeout: 30s."
    }
  ]
}
```

### Run Explanation Response

```json
{
  "kind": "run_explanation",
  "run_id": "<uuid>",
  "status": "FAILED",
  "summary": "The ping step failed because the command exited with code 1.",
  "failed_step": "ping",
  "evidence": [
    "exit_code=1",
    "stderr contains timeout"
  ],
  "suggested_next_steps": [
    "Check network connectivity.",
    "Increase timeout if failures are intermittent."
  ]
}
```

### Daily Report Response

```json
{
  "kind": "daily_report",
  "period": {
    "from": "2026-06-09T00:00:00Z",
    "to": "2026-06-10T00:00:00Z"
  },
  "runs": {
    "total": 0,
    "success": 0,
    "failed": 0,
    "cancelled": 0
  },
  "unstable_jobs": [],
  "incidents": [],
  "recommendations": []
}
```

## Workflow Generation Rules

Generated workflows must follow current RunFlow schema.

Allowed top-level fields in v1:

- `name`
- `version`
- `schema_version`
- `schedule`
- `failure_policy`
- `concurrency`
- `limits`
- `locks`
- `secrets`
- `notifications`
- `retention`
- `steps`
- `tests`

Allowed step types:

- `command`
- `plugin`
- `sleep`
- `wait_until`

Rules:

- prefer structured command form: `run.command` + `run.args`;
- avoid shell strings unless the user explicitly asks for shell behavior;
- do not invent fields outside the schema;
- do not inline secrets;
- use `secrets.<name>.from_env` for secret references;
- use `schedule: false` or omit `schedule` for manual workflows;
- use detailed `schedule.cron`, `schedule.timezone`, `schedule.enabled` for scheduled workflows;
- include timeouts for command steps when possible.

## Context Builder

The context builder must keep prompts small and factual.

For workflow draft:

- user request;
- supported schema summary;
- examples from current schema only.

For workflow review:

- workflow YAML;
- schema diagnostics;
- extracted commands;
- schedule and concurrency settings.

For run explanation:

- run state;
- event timeline;
- failed steps;
- exit codes;
- bounded stdout/stderr excerpts;
- manifest summary.

For daily report:

- aggregate counts;
- unstable jobs;
- recent failures;
- cancellation count;
- slowest runs if available.

Never send full unbounded logs to the model.

## Policy Checks

The v1 policy engine is mostly a deny-by-default validator.

It must reject:

- requests to execute shell commands;
- requests to run, cancel, or rerun jobs;
- requests to send webhooks, emails, Slack messages, or external API calls;
- generated workflows with invalid schema;
- generated workflows containing raw secret values;
- generated workflows with unknown fields.

It may allow:

- local file output when the user passes `--output`;
- text and JSON summaries;
- workflow YAML drafts.

## Audit Trail

V1 should record agent command usage, but not as run events tied to fake runs.

Recommended local file:

```text
.flow/agent/audit.jsonl
```

Each line:

```json
{
  "timestamp": "2026-06-09T12:00:00Z",
  "command": "agent review",
  "model": "qwen2.5-coder:1.5b",
  "status": "success",
  "changed_files": [],
  "warnings": []
}
```

If later RunFlow gets global events, agent audit can move there.

## Internal Rust Structure

Suggested files in `runflow-agent`:

```text
src/cli.rs
src/config.rs
src/context.rs
src/model.rs
src/output.rs
src/policy.rs
src/report.rs
src/review.rs
src/draft.rs
src/audit.rs
```

Core types stay outside text CLI parsing. Agent code must call Rust modules directly.

## Model Provider

V1 provider:

- Ollama HTTP API on localhost.

Required behavior:

- timeout;
- clear error when Ollama is unavailable;
- no panic;
- deterministic settings where possible;
- low temperature for YAML/JSON generation.

RunFlow core must still build and run without Ollama installed because Ollama belongs to `runflow-agent`.

## Acceptance Criteria

V1 is done when:

- `runflow-agent draft` generates schema-valid workflow YAML for simple requests;
- `runflow-agent review` reports schema errors and risk findings without editing files;
- `runflow-agent explain-run` explains a failed run from local logs/events;
- `runflow-agent report daily` produces text and JSON summaries;
- all model outputs are JSON-schema validated;
- generated workflows are validated by the existing workflow schema;
- no agent command runs jobs, cancels runs, sends alerts, or calls external APIs;
- errors are returned cleanly;
- tests cover success, invalid model output, invalid workflow YAML, missing run, and Ollama unavailable.

## Implementation Plan

1. Create the separate `runflow-agent` project with CLI and config parsing.
2. Add model provider trait with Ollama implementation.
3. Add output schemas and validation.
4. Add workflow draft command.
5. Add workflow review command.
6. Add run explanation command.
7. Add daily report command.
8. Add audit JSONL.
9. Add `runflow-agent` README sections after the feature is implemented.

## Open Questions

- Should `runflow-agent draft --output` overwrite by default or require `--force`?
- Should Ollama be the only v1 provider, or should a mock provider be available for tests and offline demos?
- Should report generation use SQLite projections first, then fallback to event replay?
- Should agent audit become a first-class global event type later?
