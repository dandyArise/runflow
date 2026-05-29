# RunFlow

RunFlow is a local workflow runner written in Rust. The CLI binary is named `flow`.

It currently supports:

- validating YAML workflows;
- registering and running jobs;
- creating an isolated workspace per run;
- writing run events as JSONL;
- listing runs and reading run logs;
- recording manual step actions;
- initializing and validating plugin manifests;
- building and installing simple `.flowpkg` packages.

## Requirements

- Windows PowerShell;
- Git;
- stable Rust with Cargo.

Check Rust:

```powershell
rustc --version
cargo --version
```

## Install From GitHub

```powershell
git clone https://github.com/dandyArise/runflow.git
Set-Location .\runflow
cargo build --release
```

The binary is generated here:

```powershell
.\target\release\flow.exe
```

Optional: add the release directory to your user `PATH`.

```powershell
$runflowBin = (Resolve-Path .\target\release).Path
[Environment]::SetEnvironmentVariable(
  "Path",
  [Environment]::GetEnvironmentVariable("Path", "User") + ";$runflowBin",
  "User"
)
```

Open a new terminal, then verify the install:

```powershell
flow version
```

## Use Without Global Install

From the repository directory:

```powershell
cargo run -- version
```

Pass options to the binary after `--`:

```powershell
cargo run -- --root . version
```

## Quick Start

Create `workflow.yml`:

```yaml
id: demo
version: 1
schema_version: 1
steps:
  - id: hello
    type: command
    run: echo hello
```

Validate it:

```powershell
flow validate .\workflow.yml
```

Register the job:

```powershell
flow job add .\workflow.yml
```

List jobs:

```powershell
flow job list
```

Run the job:

```powershell
flow job run demo
```

The command prints a `run_id`.

List runs:

```powershell
flow run list
```

Read JSONL logs for a run:

```powershell
flow run logs <run_id>
```

## Workspace Root

By default, RunFlow uses the current directory as its root.

Use `--root` to store runtime data somewhere else:

```powershell
flow --root C:\Temp\runflow-demo job add .\workflow.yml
flow --root C:\Temp\runflow-demo job run demo
flow --root C:\Temp\runflow-demo run list
```

RunFlow stores internal data in:

```text
.flow/
  jobs/
  runs/
  packages/
  plugins/
```

The `.flow/` directory is ignored by Git.

## CLI Commands

### Version

```powershell
flow version
```

### Workflow

```powershell
flow validate .\workflow.yml
flow migrate .\workflow.yml
```

`migrate` currently validates the workflow and reports that no migration is required.

### Jobs

```powershell
flow job add .\workflow.yml
flow job list
flow job show <job_id>
flow job run <job_id>
```

### Runs

```powershell
flow run list
flow run show <run_id>
flow run logs <run_id>
flow run cancel <run_id>
```

### Steps

These commands record a manual action in the run event log.

```powershell
flow step retry <run_id> <step_id>
flow step restart <run_id> <step_id>
flow step reset <run_id> <step_id>
flow step skip <run_id> <step_id>
flow step rerun-from <run_id> <step_id>
```

### Job Test

```powershell
flow test <job_id>
flow test <job_id> --verbose
```

### Plugins

Initialize a plugin skeleton:

```powershell
flow plugin init rust .\my-plugin
flow plugin init java .\my-plugin
flow plugin init python .\my-plugin
flow plugin init node .\my-plugin
```

Validate a plugin manifest:

```powershell
flow plugin validate .\my-plugin\plugin\manifest.json
```

Inspect a manifest:

```powershell
flow plugin inspect .\my-plugin\plugin\manifest.json
```

Validate plugin output JSON:

```powershell
flow plugin test "unused-command" .\plugin-output.json
```

### Packages

Build a package from a workflow:

```powershell
flow package build .\workflow.yml
```

Install a package:

```powershell
flow package install .\.flow\packages\demo.flowpkg
```

### Daemon

```powershell
flow daemon start
flow daemon status
flow daemon stop
```

Current state: the daemon command is a minimal PID lock, not a full long-running service yet.

### Retention

```powershell
flow retention run
```

Current state: real purge logic is planned for a later phase.

## Minimal Workflow Format

```yaml
id: backup-db
version: 1
schema_version: 1
steps:
  - id: dump
    type: command
    run: echo backup
```

Supported step types in the current engine:

- `command`
- `sleep`
- `wait_until`
- `plugin`

Example `sleep` step:

```yaml
id: wait-demo
version: 1
schema_version: 1
steps:
  - id: pause
    type: sleep
    duration: 1s
```

## Development

Format:

```powershell
cargo fmt --all
```

Check formatting:

```powershell
cargo fmt --all --check
```

Strict lint:

```powershell
cargo clippy --all-targets --all-features -- -D warnings
```

Tests:

```powershell
cargo test --all
```

Build:

```powershell
cargo build --all
```

Release build:

```powershell
cargo build --release
```

## Current Status

RunFlow is under active development.

Working now:

- complete S6 CLI;
- schema validation;
- JSONL event store;
- workflow DAG;
- local execution engine;
- isolated workspaces;
- snapshots;
- SQLite projections;
- basic plugin runtime.

Known limitations:

- daemon is still minimal;
- CLI cancellation is recorded in events, but does not stop a process already launched by a daemon;
- retention does not purge data yet;
- packaging is still simple and based on the workflow YAML.
