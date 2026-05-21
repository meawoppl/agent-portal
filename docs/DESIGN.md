# Agent Portal — System Design

This document is a detailed architectural reference for **agent-portal** (also published as the **claude-portal** CLI). It is aimed at engineers who need to navigate, extend, or operate the system. Where this doc and the code disagree, the code wins — file paths in `backticks` are anchors back to the source.

If you only need a hand-wavy overview, read the project [README](../README.md) and [`docs/PROTOCOL.md`](PROTOCOL.md) first. This document goes deeper: component responsibilities, the typed-WebSocket spine, the database model, the agent abstraction, the permission / scheduling / voice subsystems, and the cross-cutting invariants that hold the whole thing together.

## Table of Contents

- [1. Goals & Non-Goals](#1-goals--non-goals)
- [2. Topology](#2-topology)
- [3. Workspace Layout](#3-workspace-layout)
- [4. WebSocket Spine](#4-websocket-spine)
  - [4.1 Typed endpoints](#41-typed-endpoints)
  - [4.2 At-least-once output flow](#42-at-least-once-output-flow)
  - [4.3 Replay semantics](#43-replay-semantics)
- [5. Agent Abstraction](#5-agent-abstraction)
- [6. Components](#6-components)
  - [6.1 `backend`](#61-backend)
  - [6.2 `proxy` (`claude-portal` CLI)](#62-proxy-claude-portal-cli)
  - [6.3 `launcher` (`agent-portal` daemon)](#63-launcher-agent-portal-daemon)
  - [6.4 `frontend`](#64-frontend)
  - [6.5 `session-lib` family](#65-session-lib-family)
  - [6.6 `shared`](#66-shared)
  - [6.7 `portal-auth` / `portal-update`](#67-portal-auth--portal-update)
- [7. Database Model](#7-database-model)
- [8. Authentication & Identity](#8-authentication--identity)
- [9. Permission Flow](#9-permission-flow)
- [10. Scheduled Tasks](#10-scheduled-tasks)
- [11. Image Upload Subsystem](#11-image-upload-subsystem)
- [12. Voice Subsystem](#12-voice-subsystem)
- [13. Update Subsystem](#13-update-subsystem)
- [14. Deployment Modes](#14-deployment-modes)
- [15. Cross-cutting Invariants](#15-cross-cutting-invariants)
- [16. Operational Notes](#16-operational-notes)
- [17. Glossary](#17-glossary)

---

## 1. Goals & Non-Goals

### Goals

- **Remote browser access to a CLI agent.** Run Claude Code or Codex on a powerful or specialised machine (GPU box, FPGA host, dev workstation, Jetson, etc.) and drive it from any browser, including phones.
- **Multi-viewer collaboration.** Any number of authenticated viewers can attach to a live session in real time.
- **Durable history.** Conversations survive page reloads, machine reboots, and proxy/launcher restarts.
- **Heterogeneous agents from one frontend.** Claude and Codex share the same dashboard, message log, permission dialog, and scheduler. Adding a third agent is a finite, well-scoped piece of work (see [§5](#5-agent-abstraction)).
- **Self-hostable.** A single backend binary + Postgres + an OAuth client is enough to stand up an instance. The hosted instance at `txcl.io` is the same code path as a self-hosted one.

### Non-Goals

- **Not a Claude CLI replacement.** The proxy *wraps* the upstream `claude` (or `codex`) binary; we do not reimplement model interaction. When the upstream CLI changes shape, we adapt at the proxy boundary.
- **Not an IDE.** The web UI is a session viewer + input box + dashboard, not an editor. VS Code integration is a sidecar (see [`docs/VSCODE_SETUP.md`](VSCODE_SETUP.md)).
- **Not an arbitrary remote shell.** Every channel is scoped to a single agent session; the protocol carries agent IO and permission decisions, not arbitrary `exec`.

---

## 2. Topology

```
  ┌──────────────────────┐                  ┌────────────────────────┐
  │ Browser (Yew/WASM)   │                  │   Dev machine          │
  │                      │                  │                        │
  │  dashboard / session │                  │  ┌──────────────────┐  │
  │  permission dialog   │                  │  │ claude-portal    │  │
  │  voice / image UI    │                  │  │ (proxy)          │  │
  └──────────┬───────────┘                  │  │                  │  │
             │  /ws/client                  │  │ spawns + wraps   │  │
             │  /ws/voice/{id}              │  │   ┌────────────┐ │  │
             │  /ws/image-upload            │  │   │ claude CLI │ │  │
             │  HTTP (REST + image GET)     │  │   └────────────┘ │  │
             ▼                              │  └────────┬─────────┘  │
  ┌──────────────────────┐  /ws/session     │           │            │
  │      Backend         │◄─────────────────┼───────────┘            │
  │  Axum + Diesel/PG    │                  └────────────────────────┘
  │                      │
  │  SessionManager      │  /ws/launcher    ┌────────────────────────┐
  │  routes messages,    │◄─────────────────┤  agent-portal launcher │
  │  enforces auth,      │                  │  (persistent daemon)   │
  │  persists history    │                  │                        │
  └──────────┬───────────┘                  │  - spawns proxies on   │
             │                              │    demand              │
             ▼                              │  - runs scheduled      │
        ┌────────┐                          │    tasks (cron)        │
        │  PG    │                          └────────────────────────┘
        └────────┘
```

Three WebSocket endpoints carry essentially all real-time traffic:

| Endpoint            | Who connects                  | Direction       | Typed message pair                 |
| ------------------- | ----------------------------- | --------------- | ---------------------------------- |
| `/ws/session`       | `proxy` (one per session)     | Bidirectional   | `ServerToProxy` / `ProxyToServer`  |
| `/ws/client`        | `frontend` (one per viewer)   | Bidirectional   | `ServerToClient` / `ClientToServer`|
| `/ws/launcher`      | `launcher` (one per machine)  | Bidirectional   | `ServerToLauncher` / `LauncherToServer` |

Plus two specialised channels with mixed binary/JSON framing:

- `/ws/voice/{session_id}` — browser → backend → Google STT, returns transcribed prompts.
- `/ws/image-upload` — chunked binary upload of large screenshots / paste-images.

HTTP routes carry OAuth callbacks, REST endpoints for admin / settings / scheduled tasks, and `GET /images/:hash` for inline image retrieval (which is cheaper than streaming a multi-MB blob back over the session WebSocket).

---

## 3. Workspace Layout

The repository is a single Cargo workspace at the top level. Each crate has a focused responsibility:

| Crate                  | Kind     | Role |
| ---------------------- | -------- | ---- |
| `shared`               | lib      | All wire-protocol types and constants. Compiled for both native and `wasm32-unknown-unknown` so the frontend reuses the exact structs the backend emits. |
| `backend`              | bin      | Axum web server: HTTP routes, the three `/ws/*` upgrades, OAuth, Diesel/Postgres, session routing, image store. |
| `frontend`             | bin (wasm)| Yew SPA. Pages: `dashboard`, `admin`, `settings`, `splash`, `banned`, `access_denied`. |
| `proxy`                | bin      | `claude-portal` CLI. Spawns the upstream `claude` binary (via `claude-session-lib`), wraps stdin/stdout, opens a `/ws/session` WebSocket, ferries messages bidirectionally. |
| `launcher`             | bin      | `agent-portal` CLI. Long-running daemon. Connects to `/ws/launcher`, accepts spawn requests, runs the cron scheduler for scheduled tasks, holds a process manager. |
| `session-lib`          | lib      | Agent-agnostic core: `Session<A>` state machine, output buffer, sequence-number bookkeeping, heartbeats, snapshots, IO traits. |
| `claude-session-lib`   | lib      | `Agent` impl for Claude (`ClaudeAgent`) — spawns the `claude` binary, parses its line-delimited JSON output via the [`claude-codes`](https://crates.io/crates/claude-codes) SDK. |
| `codex-session-lib`    | lib      | `Agent` impl for Codex — spawns `codex` and bridges its app-server JSON-RPC dialect. |
| `portal-auth`          | lib      | Shared auth helpers (token formats, header parsing) usable from both backend and proxy. |
| `portal-update`        | lib      | GitHub-releases-driven self-update helper, used by both proxy and launcher. |

Two key separations are worth calling out because they shape every other design decision:

1. **`session-lib` vs `*-session-lib`.** The agent-agnostic state machine lives in `session-lib`. Per-agent specifics (process spawning, output parsing, input encoding, permission semantics) live in `claude-session-lib` and `codex-session-lib`. The proxy is currently Claude-only (`type ClaudeSession = Session<ClaudeAgent>` at `proxy/src/main.rs:13`); the launcher is the dispatcher that can spawn either agent.

2. **`shared` is dependency-light.** It avoids any crate that doesn't compile on `wasm32`. This is what lets the frontend type-check every message it receives against the same structs the backend emits, and means a wire-format change is a single diff in `shared/` plus rebuilds on both sides.

---

## 4. WebSocket Spine

### 4.1 Typed endpoints

Every WebSocket endpoint is defined in `shared/src/endpoints.rs` via the `WsEndpoint` trait (re-exported from the [`ws-bridge`](https://crates.io/crates/ws-bridge) crate). For each endpoint we name the URL path and the two message enums:

```rust
impl WsEndpoint for SessionEndpoint {
    const PATH: &'static str = "/ws/session";
    type ServerToClient = ServerToProxy;   // backend -> proxy
    type ClientToServer = ProxyToServer;   // proxy -> backend
}
```

Both messages serialize as JSON with an externally-tagged `"type"` discriminant. Adding a new message variant is a single enum-variant addition in `shared/`, plus exhaustive `match` updates that the compiler flags on both ends.

The three pairs:

- `ServerToProxy` / `ProxyToServer` — agent IO, registration, permission requests/responses, session events.
- `ServerToClient` / `ClientToServer` — viewer input, scrollback replay, session subscription, dashboard updates.
- `ServerToLauncher` / `LauncherToServer` — spawn requests, scheduled-task syncing, launcher health.

Each pair is exhaustive — anything not in the enum cannot be sent over that endpoint.

### 4.2 At-least-once output flow

Agent output is the highest-volume traffic. Both the proxy → backend leg and the backend → frontend leg use **sequence-numbered, ack-driven, at-least-once** delivery so a transient disconnect on either side doesn't drop messages:

```
proxy                    backend                    frontend
  │                        │                          │
  ├ SequencedOutput{seq=N}─►│                          │
  │  (buffered in proxy)   ├ persist row              │
  │                        ├ fan out to viewers ─────►│
  │                        ├ store seq for replay     │
  │◄─ OutputAck{ack_seq=N}─┤                          │
  │  drop buffer slot N    │                          │
  │                        │                          │
  │                        │◄── Ack{ack_seq=N} ───────┤  (web client)
  │                        │   per-viewer cursor      │
```

Implementation:

- **Per-session output buffer** in `session-lib/src/output_buffer.rs`. Bounded by `MAX_PENDING_MESSAGES_PER_SESSION = 100` and aged out after `MAX_PENDING_MESSAGE_AGE_SECS = 300` (both in `shared/src/protocol.rs`).
- The proxy's buffer is drained on ack; the backend's `SessionManager` (`backend/src/handlers/websocket/session_manager.rs`) keeps a per-viewer cursor so a reconnecting frontend can request replay from any `seq`.
- **Heartbeats** in `session-lib/src/heartbeat.rs` keep idle connections alive and detect half-open TCP.
- **Snapshots** (`session-lib/src/snapshot.rs`) compress old buffered messages so a long-running session doesn't grow unboundedly in proxy memory.

### 4.3 Replay semantics

`Register` from a web client carries an optional `replay_after: Option<String>` (in `shared/src/endpoints.rs::RegisterFields`). The backend interprets it as "send me every persisted message strictly *after* this seq", which lets a reconnecting browser pick up exactly where it left off. Fresh tabs send `replay_after: None` and the backend replays whatever fits in the retention window (configured per-deployment via `message_retention_count` and `message_retention_days` on `AppState`).

---

## 5. Agent Abstraction

The shape of an agent — how it's spawned, how its output is parsed, how user input is encoded — is captured by the `Agent` trait in `session-lib/src/agent.rs`. A `Session<A: Agent>` (in `session-lib/src/session.rs`) is then generic over the agent type and contains all the cross-cutting concerns: output buffering, sequencing, heartbeat, snapshot, IO loop, error handling.

Two concrete implementations exist today:

- **`ClaudeAgent`** in `claude-session-lib/src/agent.rs`. Spawns the `claude` binary (`claude-session-lib/src/spawn.rs`), parses its line-delimited JSON output via the [`claude-codes`](https://crates.io/crates/claude-codes) SDK, encodes prompts as `claude_codes::ClaudeInput`.
- **`CodexAgent`** in `codex-session-lib/src/agent.rs`. Spawns the `codex` binary, bridges its app-server JSON-RPC dialect to the shared `SequencedOutput` shape via `codex-session-lib/src/handler.rs`.

The `AgentType` enum in `shared/src/lib.rs` is the wire tag that travels with every output message (`agent_type: "claude" | "codex"`). It is tagged at the proxy emission boundary on each message — not derived from the session registration — so a future multi-agent session could in principle multiplex over one `/ws/session` connection.

Adding a third agent is a contained piece of work:

1. New crate `xyz-session-lib` that implements `Agent`.
2. Variant `Xyz` in `shared::AgentType`.
3. Variant in the launcher's process-manager dispatch (`launcher/src/process_manager.rs`).
4. If the agent has its own permission shape, a new variant in `shared::CodexPermissionInput` (or a sibling enum).
5. Frontend renderer for any agent-specific message shapes.

There is intentionally no `Box<dyn Agent>` anywhere — the generic-over-`A` approach keeps the proxy zero-cost and lets the frontend treat each agent's IO with its own typed parser.

---

## 6. Components

### 6.1 `backend`

The Axum server. Entry point: `backend/src/main.rs`. Key state in `AppState`:

- `db_pool: DbPool` — r2d2-managed Diesel/Postgres pool.
- `session_manager: SessionManager` — in-memory routing table (`session_id → connected proxy + viewer set`), defined in `backend/src/handlers/websocket/session_manager.rs`.
- `oauth_basic_client: Option<BasicClient>` — Google OAuth client (None in dev mode).
- `device_flow_store: Option<DeviceFlowStore>` — OAuth device flow for headless launchers.
- `image_store: ImageStore` — in-memory dedup cache so large images go over HTTP, not the WebSocket.
- Tunables: `message_retention_count`, `message_retention_days`, `session_max_age_days`, `max_image_mb`.

Handler modules under `backend/src/handlers/`:

| Module                   | Responsibility |
| ------------------------ | --- |
| `websocket/proxy_socket.rs`     | `/ws/session` upgrade + IO loop |
| `websocket/web_client_socket.rs`| `/ws/client` upgrade + viewer fan-out |
| `websocket/launcher_socket.rs`  | `/ws/launcher` upgrade + spawn dispatch |
| `websocket/image_upload_socket.rs` | `/ws/image-upload` chunked binary |
| `websocket/registration.rs`     | Register/RegisterAck handshake |
| `websocket/permissions.rs`      | Permission request routing |
| `websocket/session_manager.rs`  | In-process routing table + per-session state |
| `auth.rs`                       | Google OAuth, cookie issuance, email allowlist |
| `device_flow.rs`                | Device authorization grant (RFC 8628) |
| `messages.rs`                   | REST CRUD over persisted messages |
| `scheduled_tasks.rs`            | REST CRUD over cron tasks |
| `launchers.rs`                  | REST CRUD over registered launcher tokens |
| `proxy_tokens.rs`               | REST CRUD over per-machine proxy tokens |
| `voice.rs`                      | `/ws/voice/{id}` Google STT bridge |
| `images.rs`                     | `GET /images/:hash`, dedup store |
| `retention.rs`                  | Background sweeper for session_max_age + message retention |
| `sound_settings.rs`             | Per-user notification-sound prefs |
| `session_access.rs`             | "Who can see this session" authorisation helpers |
| `admin.rs`                      | Admin-only routes (user management, bans) |
| `downloads.rs`                  | Binary download links (proxy / launcher artifacts) |
| `config.rs`                     | Frontend bootstrap config (`/api/config`) |
| `helpers.rs`                    | Small shared utilities |

`SessionManager` is the in-memory routing table. It is intentionally *not* persistent: if the backend restarts, all live WebSockets reconnect from scratch and the database supplies the replay tail. Persistence is reserved for messages, sessions, users, tokens, and scheduled-task configs.

### 6.2 `proxy` (`claude-portal` CLI)

Entry point: `proxy/src/main.rs`. The proxy is per-session: each invocation of `claude-portal` from a terminal owns one Claude CLI child process and one `/ws/session` connection.

Module roles:

| Module           | Role |
| ---------------- | --- |
| `auth.rs`        | Token storage (`~/.config/agent-portal/config.json`) and JWT exchange |
| `commands.rs`    | Subcommands: `--init <token-url>`, version, etc. |
| `config.rs`      | Loads / validates `ProxyConfig` (backend URL, per-cwd tokens, agent type) |
| `session.rs`     | Wires `Session<ClaudeAgent>` to the WebSocket; this is the IO loop |
| `shim.rs`        | TTY shim — what the user sees in their terminal while the proxy is running |
| `ui.rs`          | Terminal UI bits (spinner, banners) |
| `update.rs`      | Calls `portal-update` to fetch newer releases |
| `util.rs`        | Small helpers |

The shim's UX intent: from the user's terminal, `claude-portal` looks almost identical to `claude` — same prompt, same scrollback — with a banner showing the URL viewers can use to attach.

### 6.3 `launcher` (`agent-portal` daemon)

Entry point: `launcher/src/main.rs`. A persistent daemon installed once per machine. Holds a long-lived `/ws/launcher` connection and is the control plane that lets a remote browser say "spawn a new session in `~/repos/foo`".

Module roles:

| Module               | Role |
| -------------------- | --- |
| `config.rs`          | Launcher config (backend URL, auth token, display name, max concurrency) |
| `connection.rs`      | `/ws/launcher` connect loop with backoff |
| `process_manager.rs` | Spawns / supervises in-process agent sessions (Claude or Codex), bounded by `--max-sessions` |
| `scheduler.rs`       | Cron-driven trigger for scheduled tasks (see [§10](#10-scheduled-tasks)) |
| `service.rs`         | systemd / launchd service install/uninstall (`agent-portal service ...`) |
| `pastebin.rs`        | Posts large logs to a backend-hosted pastebin endpoint for sharing |

A launcher's `max-sessions` default is 20 — generous for a workstation, conservative for a CI box. Each launched session is an in-process task (not a separate process), so spawn cost is mostly the upstream agent binary startup.

### 6.4 `frontend`

A Yew (Rust → WASM) SPA. Pages under `frontend/src/pages/`:

- `splash.rs` — landing page / sign-in entry.
- `dashboard/` — main view: live session list + the per-session terminal with scrollback, prompt input, permission dialog, image upload, voice.
- `settings/` — user preferences (sound, default agent, etc.).
- `admin/` — user management, ban list, deployment-wide config.
- `access_denied.rs`, `banned.rs` — error pages for the auth/allowlist outcomes.

The dashboard's terminal view subscribes to a session over `/ws/client`, receives `SequencedOutput` messages, and renders them via per-agent parsers in `frontend/src/components/`. Voice and image-upload UIs open their own dedicated WebSocket connections so the main session stream is not blocked by large blob transfers.

### 6.5 `session-lib` family

Files in `session-lib/src/`:

| File              | Responsibility |
| ----------------- | --- |
| `agent.rs`        | The `Agent` trait — spawn, parse, encode. |
| `session.rs`      | `Session<A>` — the agent-agnostic state machine. |
| `buffer.rs`       | Generic bounded ring buffer used by `output_buffer.rs`. |
| `output_buffer.rs`| The seq-numbered outbox; entries hold `(seq, payload, inserted_at)`. |
| `heartbeat.rs`    | Heartbeat-tick task; backs out connections that stop responding. |
| `io.rs`           | IO traits abstracting over stdio / WebSocket. |
| `probe.rs`        | Liveness probes used by the launcher's process manager. |
| `snapshot.rs`     | Compresses long histories into a smaller replay payload. |
| `error.rs`        | Crate-wide error type with `thiserror`. |
| `lib.rs`          | Re-exports the public surface. |

This crate has no dependency on backend, frontend, or any specific agent SDK — only async runtime + serde + utilities. That's what lets the same code drive both `claude-session-lib` and `codex-session-lib`.

### 6.6 `shared`

The wire-protocol crate. Everything that crosses a process boundary lives here. Notable modules:

- `lib.rs` — `AgentType`, `SessionInfo`, `VoiceMessage`, re-exports of `claude-codes` types the frontend renders.
- `endpoints.rs` — `WsEndpoint` impls + `RegisterFields`, `PermissionResponseFields`, `FileUploadStartFields`, etc.
- `api.rs` — REST request/response shapes; typed `CodexPermissionInput` envelope (replaces what used to be `serde_json::Value` poking between proxy and frontend — closes #725, #731).
- `image_upload.rs` — `ImageUploadClientMsg` / `ImageUploadServerMsg` for the chunked binary protocol.
- `proxy_tokens.rs` — token shapes for the proxy / launcher auth surface.
- `protocol.rs` — constants only: cookie name, queue limits, backoff caps.

Discipline: this crate is `wasm32-unknown-unknown`-compatible. Any dep that pulls in tokio with non-default features, file-system access, or native TLS bumps the WASM bundle hard and gets rejected during review.

### 6.7 `portal-auth` / `portal-update`

Two small support crates:

- **`portal-auth`** — JWT / token-format helpers shared between backend and proxy so the same encode/decode is used on both sides.
- **`portal-update`** — Github-releases-based self-update. Resolves platform → asset URL, downloads, atomically replaces the running binary. Used by `agent-portal update` (launcher) and `claude-portal --update` (proxy).

---

## 7. Database Model

Postgres via Diesel. Schema in `backend/src/schema.rs`. Migrations live alongside in the standard Diesel `migrations/` directory.

Tables:

| Table                          | Purpose |
| ------------------------------ | --- |
| `users`                        | OAuth-authenticated identities. One row per email. |
| `sessions`                     | One row per agent session ever registered. Indexed by user. |
| `session_members`              | "Who can see this session" — owner + share-grants. |
| `messages`                     | Persisted `SequencedOutput`s. Indexed by `(session_id, seq)`. Subject to retention sweeps. |
| `pending_inputs`               | Queued prompts that arrived while the proxy was disconnected; replayed once it reconnects. |
| `pending_permission_requests`  | Open permission requests awaiting a viewer decision. Survive proxy/backend restarts. |
| `proxy_auth_tokens`            | Bearer tokens issued to specific machines for proxy connections. |
| `scheduled_tasks`              | Cron-shaped task configs synced down to launchers. |
| `deleted_session_costs`        | Aggregate cost accounting per user, retained after individual session rows are GC'd. |

Two design choices worth highlighting:

1. **Pending state in Postgres, live state in RAM.** The `SessionManager` deliberately doesn't persist its routing table. If a backend restarts, all live sockets reconnect, the launcher re-registers, the proxy re-registers, and the pending tables (`pending_inputs`, `pending_permission_requests`) drive replay. This keeps the hot path lock-free.

2. **Deletion-aware cost accounting.** Per-message rows can be aged out aggressively, but the aggregate spend per user is preserved in `deleted_session_costs` so the cost UI remains coherent after retention sweeps.

---

## 8. Authentication & Identity

Three coexisting auth surfaces:

| Surface       | Mechanism | Token form | Where verified |
| ------------- | --- | --- | --- |
| Web browser   | Google OAuth (or dev-mode bypass) | Session cookie `cc_session` (HMAC-signed via `tower_cookies::Key`) | `backend/src/handlers/auth.rs` |
| Proxy        | Pre-shared bearer token | `proxy_auth_tokens` row, issued from the web UI | `backend/src/handlers/websocket/proxy_socket.rs` |
| Launcher     | JWT issued at install time | `LAUNCHER_AUTH_TOKEN` env var | `backend/src/handlers/websocket/launcher_socket.rs` |

The OAuth device flow (`backend/src/handlers/device_flow.rs`) supports headless installs: the launcher prints a code, the user opens a URL on a browser, approves, and the launcher exchanges the code for a JWT.

Access control on top of authentication:

- **Email allowlist** (`AppState::allowed_emails`) — explicit list of approved addresses.
- **Email domain allowlist** (`AppState::allowed_email_domain`) — wildcard for org installs.
- **Ban list** — admin-controlled, drives `/banned` redirects.
- **Per-session ACL** — `session_members` rows say which users can subscribe to which sessions.

If neither allowlist is set, the backend defaults to "single user" mode where only the first OAuth-authenticated user can sign in. Dev mode bypasses all of this and auto-logs-in `testing@testing.local`.

---

## 9. Permission Flow

Both Claude and Codex emit permission requests for sensitive operations (file edits, shell commands, MCP elicitations, etc.). The flow is identical in shape across agents:

```
agent ──► proxy ──── PermissionRequest ───► backend ──► viewers (all subscribed)
                                              │           │
                                              ▼           ▼
                                       persisted to    rendered as
                                  pending_permission_  permission
                                       requests       dialog
                                              ▲           │
                                              │           ▼
                                              └─── PermissionResponse ◄── viewer click
proxy ◄──── PermissionResponse ──── backend ◄────────────────┘
  │
  └─► agent receives the decision and either proceeds or aborts
```

Per-agent shape lives in the `CodexPermissionInput` enum (`shared/src/api.rs`). Variants today: `FileChange`, `ApplyPatch`, `Bash`, `ExecCommand`, `Permissions`, `McpElicitation`, `AskUserQuestion`. Each variant carries the strongly-typed fields the dialog needs (item ID, command + cwd, file change set, MCP server name, the question list, etc.). This is a major refactor from the prior `serde_json::Value` envelope — both proxy-side write (#725) and frontend-side read (#731) are now compile-time-checked against a single discriminator.

The `pending_permission_requests` Postgres table guarantees that:

- A viewer can refresh the browser without losing an open permission dialog.
- A backend restart preserves outstanding requests until either the proxy disconnects (timeout) or a viewer responds.
- Multiple viewers see the same dialog and the first decision wins (the others' attempts no-op against the missing row).

---

## 10. Scheduled Tasks

Cron-shaped tasks: "run this prompt in this working directory on this cron schedule". Config lives in the `scheduled_tasks` table; runtime execution lives in the launcher.

```
admin / user ───► REST POST /api/scheduled_tasks ───► sched_tasks (PG)
                                                          │
                                                          │  ScheduleSync push
                                                          ▼
   ┌──────────────────────────────────────────────────────────────────┐
   │  Launcher                                                        │
   │                                                                  │
   │   /ws/launcher receives ScheduleSync ──► scheduler.rs            │
   │                                            │                     │
   │                                            ▼                     │
   │                                   cron tick fires                │
   │                                            │                     │
   │                                            ▼                     │
   │                              process_manager spawns              │
   │                              Session<ClaudeAgent | CodexAgent>   │
   │                                            │                     │
   │                                            ▼                     │
   │                              /ws/session attached as normal      │
   └──────────────────────────────────────────────────────────────────┘
```

A scheduled task carries `cron_expression`, `timezone`, `working_directory`, `prompt`, `claude_args`, `agent_type`, `max_runtime_minutes`, and a backreference to its `last_session_id`. See [`docs/SCHEDULED_TASKS.md`](SCHEDULED_TASKS.md) for the user-visible semantics.

The launcher is the only component that owns a cron-tick clock; the backend is purely the storage + sync layer. This keeps the backend stateless w.r.t. wall-clock timing and lets a launcher offline-spawn even if the backend is briefly down (the task config is cached in the launcher's last `ScheduleSync`).

---

## 11. Image Upload Subsystem

Large screenshots and clipboard images are common in agent prompts. Sending them over the per-session WebSocket would head-of-line-block real-time agent output, so we use a dedicated channel.

- **Endpoint**: `/ws/image-upload` (`backend/src/handlers/websocket/image_upload_socket.rs`).
- **Protocol**: defined in `shared/src/image_upload.rs`. Mixed JSON + binary frames — the client sends a JSON `StartUpload` with metadata, then a stream of binary chunks, then a JSON `FinishUpload`.
- **Storage**: deduplicated by content hash in `AppState::image_store` (in-memory).
- **Retrieval**: `GET /images/:hash` for inline `<img>` tags, instead of base64-stuffing into the chat stream.
- **Cap**: `max_image_mb` (default 10) — anything larger is rejected at the proxy emission boundary, since beyond that the model context budget is the real limit.

---

## 12. Voice Subsystem

The browser captures microphone audio via the Web Speech API, opens `/ws/voice/{session_id}`, and streams 16 kHz PCM frames up. The backend bridges to Google Speech-to-Text and returns interim + final transcripts. The final transcript becomes a text prompt routed into the same session input pipeline as a manually-typed prompt.

`VoiceMessage` is defined in `shared/src/lib.rs`. It's deliberately *not* one of the `WsEndpoint` typed enums because the channel mixes JSON control messages with binary audio frames — a shape `ws-bridge` doesn't model. The bridge module is `backend/src/handlers/voice.rs`; STT credentials come from `AppState::speech_credentials_path`.

---

## 13. Update Subsystem

Two self-update callers, one library (`portal-update`):

1. **`agent-portal update`** (launcher subcommand) — hits the GitHub Releases API, picks the right platform asset (`linux-aarch64`, `linux-x86_64`, `macos-aarch64`, etc.), downloads it, replaces the running binary in-place, and restarts the system service if one is registered.

2. **`claude-portal --update`** (proxy flag) — same logic, applied to the proxy binary.

The launcher's startup path also probes for an update if `--no-update` isn't set, so a stale install fixes itself the next time the daemon restarts.

`portal-update` is intentionally release-driven, not git-driven: a remote box never pulls source, never compiles. Builds happen in CI, binaries are shipped as GitHub Release assets, and the update is purely a download-and-swap.

---

## 14. Deployment Modes

The same codebase covers three deployments:

### 14.1 Single-user local

```
laptop = backend + proxy + browser
```

Run `./scripts/dev.sh start`. Dev mode bypasses OAuth, auto-logs you in as `testing@testing.local`. This is the default getting-started experience.

### 14.2 Self-hosted multi-user

```
server   = backend + Postgres + OAuth client
machines = proxy and/or launcher
browser  = anyone with allowlisted email
```

Configure Google OAuth, set `ALLOWED_EMAIL_DOMAIN` or `ALLOWED_EMAILS`, deploy the backend via `docker-compose.yml`. Each user's machines run a launcher (so they can spawn sessions from the browser) and/or a proxy per terminal session.

### 14.3 Hosted (txcl.io)

The reference public instance. Same code path as self-hosted; the only differences are the OAuth client and the email allowlist policy. See README's Security & Privacy section for the data-handling commitments.

---

## 15. Cross-cutting Invariants

These are the load-bearing rules; violating them in a PR means breaking something elsewhere.

1. **Wire types live in `shared/` only.** Adding a field on a `*Fields` struct in `backend/` is wrong — bump `shared/`, rebuild both.
2. **`shared/` must compile to `wasm32-unknown-unknown`.** No tokio runtime features, no fs, no native TLS in dependencies.
3. **Every output message carries an `agent_type` tag at emission time.** Don't derive it from registration; do it per-message at the proxy emission boundary so multi-agent multiplexing stays possible.
4. **Sequence numbers are monotonic per (direction, session).** Never reuse, never skip — replay logic depends on contiguity.
5. **The `SessionManager` routing table is not persistent.** Persist what matters in Postgres; let live state rebuild on reconnect.
6. **Permission requests round-trip through Postgres.** Both viewer-reload and backend-restart must survive without dropping a dialog.
7. **No JSON-poking across the proxy/frontend boundary.** Use typed enums (`CodexPermissionInput`, `RegisterFields`, …). If a field is currently `serde_json::Value`, that's a TODO, not a precedent.
8. **The proxy is per-session.** Do not add multi-session state to the proxy. The launcher is the per-machine actor.
9. **Image and voice traffic must not block session output.** Always use the dedicated `/ws/image-upload` and `/ws/voice/{id}` channels (or HTTP for image retrieval).
10. **`portal-update` is the only path that mutates a deployed binary.** No `cargo install` on remote machines, no source pulls — builds happen in CI, binaries ship via GitHub Releases.

---

## 16. Operational Notes

- **Logs**: `tracing` everywhere, JSON output in production (`RUST_LOG=info,tower_http=info` is the default).
- **Migrations**: `db::run_migrations` runs automatically at backend startup. New migrations are added in the standard Diesel layout and committed alongside the schema diff.
- **Retention sweeper**: `backend/src/handlers/retention.rs` background-deletes old messages and aged-out sessions per `message_retention_days` and `session_max_age_days`.
- **Rate limiting**: `tower_governor` with a smart-IP key extractor; the policy lives at the route layer in `backend/src/main.rs`.
- **CORS**: configured per-route; permissive in dev, locked-down to the configured `public_url` in production.
- **Dev script**: `scripts/dev.sh` is the canonical entry point for local-only experimentation — it brings up Postgres in Docker, starts the backend in dev mode, builds the frontend, and serves both.

---

## 17. Glossary

- **Agent** — the underlying coding-agent CLI (`claude` or `codex`) wrapped by a session.
- **Session** — one logical conversation with one agent in one working directory. Identified by a `Uuid`.
- **Proxy** — the `claude-portal` CLI instance that owns one session and one `/ws/session` connection.
- **Launcher** — the `agent-portal` daemon installed once per machine; spawns proxies on demand and runs scheduled tasks.
- **Viewer** — an authenticated browser tab subscribed to one or more sessions.
- **Replay** — the act of resending persisted messages to a reconnecting viewer or proxy.
- **Sequenced output** — agent output tagged with a monotonic per-session `seq` for at-least-once delivery.
- **Scheduled task** — a cron-shaped trigger that spawns a session on a recurring basis.
- **Permission request** — an in-band, persistable message asking a viewer to approve a sensitive operation before the agent proceeds.

---

## Further Reading

- [`docs/PROTOCOL.md`](PROTOCOL.md) — message-by-message WebSocket reference.
- [`docs/AUTH_FLOWS.md`](AUTH_FLOWS.md) — OAuth + device flow + cookie details.
- [`docs/DATABASE.md`](DATABASE.md) — table-by-table schema reference.
- [`docs/SCHEDULED_TASKS.md`](SCHEDULED_TASKS.md) — user-visible cron semantics.
- [`docs/OVERSEER.md`](OVERSEER.md) — the long-running observer that watches the system.
- [`docs/PROXY_AUTH.md`](PROXY_AUTH.md), [`docs/proxy-internals.md`](proxy-internals.md), [`docs/proxy-login-flow.md`](proxy-login-flow.md) — proxy-side specifics.
- [`docs/DEPLOYING.md`](DEPLOYING.md), [`docs/DOCKER.md`](DOCKER.md), [`docs/LOCAL_DEVELOPMENT.md`](LOCAL_DEVELOPMENT.md) — operator-facing guides.
- [`docs/CODEX_SUPPORT.md`](CODEX_SUPPORT.md) — Codex-specific implementation notes.
