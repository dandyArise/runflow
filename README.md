# RunFlow

RunFlow is a local workflow runner written in Rust. The CLI binary is named `flow`.

It currently supports:

- validating YAML workflows;
- registering and running jobs;
- creating an isolated workspace per run;
- writing run events as JSONL;
- writing completion manifests for finished runs;
- listing runs and reading run logs;
- recording manual step actions;
- pruning old run data with retention policies;
- initializing and validating plugin manifests;
- building and installing structured `.flowpkg` packages with workflow checksums.

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

Each completed run also writes:

```text
.flow/runs/<run_id>/manifest.json
```

The manifest includes workflow versions, timestamps, final status, failure policy, artifacts, and basic metrics.

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
    <run_id>/manifest.json
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

Validate plugin output JSON, or execute a plugin command with a `PluginInput` sample. Real execution uses an isolated `.flow/plugin-tests/<uuid>/workspace` directory and validates the command stdout as plugin output JSON.

```powershell
flow plugin test "unused-command" .\plugin-output.json
flow plugin test "powershell -NoProfile -File .\plugin.ps1" .\plugin-input.json
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

`.flowpkg` files are JSON packages containing the package format version, job id, workflow versions, the workflow YAML, and a deterministic workflow checksum. Installing still accepts legacy `.flowpkg` files that contain only workflow YAML.

### Daemon

```powershell
flow daemon start
flow daemon status
flow daemon stop
flow daemon restart
```

When the daemon is running, `flow job run <job_id>` enqueues the run instead of executing it in the foreground. The daemon processes the queue, exposes JSON status with heartbeat, active run and queue size, and supports clean stop/restart requests.

`flow run cancel <run_id>` cancels queued runs directly. For an active daemon run, it records a cancel marker, kills the tracked process tree, writes `PROCESS_KILLED` / `PROCESS_TREE_KILLED`, and the daemon finishes the run as `CANCELLED`.

### Retention

```powershell
flow retention run
flow retention run --dry-run --keep-runs 20
flow retention run --keep-runs 20 --older-than-days 30
```

Retention scans `.flow/runs`, keeps the newest runs according to `--keep-runs`, optionally limits deletion to runs older than `--older-than-days`, and removes matching run snapshots too. With no policy options, it only scans and reports `0` removals.

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
- run completion manifests;
- workflow DAG;
- local execution engine;
- isolated workspaces;
- snapshots;
- SQLite projections;
- retention cleanup for runs and snapshots;
- structured `.flowpkg` packages;
- daemon queue with real status/stop/restart;
- process-tree cancellation for daemon runs;
- plugin runtime with isolated command test.

Known limitations:

- no distributed worker protocol yet;
- plugin execution is local-command based;
