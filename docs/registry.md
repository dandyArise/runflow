# RunFlow Registry v1

Le registry fournit une source de verite locale pour les plugins disponibles dans un projet RunFlow.

V1 indexe uniquement les plugins locaux declares dans :

```txt
plugins/*/plugin.yml
```

Le fichier genere est :

```txt
.flow/registry/plugins.json
```

Ce fichier est genere par RunFlow. Il ne doit pas etre edite a la main.

Decision v1 : `.flow/registry/plugins.json` est versionne dans les projets RunFlow.

Pourquoi :

- review plus lisible ;
- audit plus fiable ;
- contexte agent reproductible ;
- `registry_hash` exploitable en CI.

## Mode Operatoire Dev

Quand un plugin local est ajoute, modifie ou supprime :

1. Modifier uniquement les manifests source `plugins/*/plugin.yml`.
2. Ne jamais editer `.flow/registry/plugins.json` a la main.
3. Lancer :

```powershell
flow registry scan
```

4. Verifier le diff de `.flow/registry/plugins.json`.
5. Lancer :

```powershell
flow registry scan --check
flow validate .\workflow.yml
```

6. Committer ensemble :

```txt
plugins/<plugin>/plugin.yml
.flow/registry/plugins.json
workflows utilisant le plugin, si applicable
```

En CI, utiliser :

```powershell
flow registry scan --check
```

Cette commande doit echouer si le registry versionne n'est plus aligne avec les manifests locaux.

Pour ce repo, le registry peut rester vide tant qu'aucun plugin officiel n'est fourni.

## Manifest Plugin

Exemple :

```yaml
name: ssl_check
version: 0.1.0
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
```

Regles v1 :

- `name`, `version`, `description`, `runtime`, `entrypoint` sont obligatoires.
- `version` doit etre au format `major.minor.patch`.
- `runtime` accepte `python`, `node`, `shell`, `binary`.
- `entrypoint` doit exister dans le dossier du plugin.
- les noms de plugins doivent etre uniques.
- les permissions sont declaratives uniquement, non appliquees en v1.

## Contrat A Respecter

### Source De Verite

- La source de verite est toujours `plugins/*/plugin.yml`.
- `.flow/registry/plugins.json` est un artefact genere et versionne.
- Un consumer ne doit pas inventer de plugin absent de `plugins.json`.
- Un consumer ne doit pas inventer de chemin de script absent du registry.

### Contrat `plugins.json`

Chaque plugin contient :

```txt
name
version
description
runtime
entrypoint
manifest_path
plugin_dir
inputs
outputs
permissions
```

Contraintes :

- `entrypoint`, `manifest_path` et `plugin_dir` sont relatifs a la racine projet.
- les chemins utilisent `/`, meme sous Windows.
- `plugins` est trie par `name`.
- `registry_hash` ne depend que du contenu canonique de `plugins`.
- `generated_at` est informatif et ne doit pas servir a detecter un changement de contrat.

### Contrat Workflow

Un workflow qui utilise un plugin doit garder la syntaxe explicite :

```yaml
steps:
  - name: check
    type: plugin
    run:
      command: python
      args: ["plugins/my_plugin/run.py"]
    plugin_id: my_plugin
    input: {}
```

Contraintes :

- `type: plugin` est obligatoire.
- `plugin_id` doit exister dans `.flow/registry/plugins.json`.
- `input` doit etre un objet.
- les inputs requis doivent etre presents sauf si le manifest declare un `default`.
- les inputs non declares sont invalides.
- les types acceptes sont `string`, `integer`, `number`, `boolean`, `object`, `array`.

### Contrat Permissions

- Les permissions sont seulement declaratives en v1.
- `flow validate` ne bloque pas un workflow sur les permissions.
- L'UI, MCP et `runflow-agent` peuvent les afficher comme information.
- Aucun sandbox reel n'est implique par le registry v1.

### Contrat Agent/UI/MCP

Les consumers doivent utiliser :

```powershell
flow registry export --for-agent
```

Regles :

- utiliser uniquement les plugins listes ;
- ne pas inventer de tools ;
- retourner `needs_tool` si aucun plugin ne correspond ;
- ne pas generer de commande shell directe quand un plugin adapte existe.

## Commandes

Scanner les plugins :

```powershell
flow registry scan
flow registry scan --check
```

`--check` rescane les plugins, recalcule le registry attendu et echoue si `.flow/registry/plugins.json` n'est pas a jour. `generated_at` est ignore dans cette comparaison.

Lister :

```powershell
flow registry list plugins
flow registry list plugins --format json
```

Inspecter :

```powershell
flow registry inspect ssl_check
flow registry inspect ssl_check --format json
```

Exporter un contexte compact pour `runflow-agent` :

```powershell
flow registry export --for-agent
```

## Workflow

La syntaxe workflow reste explicite :

```yaml
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
```

`flow validate` utilise le registry si le workflow contient des steps `type: plugin`.

Verifications v1 :

- plugin inconnu ;
- registry manquant ;
- input requis absent ;
- type d'input incorrect ;
- input inconnu.

## Hash

`registry_hash` est calcule uniquement a partir du champ `plugins`, trie par `name`.

Il exclut :

- `generated_at` ;
- `registry_hash`.

Ainsi, deux scans sans changement de plugin gardent le meme hash.

Canonicalisation :

- JSON compact, sans indentation ;
- cles d'objets triees ;
- plugins tries par `name` ;
- chemins normalises avec `/` ;
- aucune dependance au separateur Windows/Linux.

Si aucun plugin n'existe, `flow registry scan` genere un registry vide avec `plugins: []`. Ce n'est pas une erreur.

## Limites V1

- pas de registry remote ;
- pas de marketplace ;
- pas de sandbox reel ;
- pas de policies globales ;
- pas de capabilities globales ;
- permissions affichees uniquement, jamais appliquees.
