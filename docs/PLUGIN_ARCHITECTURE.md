# Agent Portal Plugin Architecture

Status: planning spec.

This document defines Portal plugins as repo-adjacent capabilities installed by
the `agent-portal` launcher. A plugin gives a session a domain-specific work
surface, agent instructions, and optional local tools without baking that domain
into Agent Portal core.

The motivating example is Backplane: a PCB workflow should appear beside chat as
a polished workbench, but the PCB-specific parser, viewer, KiCad automation,
and agent guidance should live outside this repo.

## Goals

- Install plugins with the `agent-portal` CLI.
- Store plugin checkouts under a predictable repo-folder relationship:
  `agent-portal-plugins/<plugin-name>`.
- Let plugins provide dockable web apps that host cleanly in the session split
  surface.
- Let agents discover and use plugin-provided skills, prompts, CLIs, HTTP APIs,
  and MCP servers.
- Keep plugin execution local to the launcher host by default, because that host
  owns the checkout, tools, credentials, filesystem, and forwarded ports.
- Keep Agent Portal core generic: it should know how to install, start, stop,
  surface, and permission a plugin, but not how to design a PCB or run a
  notebook.

## Non-Goals

- Plugins are not arbitrary frontend bundles injected into the Portal UI.
- Plugins do not get direct database access.
- Plugins do not run inside the backend process.
- Plugins do not bypass existing session permissions, auth, or forwarding.
- The first version does not need a marketplace, signed registry, or automatic
  binary sandbox. The manifest should leave room for those.

## Definition

A Portal plugin is an installed directory containing a manifest plus any
combination of:

- a local web app or daemon that serves a session surface;
- command-line tools the agent can run;
- MCP servers or other typed tool providers;
- skills and instructions for agents;
- project detection rules;
- setup, health, and teardown commands;
- static assets or templates.

The smallest useful plugin is just instructions. The common rich plugin is a
binary that serves a website on localhost and registers that port as the
session's active surface.

## Filesystem Layout

The launcher owns a plugin root next to normal source checkouts:

```text
~/agent-portal-plugins/
  backplane/
    agent-portal-plugin.toml
    README.md
    bin/
      backplane
    skills/
      pcb-workflow/SKILL.md
    prompts/
      session.md
    assets/
    examples/
```

The default root is:

```text
$HOME/agent-portal-plugins
```

It can be overridden by:

```text
AGENT_PORTAL_PLUGIN_ROOT=/path/to/plugins
```

The folder name is the installation name. The manifest's `name` must match the
folder name. This keeps paths stable, makes manual inspection easy, and avoids a
hidden package-manager cache becoming the source of truth.

Plugin commands run with a plugin-local support home rooted inside that same
installed directory:

```text
~/agent-portal-plugins/backplane/.portal/
  home/
  cache/
  config/
  data/
  state/
```

Before invoking any manifest command, the launcher creates those directories and
sets:

```text
AGENT_PORTAL_PLUGIN_DIR=$AGENT_PORTAL_PLUGIN_ROOT/backplane
AGENT_PORTAL_PLUGIN_HOME=$AGENT_PORTAL_PLUGIN_ROOT/backplane/.portal
HOME=$AGENT_PORTAL_PLUGIN_ROOT/backplane/.portal/home
XDG_CACHE_HOME=$AGENT_PORTAL_PLUGIN_ROOT/backplane/.portal/cache
XDG_CONFIG_HOME=$AGENT_PORTAL_PLUGIN_ROOT/backplane/.portal/config
XDG_DATA_HOME=$AGENT_PORTAL_PLUGIN_ROOT/backplane/.portal/data
XDG_STATE_HOME=$AGENT_PORTAL_PLUGIN_ROOT/backplane/.portal/state
```

Installers and plugin runtimes should keep downloaded tools, generated support
files, caches, and per-plugin state under these paths. They may still read the
session repository through `{cwd}`, and they may write explicit user-requested
outputs through `{artifact_dir}` or another path supplied by the user. This is a
best-effort convention rather than a sandbox: commands can still write elsewhere
if they ignore `HOME`, `XDG_*`, and the Portal plugin environment.

This plugin-local `.portal/` is different from a repository's `.portal/`
directory. The repository directory describes project intent, such as suggested
plugins. The installed plugin directory stores plugin-owned support files.

## CLI Surface

Add a top-level launcher command group:

```console
agent-portal plugin list
agent-portal plugin search <query>
agent-portal plugin install <source> [--name <name>] [--ref <git-ref>]
agent-portal plugin update [<name>]
agent-portal plugin remove <name>
agent-portal plugin info <name>
agent-portal plugin doctor [<name>]
agent-portal plugin enable <name>
agent-portal plugin disable <name>
agent-portal plugin start <name> [--session <session-id>] [--cwd <path>]
agent-portal plugin stop <name> [--session <session-id>]
agent-portal plugin open <name> [--session <session-id>]
```

Install sources:

```console
agent-portal plugin install github:i2cjak/backplane
agent-portal plugin install github:meawoppl/agent-portal-plugins//backplane
agent-portal plugin install https://github.com/i2cjak/backplane.git
agent-portal plugin install https://github.com/meawoppl/agent-portal-plugins.git//backplane
agent-portal plugin install ./local-backplane
agent-portal plugin install ./agent-portal-plugins/backplane
```

`install` clones or copies into `agent-portal-plugins/<name>`, validates the
manifest, records the install in the launcher config, and runs the plugin's
declared setup command after explicit confirmation when setup requires network,
credential, or package-manager access.

Git and local sources may point either at a plugin root or at a plugin
subdirectory using `//sub/path`. The subdirectory form is the smooth path for a
shared plugin collection repo:

```text
github:meawoppl/agent-portal-plugins//backplane
```

The launcher clones the source repo into an internal source cache, validates the
manifest at the selected subdirectory, then materializes that plugin at:

```text
$AGENT_PORTAL_PLUGIN_ROOT/backplane
```

The installed plugin path is still one directory per plugin; the source may be a
monorepo.

`open` starts the plugin if needed, registers its web surface through the normal
forwarding path, and asks the focused session to open that surface.

## Manifest

Every plugin has `agent-portal-plugin.toml` at its root.

```toml
schema_version = 1
name = "backplane"
display_name = "Backplane"
description = "PCB review, KiCad checks, Gerber export, and board visualization."
homepage = "https://github.com/i2cjak/Backplane"
license = "MIT"

[compat]
agent_portal = ">=2.15.0"
platforms = ["linux", "macos"]

[install]
setup = "scripts/setup.sh"
doctor = "bin/backplane doctor --json"

[surface]
kind = "http"
default_title = "Backplane"
default_width_percent = 50
health_path = "/healthz"
start = "bin/backplane serve --port {port} --session {session_id} --cwd {cwd}"
ready_url = "http://127.0.0.1:{port}/"

[surface.env]
AGENT_PORTAL_PLUGIN = "backplane"

[[commands]]
name = "drc"
description = "Run design-rule checks for the active board."
run = "bin/backplane drc --cwd {cwd} --json"

[[commands]]
name = "export-gerbers"
description = "Export fabrication artifacts for review."
run = "bin/backplane export gerbers --cwd {cwd} --out {artifact_dir}"

[[skills]]
name = "pcb-workflow"
path = "skills/pcb-workflow/SKILL.md"
agents = ["claude", "codex"]

[[prompts]]
name = "session"
path = "prompts/session.md"

[[detect]]
name = "kicad"
any = ["*.kicad_pro", "*.kicad_pcb", "*.kicad_sch"]

[[detect]]
name = "gerber-package"
any = ["*.gbr", "*.gtl", "*.gbl", "*.drl", "*.zip"]

[[mcp]]
name = "backplane"
command = "bin/backplane"
args = ["mcp", "--cwd", "{cwd}"]
```

## Agent Skills

Plugin skills are repository-local instruction bundles declared with
`[[skills]]`. The `path` points at a `SKILL.md` inside the installed plugin and
`agents` optionally narrows which agent families should use it. Agents should
prefer the skill's `name` and `description` frontmatter when present; the
manifest name is the fallback identifier.

Installed skills are visible with:

```console
agent-portal plugin skills
agent-portal plugin skills kicad-pcb
```

Claude has native skill loading. When the launcher spawns a Claude session, it
materializes a per-session, skills-only Claude plugin view under the Portal
config directory and adds `--plugin-dir <generated-plugin-root>` once for each
enabled plugin with Claude-applicable skills. Each generated root contains only
`skills/<skill-name>/...` entries from the plugin manifest, preserving the
source skill directory by symlink where possible so relative references still
work. The generated directory is removed when that session task exits. This
intentionally does not pass the full Portal plugin root to Claude: future
`hooks/`, `.mcp.json`, `commands/`, or `agents/` files in a Portal plugin must
not silently become Claude runtime behavior for every session.

Codex and Muse do not currently get plugin roots through a native launcher hook,
so the launcher adds a compact plugin-skill list to the existing Portal
system-reminder for those agents. The reminder names each skill as
`plugin:skill`, includes the frontmatter description when available, and gives
the absolute `SKILL.md` path. The list is filtered by the session's agent type,
so Codex-only skills are not shown to Muse and Muse-only skills are not shown to
Codex. Agents must read the file completely before using it. If Codex grows a
stable `--plugin-dir` equivalent, prefer that native path and keep the reminder
as a fallback only.

Placeholders are expanded by the launcher:

| Placeholder | Meaning |
| --- | --- |
| `{plugin_dir}` | Absolute plugin install directory |
| `{cwd}` | Session working directory |
| `{session_id}` | Portal session UUID |
| `{port}` | Launcher-assigned localhost port |
| `{artifact_dir}` | Session-scoped scratch/artifact directory |
| `{backend_url}` | Portal backend URL known to the launcher |

Expansion is argument-aware: command arrays are preferred in the implementation
so paths and user-controlled values are not interpolated through a shell.

## Repository Config

Repository-owned Portal config lives in `.portal/` at the project root. This is
where a repo says which plugins it prefers, how they should be configured for
that checkout, and what workflow defaults agents should follow.

```text
project/
  .portal/
    plugins.toml
    prompts/
      onboarding.md
    workflows/
      pcb-review.md
```

The first config file is `.portal/plugins.toml`:

```toml
schema_version = 1

[[suggested_plugins]]
name = "backplane"
source = "github:i2cjak/backplane"
reason = "This repo contains KiCad board files and uses Backplane for PCB review."
required = false

[suggested_plugins.config]
project = "hardware/controller.kicad_pro"
default_view = "board"
fabrication_output = "build/fab"

[[suggested_plugins]]
name = "pastebom"
source = "github:meawoppl/pastebom-agent-plugin"
reason = "Manufacturing packages in releases/ should be inspectable in Portal."
required = false
```

Semantics:

- `.portal/plugins.toml` is repo intent, not installed code.
- `name` matches the plugin install name under `agent-portal-plugins/<name>`.
- `source` tells `agent-portal plugin install` where to fetch a missing plugin.
- `reason` is user-facing text shown in the suggestion/installer UI.
- `required = true` means the repo's workflow expects the plugin, but install
  still requires user confirmation.
- `suggested_plugins.config` is plugin-specific structured config. Portal stores
  and passes it through; the plugin validates the keys it understands.

The launcher reads `.portal/plugins.toml` before manifest glob detection. A
matching repo config should produce a stronger suggestion than inferred file
patterns because it is explicit project intent.

## Runtime Model

Plugins run on the launcher host.

```text
browser
  |
  | Portal session split surface
  v
backend forward origin
  |
  | existing tunnel data plane
  v
launcher/proxy host
  |
  | localhost:{port}
  v
plugin web service
```

The launcher is responsible for:

- choosing a free loopback port;
- starting the plugin service with the session cwd and session id;
- checking readiness via `surface.health_path` or TCP connect;
- registering the port using the existing forward protocol;
- stopping the service when the session ends, the plugin is closed, or the
  plugin crashes past its restart policy;
- reporting plugin status to the session UI and agent reminder.

The backend remains a transport and auth boundary. It does not spawn plugin
processes and does not inspect plugin-specific state.

## Session Surface Contract

A plugin surface should behave like a well-mannered iframe app:

- serve over HTTP on loopback;
- work under its own forwarded origin;
- avoid assuming a path prefix;
- handle being resized between roughly 30% and 70% width;
- provide a compact header/title inside the app only when useful, because Portal
  already supplies surface chrome;
- preserve state across iframe visibility changes;
- expose deep links for board/schematic/artifact views;
- provide a health endpoint;
- avoid service workers unless they are scoped carefully and tolerate forward
  origin rotation.

Portal surfaces plugin apps through the same `SessionSurface` machinery used by
ordinary forwarded apps. The first implementation can model plugin surfaces as
annotated forwards:

```text
SessionSurfaceKind::Forward {
  plugin_name: Option<String>
  title
  url
  port
  process
}
```

A later implementation can add plugin-specific surface kinds only if the generic
forward shape blocks real workflows.

## Agent Contract

Plugins provide agent guidance, not just UI. The launcher should collect enabled
plugin instructions for the active session and expose them through the same
system-reminder path used for local portal affordances.

Plugin guidance should tell the agent:

- when the plugin applies;
- how to open the surface;
- what commands/tools are available;
- what checks mean;
- what artifacts prove work is complete;
- what user approvals are required;
- how to summarize plugin output in PRs.

For Backplane, the guidance should say things like:

- detect KiCad and Gerber projects;
- open the Backplane surface before explaining visual board state;
- run DRC/ERC before declaring PCB work complete;
- export Gerbers/BOM/position files into an artifact directory;
- show rendered images or the live surface rather than only textual claims;
- treat publishing manufacturing files to external services as explicit user
  approval territory.

## Discovery and Activation

Installed plugins can activate in three ways:

1. **Manual**: user or agent runs `agent-portal plugin open backplane`.
2. **Repo-suggested**: launcher finds `.portal/plugins.toml` in the session cwd
   or an ancestor and exposes install/open suggestions for listed plugins.
3. **Detected**: launcher sees manifest `detect` rules matching the session cwd
   and exposes a suggested surface chip.
4. **Requested by agent**: the agent follows plugin instructions and asks the
   launcher to start the plugin.

Suggestion priority:

1. `.portal/plugins.toml` entry for the plugin;
2. installed plugin manifest `detect` rule;
3. agent request based on plugin guidance;
4. manual command.

Detection should be cheap and local. It should not run expensive setup, network
calls, package-manager installs, or heavyweight scans. Deep analysis belongs to
the plugin after user or agent activation.

Suggested plugins are represented separately from active plugin services:

```json
{
  "name": "backplane",
  "source": "github:i2cjak/backplane",
  "installed": true,
  "enabled": true,
  "reason": "This repo contains KiCad board files and uses Backplane for PCB review.",
  "confidence": "explicit",
  "config_source": ".portal/plugins.toml"
}
```

## Installed State

The launcher config stores install metadata:

```json
{
  "plugins": {
    "backplane": {
      "path": "/home/alice/agent-portal-plugins/backplane",
      "source": "https://github.com/i2cjak/Backplane.git",
      "source_subdir": ".",
      "ref": "main",
      "enabled": true,
      "installed_at": "2026-09-22T01:00:00Z"
    }
  }
}
```

Runtime state is per session and ephemeral:

```json
{
  "session_id": "...",
  "plugin": "backplane",
  "pid": 12345,
  "port": 43817,
  "status": "starting|ready|failed|stopped",
  "surface_url": "http://...",
  "last_error": null
}
```

Do not store runtime state in the backend database until a frontend or API needs
cross-device visibility beyond the existing forward status.

## Security and Permissions

Installing a plugin means trusting code on the launcher host. The UX should make
that explicit.

Required permission boundaries:

- `install` displays source, resolved path, setup command, and requested
  capabilities before running setup.
- Setup and update commands that execute plugin code require confirmation.
- Plugin web services bind to `127.0.0.1`, not public interfaces.
- Plugin surfaces are exposed through the existing authenticated forwarder.
- Plugins do not receive the user's Portal cookie.
- Plugins receive only the environment variables declared by the launcher plus
  manifest `surface.env`, not the launcher's whole environment by default.
- Destructive commands, external publishing, credential writes, and package
  installation should route through existing permission dialogs when initiated
  by an agent.
- Plugin command output shown to the user should be treated as untrusted text.

Future hardening can add signed manifests, registry attestations, process
sandboxing, capability grants, and per-plugin secret stores.

## Updates

`agent-portal plugin update` fetches the configured source and checks out the
requested ref. Git sources may point at a whole plugin repo or a subdirectory
inside a plugin collection repo. The first version should support Git sources
only. Update should:

1. stop active plugin services;
2. fetch the source;
3. show old and new revisions;
4. validate the manifest at `source_subdir`;
5. rerun setup only if the manifest declares setup or dependencies changed, or
   if the user passes `--setup`;
6. restart services that were previously active.

Pinned refs should not move unless the user asks. Branch refs can move.

## Backplane Mapping

Backplane should be packaged as:

```text
agent-portal-plugins/backplane/
  agent-portal-plugin.toml
  bin/backplane
  skills/pcb-workflow/SKILL.md
  prompts/session.md
```

Its service owns:

- KiCad project detection beyond the cheap manifest globs;
- board/schematic/Gerber/BOM parsing;
- DRC/ERC/check/export commands;
- a polished web workbench for the Portal split surface;
- artifact directories and preview images;
- any PasteBOM-compatible import/export bridge.

Agent Portal owns:

- installation;
- lifecycle;
- auth and forwarding;
- opening/closing/resizing the surface;
- transcript messages and permissions;
- routing plugin guidance into the active agent.

This gives PCB work the feel of a native Portal ability without making the
Portal backend or frontend PCB-aware.

## Implementation PR DAG

```text
P0 docs/spec
 |
 +-- P1 manifest parser + plugin root config
 |    |
 |    +-- P2 `agent-portal plugin list/info/install/remove`
 |    |    |
 |    |    +-- P3 setup/doctor/update lifecycle
 |    |
 |    +-- P4 session plugin runtime state
 |         |
 |         +-- P5 start/stop/open surface via existing forwarder
 |              |
 |              +-- P6 frontend suggested plugin surface chips
 |              |
 |              +-- P7 agent reminder injection for installed skills/prompts
 |                   |
 |                   +-- P8 MCP/tool command registration
 |
 +-- P9 Backplane plugin package spike
      |
      +-- P10 Backplane workbench surface
      |
      +-- P11 PCB workflow skills and verification commands
```

## Acceptance Criteria

The first complete plugin slice is done when:

- `agent-portal plugin install github:i2cjak/backplane` installs into
  `~/agent-portal-plugins/backplane`;
- `agent-portal plugin list` shows it enabled;
- a KiCad repo causes the launcher to suggest Backplane for that session;
- `agent-portal plugin open backplane` starts the Backplane web service,
  forwards it, and opens it in the session split surface;
- the agent receives Backplane instructions for that session;
- `agent-portal plugin doctor backplane` reports missing KiCad or other local
  dependencies clearly;
- removing the plugin stops running services and deletes only the plugin
  checkout after confirmation.
