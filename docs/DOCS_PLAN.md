# RunFlow Documentation Plan (FR/EN)

Ce fichier est une checklist de couverture pour la doc GitHub Pages (`docs/index.html`). Objectif: ne rien oublier (toutes les commandes + comportements).

---

## EN (Coverage Checklist)

1. What is RunFlow (concepts: workflow/job/run/step/event/manifest)
2. Install/build (source build, binary path, optional PATH, `cargo run -- ...`)
3. Quick start (validate/add/run/output/summary) + ping example
4. Workflow YAML reference (v1): `name`-only drafts, default fields, runnable workflows requiring steps, step types (`command/sleep/wait_until/plugin`)
5. CLI reference: list *every* command and flags
6. Cron schedules: `schedule:`, `flow schedule next`, `flow schedule workflow`, cron field format
7. Daemon behavior: queue, status, clean stop/restart
8. Cancel behavior: queued vs active run, process-tree kill, events written
9. Plugins: manifest + schemas, `plugin test` execution rules (cwd/workspace/timeout/validation)
10. Packages (`.flowpkg`): build/install, checksum, install location
11. Retention: dry-run/delete, keep-runs, older-than-days, what is deleted
12. Troubleshooting: common errors + where to look (`logs/`, `.flow/`)
13. Release: tagging + artifacts
14. Development: fmt/clippy/test/build

Canonical source of truth for the CLI list: `src/cli.rs` enums (`Command`, `JobCommand`, `RunCommand`, etc.).

---

## FR (Checklist de couverture)

1. C’est quoi RunFlow (concepts: workflow/job/run/step/event/manifest)
2. Installer/build (depuis les sources, binaire, PATH, `cargo run -- ...`)
3. Démarrage rapide (validate/add/run/output/summary) + exemple ping
4. Référence YAML (v1): drafts avec `name` seul, champs par défaut, workflows exécutables avec steps, types de step (`command/sleep/wait_until/plugin`)
5. Référence CLI: *toutes* les commandes et flags
6. Schedules cron: `schedule:`, `flow schedule next`, `flow schedule workflow`, format des champs cron
7. Daemon: queue, status, stop/restart propre
8. Cancel: queued vs actif, kill process tree, events écrits
9. Plugins: manifest + schémas, règles d’exécution de `plugin test` (cwd/workspace/timeout/validation)
10. Packages (`.flowpkg`): build/install, checksum, emplacement
11. Rétention: dry-run/delete, keep-runs, older-than-days, ce qui est supprimé
12. Dépannage: erreurs typiques + où regarder (`logs/`, `.flow/`)
13. Release: tags + artefacts
14. Dev: fmt/clippy/test/build

Source de vérité pour la liste de commandes: `src/cli.rs`.
