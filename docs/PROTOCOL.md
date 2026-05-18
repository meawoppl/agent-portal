# WebSocket Protocol Reference

This document describes the WebSocket protocol used for communication between the proxy CLI, backend server, and frontend web clients.

## Architecture Overview

```
claude CLI ──stdout──► proxy ──WS──► backend ──WS──► frontend
claude CLI ◄──stdin─── proxy ◄──WS── backend ◄──WS── frontend
```

Four components communicate over three WebSocket connections:

| Connection | Endpoint | Direction |
|---|---|---|
| Proxy ↔ Backend | `/ws/session` | Bidirectional |
| Frontend ↔ Backend | `/ws/client` | Bidirectional |
| Launcher ↔ Backend | `/ws/launcher` | Bidirectional |

Each endpoint has its own typed message enums defined in `shared/src/endpoints.rs`, serialized as JSON with a `"type"` discriminant tag:

| Endpoint | Server→Client | Client→Server |
|---|---|---|
| `/ws/session` | `ServerToProxy` | `ProxyToServer` |
| `/ws/client` | `ServerToClient` | `ClientToServer` |
| `/ws/launcher` | `ServerToLauncher` | `LauncherToServer` |

## Message Types

### Session Lifecycle

#### `Register` (proxy → backend)
Sent when a proxy connects to register or resume a session.

```json
{
  "type": "Register",
  "session_id": "uuid",
  "session_name": "my-project",
  "auth_token": "jwt-token",
  "working_directory": "/home/user/project",
  "resuming": false,
  "git_branch": "main",
  "replay_after": null,
  "client_version": "1.0.0"
}
```

#### `RegisterAck` (backend → proxy)
Acknowledgment of registration.

```json
{
  "type": "RegisterAck",
  "success": true,
  "session_id": "uuid",
  "error": null
}
```

#### `Register` (frontend → backend)
Web clients also use `Register` to subscribe to a session's output stream. The `replay_after` field controls which historical messages to replay.

#### `SessionUpdate` (proxy → backend → frontend)
Notifies of session metadata changes (e.g., git branch switch).

```json
{
  "type": "SessionUpdate",
  "session_id": "uuid",
  "git_branch": "feature-branch"
}
```

#### `SessionStatus` (backend → frontend)
Session status changes.

```json
{
  "type": "SessionStatus",
  "status": "active"
}
```

Status values: `active`, `inactive`, `disconnected`.

### Message Sequencing

The protocol uses sequence numbers for reliable, at-least-once delivery in both directions.

#### Output Flow (proxy → backend)

```
proxy                    backend
  │                        │
  ├─ SequencedOutput{seq=1}─►│  proxy buffers msg
  │                        ├─ stores in DB
  │◄─ OutputAck{ack_seq=1}──┤  backend confirms
  │   proxy drops buffer   │
```

**`SequencedOutput`** (proxy → backend): Claude's output with a monotonic sequence number. The proxy buffers these until acknowledged.

```json
{
  "type": "SequencedOutput",
  "seq": 42,
  "content": { "type": "assistant", "message": { ... } },
  "agent_type": "claude"
}
```

`agent_type` (added in 2.5.42) is `"claude"` or `"codex"` and identifies the
wire format the `content` JSON conforms to. It is tagged at the proxy emission
boundary on each message — not derived from the registration — so a future
multi-agent session could ferry messages from more than one agent over the
same `/ws/session` connection without mis-attributing them. Pre-2.5.42
proxies that don't send this field parse as `"claude"` by default
(`#[serde(default)]`), which is a presumed misattribution for any in-flight
codex session running an older proxy until the proxy upgrades.

**`OutputAck`** (backend → proxy): Confirms storage of all messages up to `ack_seq`. The proxy can drop buffered messages with `seq <= ack_seq`.

```json
{
  "type": "OutputAck",
  "session_id": "uuid",
  "ack_seq": 42
}
```

**Deduplication**: The backend tracks the last acknowledged sequence per session. If a `SequencedOutput` arrives with `seq <= last_ack`, it's treated as a duplicate and immediately acked without re-storing.

#### Input Flow (frontend → backend → proxy)

```
frontend                 backend                  proxy
  │                        │                        │
  ├─ ClaudeInput{content}──►│                        │
  │                        ├─ assigns seq, stores   │
  │                        ├─ SequencedInput{seq=1}─►│
  │                        │                        ├─ forwards to claude
  │                        │◄─ InputAck{ack_seq=1}──┤
  │                        ├─ deletes from DB       │
```

**`ClaudeInput`** (frontend → backend): User input to send to Claude. Backend assigns a sequence number, stores in `pending_inputs` table, then forwards as `SequencedInput`.

```json
{
  "type": "ClaudeInput",
  "content": { "type": "human", "message": "hello" },
  "send_mode": "normal"
}
```

**`SequencedInput`** (backend → proxy): Input with assigned sequence number, stored in DB until acknowledged.

```json
{
  "type": "SequencedInput",
  "session_id": "uuid",
  "seq": 5,
  "content": { "type": "human", "message": "hello" }
}
```

**`InputAck`** (proxy → backend): Confirms receipt of input. Backend deletes all `pending_inputs` with `seq <= ack_seq`.

```json
{
  "type": "InputAck",
  "session_id": "uuid",
  "ack_seq": 5
}
```

**Replay on reconnect**: When a proxy reconnects, the backend replays all unacknowledged `pending_inputs` as `SequencedInput` messages.

#### Legacy Output

**`ClaudeOutput`** (proxy → backend): Unsequenced output, used for backward compatibility. No acknowledgment or buffering.

```json
{
  "type": "ClaudeOutput",
  "content": { "type": "assistant", "message": { ... } },
  "agent_type": "claude"
}
```

Carries the same `agent_type` tag as `SequencedOutput` (added in 2.5.42,
default `"claude"` for pre-2.5.42 proxies).

### Permission Flow

```
proxy                    backend                  frontend
  │                        │                        │
  ├─ PermissionRequest────►│                        │
  │                        ├─ stores in DB          │
  │                        ├─ PermissionRequest────►│  user sees prompt
  │                        │                        │
  │                        │◄─ PermissionResponse──┤  user clicks allow
  │                        ├─ deletes from DB       │
  │◄─ PermissionResponse──┤                        │
  │   forwards to claude   │                        │
```

**`PermissionRequest`** (proxy → backend → frontend): A tool wants to execute and needs user approval.

```json
{
  "type": "PermissionRequest",
  "request_id": "unique-id",
  "tool_name": "Bash",
  "input": { "command": "rm -rf /tmp/test" },
  "permission_suggestions": []
}
```

**`PermissionResponse`** (frontend → backend → proxy): User's decision.

```json
{
  "type": "PermissionResponse",
  "request_id": "unique-id",
  "allow": true,
  "input": { "command": "rm -rf /tmp/test" },
  "permissions": [],
  "reason": null
}
```

The backend stores pending permission requests in the `pending_permission_requests` table and replays them when a web client connects, ensuring permissions aren't lost if the user refreshes.

### Keep-Alive

**`Heartbeat`** (bidirectional): Keeps WebSocket connections alive. The proxy sends these periodically; the backend echoes them back.

```json
{ "type": "Heartbeat" }
```

### Cost Tracking

**`UserSpendUpdate`** (backend → frontend): Periodic spend summary sent to web clients.

```json
{
  "type": "UserSpendUpdate",
  "total_spend_usd": 12.34,
  "session_costs": [
    { "session_id": "uuid", "total_cost_usd": 5.67 }
  ]
}
```

### Voice Input

**`StartVoice`** / **`StopVoice`** (frontend → backend): Control voice recording sessions.

**`Transcription`** (backend → frontend): Speech-to-text results with confidence scores.

**`VoiceError`** / **`VoiceEnded`** (backend → frontend): Error and completion signals.

### Server Lifecycle

**`ServerShutdown`** (backend → all clients): Sent before graceful shutdown.

```json
{
  "type": "ServerShutdown",
  "reason": "Server restarting for update",
  "reconnect_delay_ms": 5000
}
```

### Error

**`Error`** (any direction): Generic error message.

```json
{
  "type": "Error",
  "message": "Something went wrong"
}
```

## Connection Behavior

### Proxy Connection (`/ws/session`)

1. Proxy connects and sends `Register`
2. Backend looks up or creates session in DB
3. Backend sends `RegisterAck`
4. If successful, backend replays unacknowledged `pending_inputs`
5. Proxy begins forwarding Claude's stdout as `SequencedOutput`
6. On disconnect, backend marks session as `"disconnected"`

### Web Client Connection (`/ws/client`)

1. Frontend connects (must have valid session cookie)
2. Frontend sends `Register` with `session_id` to subscribe
3. Backend verifies session access via `session_members` table
4. Backend replays message history (filtered by `replay_after`)
5. Backend replays any pending permission request
6. Frontend receives live `ClaudeOutput`, `PermissionRequest`, etc.

### Reconnection

**Proxy reconnection**: The proxy has an exponential backoff reconnect loop. On reconnect, it re-sends `Register` with `resuming: true`. The backend replays pending inputs. Output messages buffered in the proxy's local buffer are re-sent as `SequencedOutput` with their original sequence numbers.

**Frontend reconnection**: Web clients reconnect and re-register with `replay_after` set to the timestamp of their last received message to avoid duplicates.

**Backend buffering**: When a proxy is disconnected, the backend queues up to 100 messages per session (with a 5-minute TTL) in memory. These are replayed on proxy reconnection.

## Send Modes

The `ClaudeInput` message supports an optional `send_mode` field:

| Mode | Behavior |
|---|---|
| `normal` (default) | Single message send |
| `wiggum` | Iterative autonomous loop - proxy re-sends the prompt after each result until Claude signals completion |

## Authentication

### Proxy Authentication
The proxy includes a JWT `auth_token` in the `Register` message. The backend verifies this against `proxy_auth_tokens` in the database.

### Web Client Authentication
Web clients authenticate via session cookies (signed with the server's cookie key). The `/ws/client` endpoint extracts the user ID from the cookie before allowing the WebSocket upgrade.

In dev mode, both authentication methods fall back to the `testing@testing.local` user.
