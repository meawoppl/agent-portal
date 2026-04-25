# Overseer: Cross-Session Agent Observer

## Overview

The Overseer is a meta-agent that watches all active Claude Code sessions across all connected machines, provides running commentary to the user, and can intervene in sessions by injecting messages, answering permissions, or pausing runaway agents.

The core design principle is **no new tools**. The Overseer is a regular Claude Code session that reads session state from a directory tree on disk. The backend materializes live session data as files. Claude's existing Read, Grep, Glob, and Bash tools work unchanged.

## Goals

1. **Situational awareness** - The user has a single place to understand what all their agents are doing across all machines, without tabbing between sessions.

2. **Conflict detection** - When two sessions edit the same file, touch the same git branch, or duplicate work, the Overseer notices and alerts the user.

3. **Cost visibility** - Real-time spend tracking across all sessions with anomaly detection (spend velocity spikes, runaway loops).

4. **Intervention** - The user can ask the Overseer to pause a session, inject a message into a session, answer pending permissions, or ask a running agent a question. The user can also grant the Overseer autonomy to make some of these decisions on its own.

5. **Narrative** - Instead of raw log data, the Overseer provides a human-readable thread of what's happening. "Session A finished the auth refactor ($0.43, 14 turns). Session B is still running tests — 3 failures remain, it's on its 4th retry of the flaky integration test."

## Non-Goals

- The Overseer does not replace the per-session UI. Users still interact with individual sessions directly for normal work.
- The Overseer does not orchestrate multi-agent workflows from scratch. It observes and intervenes in sessions that already exist. (Orchestration is a separate feature that could build on this infrastructure.)
- The Overseer is not a security boundary. It runs with the same permissions as the user.

## Architecture

### Data Flow

```
Machine A (launcher + proxies) ──WSS──┐
Machine B (launcher + proxies) ──WSS──┤
Machine C (launcher + proxies) ──WSS──┼──▶ Backend
                                      │      │
                                      │      ├── Global Event Bus (broadcast channel)
                                      │      │        │
                                      │      │        ▼
                                      │      │   File Writer Task
                                      │      │        │
                                      │      │        ▼
                                      │      │   /tmp/agent-sessions/  (tmpfs)
                                      │      │        │
                                      │      │        ▼
                                      │      │   Overseer (Claude Code session)
                                      │      │        │
                                      │      │        ├── reads files (standard tools)
                                      │      │        ├── writes to action files → backend picks up
                                      │      │        └── commentary → Overseer WS → frontend panel
                                      │      │
                                      │      └── Existing per-session WS (unchanged)
                                      │
                                      └───────────────── Frontend (existing session views)
                                                            │
                                                            └── Overseer Panel (new sidebar/panel)
```

### Key Insight: No New Protocols

Every proxy on every machine already connects to the central backend via WebSocket. All session output, metadata, costs, and status already flow through the backend. The Overseer doesn't need direct access to any proxy — it reads what the backend already knows.

The session state is materialized as plain files on a tmpfs. The Overseer is a Claude Code session launched with `--add-dir /tmp/agent-sessions`. It uses Read, Grep, Glob — tools it already has. No MCP server, no FUSE mount, no custom tool definitions.

## Component Design

### 1. Global Event Bus

A `tokio::sync::broadcast` channel on `SessionManager` that publishes every session event to all subscribers.

**Location:** `backend/src/handlers/websocket/session_manager.rs`

**Change:** Add one field to `SessionManager`:

```rust
pub struct SessionManager {
    // ... existing fields ...

    /// Global broadcast channel for all session events.
    /// Subscribers receive (session_id, event_type, raw_json) for every
    /// message from every session. Used by the file writer, metrics,
    /// audit logging, and the Overseer.
    pub global_tx: tokio::sync::broadcast::Sender<SessionEvent>,
}

/// An event published to the global bus
#[derive(Clone, Debug)]
pub struct SessionEvent {
    pub session_id: Uuid,
    pub session_key: String,
    pub event_type: SessionEventType,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub payload: serde_json::Value,
}

#[derive(Clone, Debug)]
pub enum SessionEventType {
    /// Claude output (assistant, user, system, result, error, etc.)
    Output,
    /// Session registered (proxy connected)
    Connected,
    /// Session disconnected
    Disconnected,
    /// Permission request from agent
    PermissionRequest,
    /// Permission response from user
    PermissionResponse,
    /// User sent input
    UserInput,
    /// Session metadata changed (git branch, PR, etc.)
    MetadataUpdate,
    /// Cost/token update
    CostUpdate,
}
```

**Integration point:** In `handle_claude_output()` at `backend/src/handlers/websocket/message_handlers.rs`, after the existing `broadcast_to_web_clients` call, add:

```rust
session_manager.global_tx.send(SessionEvent {
    session_id,
    session_key: key.clone(),
    event_type: SessionEventType::Output,
    timestamp: chrono::Utc::now(),
    payload: content.clone(),
});
```

Similar one-liners at session connect/disconnect, permission request/response, and user input handlers.

**Backpressure:** `tokio::sync::broadcast` drops old messages for slow consumers. This is the correct behavior — the file writer doesn't need perfect delivery, it needs recent state. Channel capacity of 4096 messages is plenty.

### 2. Session State File Tree

A background tokio task subscribes to the global event bus and writes session state to `/tmp/agent-sessions/` (or a configurable path).

**Directory structure:**

```
/tmp/agent-sessions/
├── index.json                              # Summary of all sessions
├── {session-id}/
│   ├── info.json                           # Session metadata (updated on change)
│   ├── messages.jsonl                      # Full message log (append-only)
│   ├── recent.jsonl                        # Last 100 messages (rewritten periodically)
│   ├── cost.json                           # Current cost/token snapshot
│   ├── tools.json                          # Tool usage summary
│   ├── files.json                          # Files read/written/edited
│   ├── permissions.json                    # Pending + recent permission requests
│   ├── git.json                            # Branch, PR URL, repo URL
│   └── status                              # Single line: "active", "idle 3m", "awaiting-input", "awaiting-permission"
```

**File contents:**

#### `index.json`

```json
{
  "sessions": [
    {
      "id": "550e8400-e29b-41d4-a716-446655440000",
      "name": "refactor-auth",
      "status": "active",
      "model": "claude-opus-4-6",
      "hostname": "dev-laptop",
      "working_directory": "/home/user/myproject",
      "branch": "meawoppl/refactor-auth",
      "cost_usd": 0.43,
      "duration_seconds": 342,
      "last_activity": "2026-04-25T10:32:15Z"
    }
  ],
  "total_active": 3,
  "total_cost_usd": 1.27,
  "updated_at": "2026-04-25T10:32:15Z"
}
```

#### `info.json`

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "name": "refactor-auth",
  "status": "active",
  "hostname": "dev-laptop",
  "working_directory": "/home/user/myproject",
  "agent_type": "claude",
  "client_version": "2.1.117",
  "model": "claude-opus-4-6",
  "branch": "meawoppl/refactor-auth",
  "pr_url": "https://github.com/user/repo/pull/42",
  "repo_url": "https://github.com/user/repo",
  "launcher_id": "...",
  "created_at": "2026-04-25T10:25:00Z",
  "last_activity": "2026-04-25T10:32:15Z",
  "user_email": "matt@exclosure.io"
}
```

#### `cost.json`

```json
{
  "total_cost_usd": 0.43,
  "input_tokens": 125000,
  "output_tokens": 8500,
  "cache_read_tokens": 95000,
  "cache_creation_tokens": 30000,
  "turns": 14,
  "updated_at": "2026-04-25T10:32:15Z"
}
```

#### `tools.json`

Derived by parsing `tool_use` content blocks from the message stream:

```json
{
  "tool_calls": 47,
  "by_tool": {
    "Edit": { "calls": 12, "errors": 0 },
    "Read": { "calls": 18, "errors": 0 },
    "Bash": { "calls": 8, "errors": 2 },
    "Grep": { "calls": 5, "errors": 0 },
    "Glob": { "calls": 3, "errors": 0 },
    "Write": { "calls": 1, "errors": 0 }
  },
  "updated_at": "2026-04-25T10:32:15Z"
}
```

#### `files.json`

Derived by parsing tool inputs for file_path fields:

```json
{
  "read": ["src/auth.rs", "src/main.rs", "Cargo.toml"],
  "edited": ["src/auth.rs", "src/handlers/login.rs"],
  "written": ["src/auth/middleware.rs"],
  "most_touched": "src/auth.rs",
  "updated_at": "2026-04-25T10:32:15Z"
}
```

#### `permissions.json`

```json
{
  "pending": [
    {
      "request_id": "req_123",
      "tool_name": "Bash",
      "input_summary": "cargo test --workspace",
      "requested_at": "2026-04-25T10:32:10Z"
    }
  ],
  "recent": [
    {
      "tool_name": "Edit",
      "input_summary": "src/auth.rs",
      "decision": "allowed",
      "decided_at": "2026-04-25T10:31:45Z"
    }
  ],
  "total_allowed": 23,
  "total_denied": 1
}
```

#### `messages.jsonl`

Raw Claude output, one JSON object per line. Appended on each `SessionEventType::Output` event. This is the full unfiltered stream — the Overseer can grep through it for specific patterns.

```jsonl
{"type":"system","subtype":"init","model":"claude-opus-4-6","tools":["Read","Edit","Bash",...]}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"I'll start by..."}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"src/auth.rs"}}]}}
```

#### `recent.jsonl`

Same format as `messages.jsonl` but only the last 100 messages. Rewritten every 30 seconds or on session completion. This is what the Overseer reads most often — recent context without loading the full history.

#### `status`

A single line of text, designed for `cat` or quick `Read`:

```
active — editing src/auth.rs (turn 14, $0.43)
```

Or:

```
awaiting-input — result after 14 turns ($0.43)
```

Or:

```
awaiting-permission — Bash: cargo test --workspace
```

**Write frequency:**

| File | Updated when | Frequency |
|------|-------------|-----------|
| `index.json` | Any session change | Every few seconds |
| `info.json` | Session metadata changes | On connect, metadata update, disconnect |
| `messages.jsonl` | Every output message | Per message (append) |
| `recent.jsonl` | Periodically | Every 30 seconds |
| `cost.json` | Result messages or cost updates | Per result message |
| `tools.json` | Tool use detected in output | Per tool_use content block |
| `files.json` | File path detected in tool input | Per tool_use with file_path |
| `permissions.json` | Permission request/response | On each event |
| `git.json` | Session metadata update | On branch/PR change |
| `status` | Status change or activity | Every few seconds |

**Cleanup:** When a session disconnects or is deleted, its directory is removed after a configurable grace period (default 1 hour) so the Overseer can review completed sessions.

### 3. Action Files (Write Interface)

The Overseer interacts with sessions by writing to specific files. The file writer task watches these with `inotify` (Linux) or polling and translates writes into backend actions.

```
/tmp/agent-sessions/{session-id}/
├── _inject                    # Write text → send as SequencedInput to session
├── _interrupt                 # Write anything → send Interrupt to session
├── _permission                # Write "allow" or "deny" → answer pending permission
```

**`_inject`:**

The Overseer writes a text message. The file writer reads it, sends it as `ServerToProxy::SequencedInput` to the proxy, and truncates the file. The message appears in the session as if the user typed it, but tagged with an "Overseer" sender badge.

```
# Overseer does:
Write /tmp/agent-sessions/{id}/_inject "Please pull the latest changes from main before continuing."
```

**`_interrupt`:**

Any write triggers an `ServerToProxy::Interrupt` to the session's proxy. Used to stop a runaway session.

**`_permission`:**

Write `allow` or `deny` to respond to the pending permission request. Optionally `allow-remember` to also persist the permission rule.

**Alternative: HTTP instead of action files.**

The action files approach has the advantage of using Claude's existing Write tool. But it requires `inotify` watching and has race conditions (what if two writes happen before the watcher processes the first?).

A simpler alternative: the Overseer uses `Bash` to call `curl` against the backend's existing REST API:

```bash
# Inject input
curl -X POST http://localhost:3000/api/sessions/{id}/input -d '{"text": "pull latest changes"}'

# Interrupt
curl -X POST http://localhost:3000/api/sessions/{id}/stop

# Answer permission
curl -X POST http://localhost:3000/api/sessions/{id}/permission -d '{"decision": "allow"}'
```

This uses existing endpoints (or trivial new ones) and avoids the filesystem write-watching complexity entirely. The Overseer already has Bash access.

**Recommendation:** Start with the HTTP/curl approach. Add action files later if the Overseer's system prompt benefits from the filesystem metaphor.

### 4. Overseer Claude Session

The Overseer is a Claude Code session managed by the backend. It runs as a child process (via `claude-session-lib`) with a specific configuration.

**Launch configuration:**

```
claude --add-dir /tmp/agent-sessions
       --system-prompt <overseer-system-prompt>
       --model claude-haiku-4-5       # cheap model for observation
       --max-budget-usd 1.00          # cost cap per observation cycle
       --output-format json
```

**System prompt (summarized):**

```
You are the Overseer — a meta-agent observing all active Claude Code sessions
for the user. Your job is to provide situational awareness, detect problems,
and intervene when asked.

The /tmp/agent-sessions/ directory contains live state for all running sessions.
Each subdirectory is a session. Key files:
- index.json: summary of all sessions
- {id}/status: one-line current state
- {id}/recent.jsonl: last 100 messages
- {id}/cost.json: spend and token counts
- {id}/files.json: files being read/edited
- {id}/tools.json: tool usage patterns
- {id}/permissions.json: pending permissions

You should:
1. Periodically check index.json for new/completed sessions
2. Read status files to understand what each session is doing
3. Watch for conflicts (two sessions editing the same file)
4. Watch for anomalies (high cost velocity, repeated errors, idle sessions)
5. Provide concise, useful commentary — not a firehose of updates

To intervene in a session, use curl via Bash:
- Inject a message: curl -X POST http://localhost:3000/api/overseer/inject/{session-id} -d '{"text":"..."}'
- Interrupt: curl -X POST http://localhost:3000/api/overseer/interrupt/{session-id}
- Answer permission: curl -X POST http://localhost:3000/api/overseer/permission/{session-id} -d '{"decision":"allow"}'

Only intervene when asked by the user or when you detect a clear problem
(file conflict, runaway cost, obvious error loop). Default to commentary.
```

**Lifecycle:**

The Overseer session starts when the user enables it (toggle in the UI) and runs continuously. It polls the file tree on its own cadence — there's no push mechanism. Claude naturally reads files, thinks, comments, then reads again.

If the Overseer's context fills up, it compacts like any other session. The file tree is always fresh regardless of the Overseer's internal context state, so compaction doesn't lose external state.

**Cost control:**

The Overseer should be cheap. Using Haiku with a budget cap keeps costs proportional to value. The Overseer's job is pattern recognition and natural language commentary, not complex coding — Haiku is well-suited for this.

Expected cost: $0.01-0.05 per hour of observation, depending on activity level and polling frequency.

### 5. Frontend Panel

A collapsible sidebar or bottom panel in the dashboard that shows the Overseer's commentary thread and accepts user input.

**WebSocket endpoint:** `/ws/overseer`

The Overseer session's output is forwarded to connected web clients via a dedicated WebSocket. The protocol reuses `ServerToClient::ClaudeOutput` — same format as regular sessions. The frontend renders it in a simpler, chat-like format (no tool blocks, just text and actions).

**UI elements:**

- **Commentary thread** — A scrolling list of Overseer messages. Rendered as simple text with timestamps, not full message blocks. Color-coded by type: observation (gray), warning (amber), action taken (blue), error (red).
- **Input box** — The user types here to talk to the Overseer. "What's session 3 doing?" / "Pause the auth refactor" / "Approve all pending bash permissions"
- **Session links** — When the Overseer mentions a session, it's a clickable link that navigates to that session's view.
- **Mode toggle** — Advisory (commentary only) / Active (can intervene with user confirmation) / Autonomous (acts on its own judgment)
- **Enable/disable toggle** — Starts or stops the Overseer session. When disabled, the file writer task still runs (zero cost), but no Claude session is consuming the data.

**Mock/design reference:**

```
┌─────────────────────────────────────────────┐
│ Overseer                          [Advisory ▾] [✕] │
├─────────────────────────────────────────────┤
│                                             │
│ 10:25  Sessions: 3 active on 2 machines     │
│                                             │
│ 10:27  "fix-tests" completed — $0.18,       │
│        8 turns, all tests passing            │
│                                             │
│ 10:30  ⚠ "refactor-auth" and "add-logging" │
│        both editing src/middleware.rs        │
│                                             │
│ 10:32  "refactor-auth" hit rate limit,      │
│        waiting 2m. Cost so far: $0.43       │
│                                             │
│ 10:35  "add-logging" finished — $0.31       │
│        "refactor-auth" resumed, reading the │
│        file that "add-logging" just changed  │
│        → should it pull those changes?       │
│                                             │
├─────────────────────────────────────────────┤
│ > yes, inject a message telling it to read  │
│   the latest version                        │
│                                        [Send] │
└─────────────────────────────────────────────┘
```

## Implementation Plan

### Phase 1: Global Event Bus

Add `tokio::sync::broadcast` channel to `SessionManager`. Publish events from existing output, connect, disconnect, permission, and input handlers. This is ~50 lines of code and affects no existing behavior.

**Files changed:**
- `backend/src/handlers/websocket/session_manager.rs` — Add `global_tx` field, `SessionEvent` types
- `backend/src/handlers/websocket/message_handlers.rs` — Publish output events
- `backend/src/handlers/websocket/proxy_socket.rs` — Publish connect/disconnect events
- `backend/src/handlers/websocket/web_client_socket.rs` — Publish user input events
- `backend/src/handlers/websocket/permissions.rs` — Publish permission events

### Phase 2: File Writer Task

A background tokio task that subscribes to the global event bus and writes the file tree to tmpfs. Parses tool_use blocks to derive `tools.json` and `files.json`. Maintains `index.json` and per-session state.

**Files added:**
- `backend/src/overseer/mod.rs` — File writer task
- `backend/src/overseer/state.rs` — Per-session derived state (tool counts, file lists)

**Configuration:**
- `OVERSEER_DIR` env var (default `/tmp/agent-sessions`)
- `OVERSEER_ENABLED` env var (default `false`)

### Phase 3: Overseer API Endpoints

HTTP endpoints for the Overseer to inject messages, interrupt sessions, and answer permissions. These are thin wrappers around existing `SessionManager` methods.

**Endpoints:**
- `POST /api/overseer/inject/{session_id}` — Send input to a session
- `POST /api/overseer/interrupt/{session_id}` — Interrupt a session
- `POST /api/overseer/permission/{session_id}` — Answer a pending permission

**Authentication:** These endpoints use the same session cookie auth as other API endpoints. The Overseer's curl commands run on the same machine as the backend, using localhost.

### Phase 4: Overseer Session Management

Backend spawns and manages the Overseer's Claude session. Forwards its output to the frontend via `/ws/overseer`. Handles lifecycle (start, stop, restart, context compaction).

**Files added:**
- `backend/src/overseer/session.rs` — Overseer Claude session lifecycle
- `backend/src/handlers/overseer.rs` — WebSocket handler for `/ws/overseer`

### Phase 5: Frontend Panel

The Overseer panel in the dashboard UI. Commentary thread, input box, mode toggle.

**Files added:**
- `frontend/src/components/overseer_panel.rs` — The sidebar component
- `frontend/styles/overseer.css` — Styling

### Phase 6: Refinement

- Tune the file writer's derived state (better tool/file extraction)
- Tune the Overseer's system prompt based on real usage
- Add mode toggle (advisory / active / autonomous)
- Add cost tracking for the Overseer itself
- Add Overseer session persistence across backend restarts

## Open Questions

1. **Should the Overseer see all users' sessions or only the current user's?** In a team setting, seeing everyone's sessions is more powerful but raises privacy/permission questions. Start with current-user-only.

2. **Should the Overseer survive backend restarts?** The file tree persists on tmpfs (lost on reboot, but survives backend restart). The Overseer's Claude session can be resumed via `--resume` if the session ID is persisted. Probably not worth the complexity for v1.

3. **Should the Overseer have its own permission mode?** It needs Bash (for curl) and Read/Glob/Grep (for the file tree). It doesn't need Edit or Write (unless using action files). A restrictive `--allowed-tools` list makes sense.

4. **How should the Overseer handle high session counts?** With 20+ active sessions, the file tree gets large. The Overseer's system prompt should instruct it to start with `index.json` and `status` files, only diving into `recent.jsonl` for sessions that look interesting. The file tree structure naturally supports this — shallow reads are cheap, deep reads are opt-in.

5. **Should the file tree be user-scoped?** `/tmp/agent-sessions/{user-id}/{session-id}/` vs `/tmp/agent-sessions/{session-id}/`. User-scoping is cleaner for multi-user deployments but adds a directory level. Start with flat (single-user assumption), add scoping when multi-user Overseer is needed.

6. **What about the `--add-dir` token limit?** Claude Code may have limits on how much data it indexes from added directories. If the file tree is large, we may need to configure the Overseer to not use `--add-dir` and instead explicitly Read files. This needs testing.
