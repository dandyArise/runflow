# RunFlow

RunFlow est un runner de workflows local, écrit en Rust. Le binaire CLI s'appelle `flow`.

Il sait déjà :

- valider des workflows YAML ;
- enregistrer et lancer des jobs ;
- créer un workspace isolé par run ;
- écrire les événements du run en JSONL ;
- afficher les runs et leurs logs ;
- gérer les actions manuelles de step ;
- initialiser et valider des manifests plugin ;
- construire et installer un package simple `.flowpkg`.

## Prérequis

- Windows PowerShell ;
- Git ;
- Rust stable avec Cargo.

Vérifier Rust :

```powershell
rustc --version
cargo --version
```

## Installation depuis GitHub

```powershell
git clone https://github.com/dandyArise/runflow.git
Set-Location .\runflow
cargo build --release
```

Le binaire est généré ici :

```powershell
.\target\release\flow.exe
```

Optionnel : ajouter le dossier au `PATH` pour utiliser `flow` partout.

```powershell
$runflowBin = (Resolve-Path .\target\release).Path
[Environment]::SetEnvironmentVariable(
  "Path",
  [Environment]::GetEnvironmentVariable("Path", "User") + ";$runflowBin",
  "User"
)
```

Ouvrir un nouveau terminal, puis vérifier :

```powershell
flow version
```

## Utilisation sans installation globale

Depuis le dossier du repo :

```powershell
cargo run -- version
```

Pour passer des options au binaire :

```powershell
cargo run -- --root . version
```

## Exemple rapide

Créer `workflow.yml` :

```yaml
id: demo
version: 1
schema_version: 1
steps:
  - id: hello
    type: command
    run: echo hello
```

Valider :

```powershell
flow validate .\workflow.yml
```

Ajouter le job :

```powershell
flow job add .\workflow.yml
```

Lister les jobs :

```powershell
flow job list
```

Lancer le job :

```powershell
flow job run demo
```

La commande retourne un `run_id`.

Lister les runs :

```powershell
flow run list
```

Afficher les logs JSONL d'un run :

```powershell
flow run logs <run_id>
```

## Racine de travail

Par défaut, RunFlow utilise le dossier courant comme racine.

Pour isoler les données dans un autre dossier :

```powershell
flow --root C:\Temp\runflow-demo job add .\workflow.yml
flow --root C:\Temp\runflow-demo job run demo
flow --root C:\Temp\runflow-demo run list
```

RunFlow crée ses données internes dans :

```text
.flow/
  jobs/
  runs/
  packages/
  plugins/
```

Le dossier `.flow/` est ignoré par Git.

## Commandes CLI

### Version

```powershell
flow version
```

### Workflow

```powershell
flow validate .\workflow.yml
flow migrate .\workflow.yml
```

`migrate` valide actuellement le workflow et indique qu'aucune migration n'est requise.

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

Ces commandes enregistrent une action manuelle dans l'event log du run.

```powershell
flow step retry <run_id> <step_id>
flow step restart <run_id> <step_id>
flow step reset <run_id> <step_id>
flow step skip <run_id> <step_id>
flow step rerun-from <run_id> <step_id>
```

### Test de job

```powershell
flow test <job_id>
flow test <job_id> --verbose
```

### Plugins

Initialiser un squelette plugin :

```powershell
flow plugin init rust .\my-plugin
flow plugin init java .\my-plugin
flow plugin init python .\my-plugin
flow plugin init node .\my-plugin
```

Valider un manifest plugin :

```powershell
flow plugin validate .\my-plugin\plugin\manifest.json
```

Inspecter un manifest :

```powershell
flow plugin inspect .\my-plugin\plugin\manifest.json
```

Tester une sortie plugin JSON :

```powershell
flow plugin test "unused-command" .\plugin-output.json
```

### Packages

Construire un package depuis un workflow :

```powershell
flow package build .\workflow.yml
```

Installer un package :

```powershell
flow package install .\.flow\packages\demo.flowpkg
```

### Daemon

```powershell
flow daemon start
flow daemon status
flow daemon stop
```

État actuel : le daemon est un verrou PID minimal, pas encore un service long-running complet.

### Retention

```powershell
flow retention run
```

État actuel : la purge réelle est prévue pour une phase suivante.

## Format workflow minimal

```yaml
id: backup-db
version: 1
schema_version: 1
steps:
  - id: dump
    type: command
    run: echo backup
```

Types de steps supportés dans le moteur actuel :

- `command`
- `sleep`
- `wait_until`
- `plugin`

Exemple `sleep` :

```yaml
id: wait-demo
version: 1
schema_version: 1
steps:
  - id: pause
    type: sleep
    duration: 1s
```

## Développement

Formater :

```powershell
cargo fmt --all
```

Vérifier le format :

```powershell
cargo fmt --all --check
```

Lint strict :

```powershell
cargo clippy --all-targets --all-features -- -D warnings
```

Tests :

```powershell
cargo test --all
```

Build :

```powershell
cargo build --all
```

Build release :

```powershell
cargo build --release
```

## État actuel

RunFlow est en développement actif.

Fonctionnel maintenant :

- CLI complète S6 ;
- validation de schémas ;
- event store JSONL ;
- DAG workflow ;
- moteur d'exécution local ;
- workspace isolé ;
- snapshots ;
- projections SQLite ;
- runtime plugin de base.

Limites connues :

- daemon encore minimal ;
- cancellation CLI enregistrée dans les événements, sans arrêt d'un processus déjà lancé par un daemon ;
- retention sans purge réelle ;
- packaging encore simple, basé sur le YAML workflow.
