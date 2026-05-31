# RunFlow

RunFlow is a local workflow runner written in Rust. The CLI binary is named `flow`.

It currently supports:

- validating YAML workflows;
- registering and running jobs;
- creating an isolated workspace per run;
- capturing command stdout/stderr without shell redirection;
- writing workflow and step log metadata;
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
name: demo
version: 1
schema_version: 1
steps:
  - name: hello
    type: command
    run:
      command: echo
      args: ["hello"]
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
logs/<run_id>/workflow.metadata.json
logs/<run_id>/<step_name>/step.metadata.json
logs/<run_id>/<step_name>/stdout.log
logs/<run_id>/<step_name>/stderr.log
```

RunFlow captures stdout and stderr itself with Rust. Recommended workflows do not need `cmd`, `powershell`, `bash`, `>`, `2>&1`, or pipes just to produce logs.

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
logs/
  <run_id>/workflow.metadata.json
  <run_id>/<step_name>/stdout.log
  <run_id>/<step_name>/stderr.log
  <run_id>/<step_name>/step.metadata.json
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

`validate` also checks the optional workflow `schedule` cron expression. `migrate` currently validates the workflow and reports that no migration is required.

### Schedules

RunFlow accepts cron expressions through the `cron` crate format:

```text
sec min hour day-of-month month day-of-week year
```

Preview the next executions for an expression:

```powershell
flow schedule next "0 */5 * * * * *"
flow schedule next "0 */5 * * * * *" --count 3
flow schedule next "0 */5 * * * * *" --from 2026-05-31T14:00:00Z
```

Preview the next executions from a workflow file containing `schedule:`:

```powershell
flow schedule workflow .\workflow.yml
```

Recommended workflow format:

```yaml
name: scheduled-job
schedule:
  cron: "0 */5 * * * * *"
  timezone: Europe/Paris
  enabled: true
steps:
  - name: hello
    type: command
    run:
      command: echo
      args: ["hello"]
```

The short format remains supported and is equivalent to UTC + enabled:

```yaml
schedule: "0 */5 * * * * *"
```

When `flow daemon start` is running, scheduled jobs are automatically enqueued when their next cron occurrence is due. The daemon stores schedule cursors in `.flow/daemon/schedules/` so a job is not enqueued twice for the same occurrence.

Examples:

```text
"0 0 * * * * *"      # every hour
"0 */10 * * * * *"   # every 10 minutes
"0 30 9 * * Mon *"   # every Monday at 09:30 UTC
```

### Jobs

```powershell
flow job add .\workflow.yml
flow job update .\workflow.yml
flow job delete <job_name>
flow job clear
flow job list
flow job show <job_name>
flow job run <job_name>
```

### Runs

```powershell
flow run list
flow run show <run_id>
flow run logs <run_id>
flow run summary <run_id>
flow run output <run_id> <step_name>
flow run output <run_id> <step_name> --stderr
flow run cancel <run_id>
```

`run logs` reads JSONL events. `run summary` reads `logs/<run_id>/workflow.metadata.json`. `run output` prints `stdout.log` or `stderr.log` for one step.

### Steps

These commands record a manual action in the run event log.

```powershell
flow step retry <run_id> <step_name>
flow step restart <run_id> <step_name>
flow step reset <run_id> <step_name>
flow step skip <run_id> <step_name>
flow step rerun-from <run_id> <step_name>
```

### Job Test

```powershell
flow test <job_name>
flow test <job_name> --verbose
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

`.flowpkg` files are JSON packages containing the package format version, job name, workflow versions, the workflow YAML, and a deterministic workflow checksum. Installing still accepts legacy `.flowpkg` files that contain only workflow YAML.

### Daemon

```powershell
flow daemon start
flow daemon status
flow daemon stop
flow daemon restart
```

When the daemon is running, `flow job run <job_name>` enqueues the run instead of executing it in the foreground. The daemon also scans registered jobs with `schedule.enabled: true`, enqueues them at the next due cron occurrence, exposes JSON status with heartbeat, active run and queue size, and supports clean stop/restart requests.

`flow run cancel <run_id>` cancels queued runs directly. For an active daemon run, it records a cancel marker, kills the tracked process tree, writes `PROCESS_KILLED` / `PROCESS_TREE_KILLED`, and the daemon finishes the run as `CANCELLED`.

### Retention

```powershell
flow retention run
flow retention run --dry-run --keep-runs 20
flow retention run --keep-runs 20 --older-than-days 30
```

Retention scans `.flow/runs`, keeps the newest runs according to `--keep-runs`, optionally limits deletion to runs older than `--older-than-days`, and removes matching run snapshots too. With no policy options, it only scans and reports `0` removals.

## Minimal Workflow Format

The smallest valid draft workflow only needs `name`:

```yaml
name: draft-job
```

RunFlow defaults missing fields to:

```yaml
version: 1
schema_version: 1
steps: []
```

Draft workflows can be validated, added, listed and edited. They cannot be run, tested or packaged until they contain at least one step.

Runnable workflows need steps:

```yaml
name: backup-db
version: 1
schema_version: 1
steps:
  - name: dump
    type: command
    run:
      command: echo
      args: ["backup"]
```

Preferred command steps use `run.command` plus a separate string per argument:

```yaml
name: network-check
version: 1
schema_version: 1
steps:
  - name: ping_google
    type: command
    run:
      command: ping
      args: ["-n", "4", "google.com"]
```

Shells remain supported for advanced, platform-specific steps, but RunFlow still captures its own logs:

```yaml
steps:
  - name: list_directory_windows
    type: command
    run:
      command: cmd
      args: ["/C", "dir"]
```

Shell steps are marked with `is_shell: true` and a portability warning in `step.metadata.json`.

Avoid this in recommended examples:

```yaml
run:
  command: cmd
  args: ["/C", "ping -n 4 google.com > ping.log 2>&1"]
```

## Step By Step: Ping With Logs

Create `workflow.yml`:

```yaml
name: ping-cloudflare
version: 1
schema_version: 1

steps:
  - name: ping_1111
    type: command
    run:
      command: ping
      args: ["-n", "4", "1.1.1.1"]
```

Run it:

```powershell
flow job add .\workflow.yml
$runId = flow job run ping-cloudflare
```

Read the generated logs through RunFlow:

```powershell
flow run summary $runId
flow run output $runId ping_1111
flow run output $runId ping_1111 --stderr
```

Or read the files directly:

```powershell
Get-Content ".\logs\$runId\workflow.metadata.json"
Get-Content ".\logs\$runId\ping_1111\step.metadata.json"
Get-Content ".\logs\$runId\ping_1111\stdout.log"
Get-Content ".\logs\$runId\ping_1111\stderr.log"
```

On Linux/macOS, use `args: ["-c", "4", "1.1.1.1"]` for `ping`.

## Release

GitHub Releases are built from tags:

```powershell
git tag v0.1.1
git push origin v0.1.1
```

The release workflow builds `flow.exe` on Windows and publishes:

```text
flow-windows-x64.exe
flow-windows-x64.exe.sha256
```

Supported step types in the current engine:

- `command`
- `sleep`
- `wait_until`
- `plugin`

Example `sleep` step:

```yaml
name: wait-demo
version: 1
schema_version: 1
steps:
  - name: pause
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
