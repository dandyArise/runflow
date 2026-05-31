# RunFlow Documentation Plan (FR/EN)

Ce fichier est une checklist de couverture pour la doc GitHub Pages (`docs/index.html`). Objectif: ne rien oublier (toutes les commandes + comportements).

---

## EN (Coverage Checklist)

1. What is RunFlow (concepts: workflow/job/run/step/event/manifest)
2. Install/build (source build, binary path, optional PATH, `cargo run -- ...`)
3. Quick start (validate/add/run/output/summary) + ping example
4. Workflow YAML reference (v1): top-level fields + step types (`command/sleep/wait_until/plugin`)
5. CLI reference: list *every* command and flags
6. Daemon behavior: queue, status, clean stop/restart
7. Cancel behavior: queued vs active run, process-tree kill, events written
8. Plugins: manifest + schemas, `plugin test` execution rules (cwd/workspace/timeout/validation)
9. Packages (`.flowpkg`): build/install, checksum, install location
10. Retention: dry-run/delete, keep-runs, older-than-days, what is deleted
11. Troubleshooting: common errors + where to look (`logs/`, `.flow/`)
12. Release: tagging + artifacts
13. Development: fmt/clippy/test/build

Canonical source of truth for the CLI list: `src/cli.rs` enums (`Command`, `JobCommand`, `RunCommand`, etc.).

---

## FR (Checklist de couverture)

1. C’est quoi RunFlow (concepts: workflow/job/run/step/event/manifest)
2. Installer/build (depuis les sources, binaire, PATH, `cargo run -- ...`)
3. Démarrage rapide (validate/add/run/output/summary) + exemple ping
4. Référence YAML (v1): champs top-level + types de step (`command/sleep/wait_until/plugin`)
5. Référence CLI: *toutes* les commandes et flags
6. Daemon: queue, status, stop/restart propre
7. Cancel: queued vs actif, kill process tree, events écrits
8. Plugins: manifest + schémas, règles d’exécution de `plugin test` (cwd/workspace/timeout/validation)
9. Packages (`.flowpkg`): build/install, checksum, emplacement
10. Rétention: dry-run/delete, keep-runs, older-than-days, ce qui est supprimé
11. Dépannage: erreurs typiques + où regarder (`logs/`, `.flow/`)
12. Release: tags + artefacts
13. Dev: fmt/clippy/test/build

Source de vérité pour la liste de commandes: `src/cli.rs`.

