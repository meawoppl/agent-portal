# Writing Agent Portal Plugins

This guide is for people building a plugin. It explains what goes on disk, what
abilities a plugin can expose, how agents see those abilities, and how to keep a
plugin well-behaved inside a session.

For the platform architecture and lifecycle rationale, see
[`PLUGIN_ARCHITECTURE.md`](PLUGIN_ARCHITECTURE.md).

## What A Plugin Is

A plugin is a local directory installed by the launcher under:

```text
$HOME/agent-portal-plugins/<plugin-name>/
```

It contains an `agent-portal-plugin.toml` manifest plus any local runtime code,
skills, scripts, assets, examples, or toolchain installers the plugin owns. A
plugin may be tiny, with only a `SKILL.md`, or rich, with a local HTTP workbench
that opens beside chat.

Plugins extend sessions through these abilities:

| Ability | What It Gives The User | How Agents Use It |
| --- | --- | --- |
| Skills | Domain instructions and procedures | Claude receives native skill roots; Codex and Muse receive plugin skill reminders with `SKILL.md` paths |
| Surface | A docked web pane beside chat | Agent or user runs `agent-portal plugin open <name>` |
| Commands | Plugin-owned CLI operations | Agents inspect `runtime --json` and run narrow plugin commands |
| Toolchains | Managed local dependencies | Agents or users run `agent-portal plugin setup <name> [toolchain]` and `doctor` |
| Prompts/assets | Reusable templates and support files | Skills point agents at the relevant files |
| Detection | Repo hints and cheap file matching | Portal can suggest installing or opening the plugin |
| MCP/apps | Typed tool providers, when declared | Future or plugin-specific tool surfaces can expose them |

Agent Portal owns install, lifecycle, forwarding, permissions, and session UI.
The plugin owns its domain model, tools, workflow instructions, and web app.

## Minimal Layout

```text
agent-portal-plugins/
  esp32-fpga/
    agent-portal-plugin.toml
    README.md
    bin/
      esp32-fpga
    skills/
      workflow/
        SKILL.md
      firmware/
        SKILL.md
    prompts/
      bringup.md
    assets/
    examples/
```

The install directory name is the plugin name. Keep the manifest `name` equal
to that directory name.

## Plugin-Local State

Every plugin command runs with a plugin-local home under the install directory:

```text
agent-portal-plugins/<plugin-name>/.portal/
  home/
  cache/
  config/
  data/
  state/
    surfaces/
  toolchains/
```

The launcher creates those directories before invoking manifest commands and
sets:

```text
AGENT_PORTAL_PLUGIN_DIR=/home/alice/agent-portal-plugins/esp32-fpga
AGENT_PORTAL_PLUGIN_HOME=/home/alice/agent-portal-plugins/esp32-fpga/.portal
AGENT_PORTAL_PLUGIN_TOOLCHAIN_ROOT=/home/alice/agent-portal-plugins/esp32-fpga/.portal/toolchains
HOME=/home/alice/agent-portal-plugins/esp32-fpga/.portal/home
XDG_CACHE_HOME=/home/alice/agent-portal-plugins/esp32-fpga/.portal/cache
XDG_CONFIG_HOME=/home/alice/agent-portal-plugins/esp32-fpga/.portal/config
XDG_DATA_HOME=/home/alice/agent-portal-plugins/esp32-fpga/.portal/data
XDG_STATE_HOME=/home/alice/agent-portal-plugins/esp32-fpga/.portal/state
```

Installers should place downloaded SDKs, generated metadata, caches, and service
state inside those directories. They may still read the session checkout through
`{cwd}` and write explicit outputs requested by the user.

This is a convention, not a hard sandbox. If a third-party installer ignores
`HOME` and `XDG_*`, document that behavior and keep the command behind an
explicit setup step.

## Manifest Example

`agent-portal-plugin.toml` is the plugin contract:

```toml
schema_version = 1
name = "esp32-fpga"
display_name = "ESP32/FPGA"
description = "Bring-up tools, HDL/firmware workflows, board viewers, and lab notes."
homepage = "https://github.com/meawoppl/agent-portal-plugins"
license = "MIT"

[compat]
agent_portal = ">=2.14.1547"
platforms = ["linux", "macos"]

[install]
setup = "bin/esp32-fpga setup --json"
doctor = "bin/esp32-fpga doctor --json"

[surface]
kind = "http"
default_title = "ESP32/FPGA"
default_width_percent = 50
health_path = "/healthz"
start = "bin/esp32-fpga serve --port {port} --session {session_id} --cwd {cwd}"
stop = "bin/esp32-fpga stop --session {session_id}"
ready_url = "http://127.0.0.1:{port}/"

[surface.env]
ESP32_FPGA_MODE = "portal"

[[commands]]
name = "doctor"
description = "Check installed toolchains and project layout."
run = "bin/esp32-fpga doctor --cwd {cwd} --json"

[[commands]]
name = "build-firmware"
description = "Build firmware for the selected board."
run = "bin/esp32-fpga firmware build --cwd {cwd} --json"

[[commands]]
name = "synthesize"
description = "Run FPGA synthesis for the selected target."
run = "bin/esp32-fpga fpga synth --cwd {cwd} --json"

[[toolchains]]
name = "esp-idf"
description = "Managed ESP-IDF checkout and Python environment."
home = ".portal/toolchains/esp-idf"
install = "bin/esp32-fpga setup esp-idf --prefix {toolchain_home}"
doctor = "bin/esp32-fpga doctor esp-idf --prefix {toolchain_home} --json"

[[toolchains]]
name = "oss-cad-suite"
description = "Managed open FPGA toolchain."
home = ".portal/toolchains/oss-cad-suite"
install = "bin/esp32-fpga setup oss-cad-suite --prefix {toolchain_home}"
doctor = "bin/esp32-fpga doctor oss-cad-suite --prefix {toolchain_home} --json"

[[skills]]
name = "workflow"
path = "skills/workflow/SKILL.md"
agents = ["claude", "codex"]

[[skills]]
name = "firmware"
path = "skills/firmware/SKILL.md"
agents = ["claude", "codex"]

[[prompts]]
name = "bringup"
path = "prompts/bringup.md"

[[detect]]
name = "esp-idf"
any = ["sdkconfig", "sdkconfig.defaults", "idf_component.yml"]

[[detect]]
name = "fpga"
any = ["*.sv", "*.v", "*.xdc", "*.pcf"]

[capabilities]
domain = "embedded-development"
surface = "lab-workbench"
toolchains = ["esp-idf", "oss-cad-suite"]
```

## Placeholders

Manifest command strings may use these placeholders:

| Placeholder | Meaning |
| --- | --- |
| `{plugin_dir}` | Absolute plugin install directory |
| `{plugin_home}` | Plugin `.portal/` support directory |
| `{toolchain_root}` | Plugin `.portal/toolchains/` directory |
| `{toolchain_home}` | Selected toolchain's home for `setup <name> <toolchain>` |
| `{cwd}` | Session working directory |
| `{session_id}` | Portal session id when known |
| `{port}` | Launcher-assigned loopback port |
| `{artifact_dir}` | Session-scoped scratch/artifact directory |
| `{backend_url}` | Portal backend URL known to the launcher |

Keep commands thin. Prefer forwarding unknown subcommand arguments to the
plugin-owned tool so the tool can evolve without forcing Portal manifest churn.

## Skills

A skill is a directory containing `SKILL.md`, plus any supporting scripts,
templates, or reference files that `SKILL.md` points to.

```text
skills/
  firmware/
    SKILL.md
    references/
      esp-idf-layout.md
    scripts/
      inspect-sdkconfig.py
```

Use frontmatter so agents get a useful name and short description:

```markdown
---
name: firmware
description: Build, flash, and debug ESP-IDF firmware projects.
---

# Firmware Workflow

Use this skill when the user asks for ESP32 firmware work...
```

Write skills as operational procedure, not marketing copy. Include:

- when the skill applies;
- what files to inspect first;
- what commands to run;
- which outputs prove success;
- how to use the plugin surface;
- what needs user approval;
- what not to change.

Agent exposure:

- **Claude**: the launcher creates a per-session, skills-only Claude plugin root
  and passes it with `--plugin-dir`. Only declared skills are included.
- **Codex and Muse**: the launcher inserts a compact Plugin Skills section into
  the session reminder. It includes `plugin:skill`, the description, and the
  absolute `SKILL.md` path. The agent must read the file before using it.

Do not assume the full plugin directory is injected into an agent runtime.
Always declare each skill in the manifest.

## Surfaces

A surface is a local HTTP app that Portal opens beside chat through the normal
forwarding path.

Surface requirements:

- bind to `127.0.0.1`;
- serve a health endpoint;
- tolerate forwarded origins and path changes;
- preserve state across tab visibility and pane resizing;
- keep app state inside the plugin service or browser, not in Portal backend;
- avoid service workers unless their scope is carefully constrained.

### Surface Edit Stacks

Forwarded surfaces may hand a stack of visual/text annotations back to the
active session by posting JSON to the Portal-owned endpoint on the forward
origin: `POST /__portal/edit-stack`. The forward proxy intercepts that path
before the request reaches the plugin service, stores each annotation in the
durable session edit stack, enqueues the submitted item(s) into the active agent
session, and broadcasts an updated work-queue snapshot to the chat view. The
manual session-scoped endpoints below can still be used to stage, inspect,
dismiss, or send queue items from Portal UI and agent tooling.

This is intentionally domain-neutral. Plugins should put their own coordinates,
view names, slide numbers, waveform cursors, or other loose context in
`source`/`context`; Portal stores and displays that JSON and injects it into the
agent prompt when the user sends an item.

Use this request shape:

```js
await fetch("/__portal/edit-stack", {
  method: "POST",
  headers: { "content-type": "application/json" },
  credentials: "same-origin",
  body: JSON.stringify({
    source: {
      plugin: "my-plugin",
      page: location.href,
    },
    items: [
      {
        title: "Short human-readable title",
        body: "User note or speech transcript",
        context: {
          tab: "PCB",
          rect: { x: 0.2, y: 0.1, w: 0.3, h: 0.4 },
        },
        image: {
          dataUrl: "data:image/png;base64,...",
        },
      },
    ],
  }),
});
```

Keep payloads small and self-contained. Prefer a normalized region, current
view/project identifiers, and an optional cropped image over full application
state. Portal bounds the number of items, text size, JSON context size, and
image data URL size before saving the stack. A `401` response means the
forward's Portal session cookie is missing or stale; reopen the forward from
the session before submitting annotations.

Portal exposes the same state through session-scoped REST endpoints:

| Method | Path | Meaning |
| --- | --- | --- |
| `GET` | `/api/sessions/{id}/edit-stack` | List non-deleted stack items visible to the current user |
| `POST` | `/api/sessions/{id}/edit-stack` | Add one or more items using the message shape above |
| `PATCH` | `/api/sessions/{id}/edit-stack/{item_id}` | Mark an item `pending`, `sent`, or `dismissed` |
| `DELETE` | `/api/sessions/{id}/edit-stack/{item_id}` | Dismiss an item from the visible stack |

Agent-side tooling can use the bearer-token equivalents:
`GET /api/agent/sessions/{id}/edit-stack` and
`POST /api/agent/sessions/{id}/edit-stack`. The bundled CLI exposes those as
`agent-portal work-queue list [session-id]` and
`agent-portal work-queue add <session-id> "task"`.

The backend enforces normal session reader/mutator permissions. Items keep the
creator id and display name; Portal only shows contributor names in the compact
chat panel when there is more than one contributor in the pending stack.

### Surface voice transcription

A forwarded surface can turn a recorded voice note into text with the Portal's
speech-to-text provider by posting the recording to
`POST /__portal/stt/transcribe` on the forward origin. The forward proxy
intercepts it the same way as the edit stack: the `portal_fwd` cookie
identifies the user, and the forward's own session supplies vocabulary hints,
so there is no `session_id` parameter. The body is the raw recording as the
browser produced it (`MediaRecorder` output is fine) with its `audio/*`
Content-Type; an optional `language` query parameter carries a BCP-47 tag. The
reply is `{"text": "…"}`.

```js
const blob = new Blob(chunks, { type: recorder.mimeType });
const response = await fetch("/__portal/stt/transcribe?language=en-US", {
  method: "POST",
  headers: { "content-type": blob.type },
  credentials: "same-origin",
  body: blob,
});
const { text } = await response.json();
```

Responses: `401` stale forward cookie (reopen the forward from the session),
`405` for anything but `POST`, `400` for a missing or non-audio Content-Type or
an empty body, `413` over the server's per-recording cap, `503` when no speech
provider is configured (fall back to the browser's Web Speech API if you have
one), `502` when the provider failed. Portals older than this endpoint answer
`404`, which a plugin should treat like `503`.

The launcher owns the lifecycle:

1. choose a free port;
2. expand placeholders in `[surface].start`;
3. start the process;
4. wait for `health_path` or TCP readiness;
5. register the port with the existing forwarder;
6. open the session pane.

Use `agent-portal plugin status <name> --json` to inspect the stored process,
port, health, and surface metadata. Runtime state lives in:

```text
.portal/state/surfaces/surface.json
```

## Commands

Commands are named entry points for agents and users. They should be stable,
machine-readable wrappers around plugin-owned CLIs.

Good command behavior:

- accept `--json` for structured output;
- emit clear nonzero failures;
- include the exact underlying command in diagnostic JSON when useful;
- avoid interactive prompts unless the command is explicitly a setup command;
- keep destructive operations behind obvious subcommands and user approval.

The manifest should expose common, discoverable verbs. The plugin binary can
still support richer pass-through subcommands.

Set `accepts_args = true` on a command to let the user pass extra arguments
when Portal runs it. Each argument is shell-quoted and appended to `run`, so a
thin pass-through entry can expose the whole plugin CLI. It defaults to
`false`, which runs `run` exactly as written.

```toml
[[commands]]
name = "esp32-fpga"
description = "Run any esp32-fpga subcommand."
run = "bin/esp32-fpga"
accepts_args = true
```

Useful inspection commands:

```console
agent-portal plugin runtime esp32-fpga --json
agent-portal plugin doctor esp32-fpga
agent-portal plugin setup esp32-fpga
agent-portal plugin setup esp32-fpga esp-idf
```

## Toolchains

Toolchains describe local dependencies managed by the plugin, such as KiCad,
ESP-IDF, OSS CAD Suite, Verilator, OpenOCD, or a Python environment.

Declare a `[[toolchains]]` entry when a dependency is large, version-sensitive,
or benefits from plugin-local installation.

Toolchain installers should:

- install into `{toolchain_home}` or another path under `{toolchain_root}`;
- respect the plugin-local `HOME` and `XDG_*` environment;
- provide a `doctor` command that reports exact versions and missing pieces;
- avoid mutating global system state unless the user explicitly asks;
- produce actionable errors when a platform is unsupported.

Use global package managers only as a deliberate fallback, not as the default
place to hide plugin state.

## Repository Suggestions

Projects can suggest plugins with `.portal/plugins.toml`:

```toml
schema_version = 1

[[suggested_plugins]]
name = "esp32-fpga"
source = "github:meawoppl/agent-portal-plugins//esp32-fpga"
reason = "This repo contains ESP32 firmware and FPGA gateware."
required = false

[suggested_plugins.config]
default_board = "boards/devkit"
firmware_dir = "firmware"
gateware_dir = "gateware"
```

This file is repo intent, not installed code. It tells Portal what to suggest
and gives plugins project-specific config. The plugin should validate any config
it consumes.

Prefer `.portal/plugins.toml` over hidden plugin-specific dotfiles when the goal
is "this repo works best with these Portal abilities."

## Authoring Workflow

1. Create the plugin directory under `agent-portal-plugins/<name>`.
2. Add `agent-portal-plugin.toml`.
3. Add at least one declared skill.
4. Add a `doctor` command that can run before setup and explain missing tools.
5. Add setup commands only for work that must install or download dependencies.
6. If the plugin has a UI, add a loopback HTTP surface with `/healthz`.
7. Install locally:

   ```console
   agent-portal plugin install ./agent-portal-plugins/esp32-fpga
   agent-portal plugin info esp32-fpga
   agent-portal plugin runtime esp32-fpga --json
   agent-portal plugin skills esp32-fpga
   agent-portal plugin doctor esp32-fpga
   ```

8. Open it from a session:

   ```console
   agent-portal plugin open esp32-fpga
   ```

9. Test with the agents that should use it. Confirm Claude receives native
   skills and Codex/Muse receive the Plugin Skills reminder.

### Validating and previewing

Check a plugin directory before installing it. Neither command installs the
plugin or runs any of its commands.

```console
agent-portal plugin validate ./agent-portal-plugins/esp32-fpga
agent-portal plugin preview ./agent-portal-plugins/esp32-fpga --cwd ~/src/my-board
```

`validate` reports `error:` and `warning:` lines and exits non-zero only on
errors: a missing or unparsable manifest, an invalid `name`, missing skill or
prompt files, duplicate command or skill names, an empty command `run`, a
`[surface]` without `start`, a `default_width_percent` outside 10-90, a
`[[detect]]` with an empty `any`, or an unknown `{placeholder}`. Warnings cover
unknown keys, missing `homepage`/`license`/`[compat]`/`install.doctor`, a
surface without `health_path`, unknown skill `agents`, skills without
frontmatter `name`/`description`, and detect patterns that are too broad
(`*.json`, `README*`, ...) or can never match (detection compares file names
only, so a pattern containing `/` never fires). Add `--json` for
`{ ok, errors: [...], warnings: [...] }`, each item `{ field, message }`.

`preview` renders what Portal will show: the plugin card, skills with their
frontmatter descriptions and byte/estimated-token sizes, prompts, commands
(and whether they accept arguments), surface dock metadata, and the total
context estimate. With `--cwd` it reports whether the `[[detect]]` rules would
suggest the plugin in that directory and why, using the same matching as the
Portal backend. It ends with the `validate` findings. `--json` prints the same
data as JSON.

## Runtime JSON Expectations

`agent-portal plugin runtime <name> --json` is the stable machine-readable view
for agents and future UI. It should include enough information for an agent to
decide:

- where the plugin is installed;
- which plugin-local directories exist;
- what surface can be opened;
- what skills are declared;
- what commands are available;
- what toolchains are managed;
- what capabilities and repo suggestions apply.

Agents should prefer this command over scraping the manifest directly.

## Security Expectations

Installing a plugin means trusting local code. Author plugins as if every setup
step deserves review.

- Make network/package-manager work explicit in setup commands.
- Bind web services to loopback.
- Do not expect Portal cookies or backend secrets.
- Treat user repositories as untrusted input.
- Escape any text rendered into plugin HTML.
- Do not upload manufacturing, firmware, logs, or credentials to external
  services without explicit user approval.
- Keep generated outputs predictable and under project or artifact directories.

## Checklist

Before publishing a plugin:

- `agent-portal plugin validate <path>` reports no errors.
- `agent-portal plugin runtime <name> --json` is useful to an agent.
- `agent-portal plugin doctor <name>` reports actionable diagnostics.
- Skills are declared and have frontmatter descriptions.
- Setup keeps downloads and caches under `.portal/`.
- Surface starts, reports healthy, opens in a split pane, and stops cleanly.
- Commands return structured JSON for success and failure.
- Repo suggestions are documented with `.portal/plugins.toml`.
- README explains install, setup, doctor, open, and common workflows.
