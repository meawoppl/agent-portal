# Claude Session Library

> **STATUS: COMPLETED** — implemented as the `claude-session-lib/` workspace crate (with the agent-agnostic core later split into `session-lib/` and a Codex counterpart in `codex-session-lib/`). This document is kept as a historical design record; the crate-level rustdoc is authoritative.

This document describes `claude-session-lib`, a library for managing Claude Code sessions programmatically.

## Goal

Enable a **persistence service** to manage multiple Claude Code sessions - launching them, restarting them on failure, and maintaining their state across service restarts.

The library provides tight encapsulation: a `Session` owns everything needed to survive restarts (buffer state, Claude process handle, etc.), while the service orchestrates multiple `Session` instances without reaching into their internals.

## Use Cases

1. **Persistence Service** - A daemon that maintains long-running Claude sessions, restarting them on failure, surviving service restarts
2. **Headless Automation** - Run Claude Code sessions without a UI, with programmatic permission handling
3. **Custom Backends** - Embed session management in alternative backends

## Library Structure

```
claude-session-lib/
├── Cargo.toml
└── src/
    ├── lib.rs           # Public API, re-exports
    ├── session.rs       # Session struct and event loop
    ├── buffer.rs        # OutputBuffer for replay
    ├── snapshot.rs      # SessionSnapshot, serialization
    └── error.rs         # SessionError
```

## Core API

### Session

```rust
use claude_session_lib::{Session, SessionConfig, SessionEvent, PermissionResponse};

// Create a new session
let config = SessionConfig {
    session_id: Uuid::new_v4(),
    working_directory: PathBuf::from("/path/to/project"),
    session_name: "my-session".to_string(),
    resume: false,
    claude_path: None, // Uses "claude" from PATH
};

let mut session = Session::new(config).await?;

// Event loop
while let Some(event) = session.next_event().await {
    match event {
        SessionEvent::Output(output) => {
            // Handle Claude output
        }
        SessionEvent::PermissionRequest { request_id, tool_name, input } => {
            // Respond to permission request
            session.respond_permission(&request_id, PermissionResponse::allow()).await?;
        }
        SessionEvent::Exited { code } => {
            println!("Session exited with code {}", code);
            break;
        }
        SessionEvent::Error(e) => {
            eprintln!("Error: {}", e);
        }
    }
}
```

### Snapshot/Restore

```rust
// Save session state for persistence
let snapshot = session.snapshot();
let bytes = snapshot.to_bytes()?;
fs::write("session.json", bytes)?;

// Later: restore the session
let bytes = fs::read("session.json")?;
let snapshot = SessionSnapshot::from_bytes(&bytes)?;
let mut session = Session::restore(snapshot).await?;
```

### Types

#### SessionConfig

```rust
pub struct SessionConfig {
    pub session_id: Uuid,
    pub working_directory: PathBuf,
    pub session_name: String,
    pub resume: bool,
    pub claude_path: Option<PathBuf>,
    pub extra_args: Vec<String>,
    pub agent_type: AgentType,
}
```

#### SessionEvent

```rust
pub enum SessionEvent {
    Output(ClaudeOutput),
    PermissionRequest { request_id: String, tool_name: String, input: serde_json::Value },
    Exited { code: i32 },
    Error(SessionError),
}
```

#### PermissionResponse

```rust
pub struct PermissionResponse {
    pub allow: bool,
    pub input: Option<serde_json::Value>,
}

impl PermissionResponse {
    pub fn allow() -> Self;
    pub fn allow_with_input(input: serde_json::Value) -> Self;
    pub fn deny() -> Self;
}
```

#### SessionError

```rust
pub enum SessionError {
    SpawnFailed(std::io::Error),
    CommunicationError(String),
    SessionNotFound,
    InvalidPermissionResponse(String),
    AlreadyExited(i32),
    SerializationError(serde_json::Error),
    ClaudeError(claude_codes::Error),
}
```

## Example: Persistence Service

```rust
use claude_session_lib::{Session, SessionConfig, SessionEvent, SessionSnapshot, PermissionResponse};
use std::collections::HashMap;
use uuid::Uuid;

struct PersistenceService {
    sessions: HashMap<Uuid, Session>,
    snapshot_dir: std::path::PathBuf,
}

impl PersistenceService {
    async fn run(&mut self) {
        loop {
            for (id, session) in &mut self.sessions {
                while let Some(event) = session.next_event().await {
                    match event {
                        SessionEvent::Output(output) => {
                            self.broadcast_output(*id, output).await;
                        }
                        SessionEvent::PermissionRequest { request_id, tool_name, input } => {
                            let response = self.handle_permission(&tool_name, &input).await;
                            let _ = session.respond_permission(&request_id, response).await;
                        }
                        SessionEvent::Exited { code } => {
                            if self.should_restart(*id) {
                                self.restart_session(*id).await;
                            }
                        }
                        _ => {}
                    }
                }

                // Periodically snapshot for persistence
                self.save_snapshot(*id, session.snapshot()).await;
            }

            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    async fn restore_sessions(&mut self) -> anyhow::Result<()> {
        let entries = std::fs::read_dir(&self.snapshot_dir)?;
        for entry in entries {
            let entry = entry?;
            let bytes = std::fs::read(entry.path())?;
            let snapshot = SessionSnapshot::from_bytes(&bytes)?;

            match Session::restore(snapshot).await {
                Ok(session) => {
                    self.sessions.insert(session.id(), session);
                }
                Err(e) => {
                    tracing::warn!("Failed to restore session: {}", e);
                }
            }
        }
        Ok(())
    }
}
```

## Relationship to claude-portal Proxy

The `claude-portal` proxy and `claude-session-lib` serve different purposes:

| Aspect | claude-portal proxy | claude-session-lib |
|--------|---------------------|-------------------|
| Purpose | Web-based session UI | Programmatic session management |
| Transport | WebSocket to backend | None (event-based API) |
| Persistence | File-based buffer | Snapshot/restore |
| Target | End users | Services/automation |

The proxy is a full application with WebSocket protocol handling, authentication, git branch detection, and UI. The library is a building block for creating similar services with different requirements.

## Design Decisions

1. **Event-based API** - Rather than a blocking `run()` method, the library exposes `next_event()` for polling. This allows services to multiplex multiple sessions and integrate with their own event loops.

2. **String-based request IDs** - Permission request IDs use `String` (not `Uuid`) to match the claude-codes protocol exactly.

3. **In-memory buffer** - The `OutputBuffer` is in-memory only. Services handle their own persistence strategy via `snapshot()` / `restore()`.

4. **No transport layer** - The library doesn't include WebSocket, HTTP, or any network code. Services bring their own transport.

5. **Minimal dependencies** - Only depends on `claude-codes`, `tokio`, `serde`, `uuid`, `chrono`, and `thiserror`.

## Open Questions

1. **Permission timeout** - If a permission request goes unanswered, should the library timeout and auto-deny? Currently left to the service.

2. **Buffer size limits** - Max buffer size is 1000 outputs. Should this be configurable?

3. **Git branch detection** - Not currently implemented. Services can detect branch changes by watching for Bash tool results containing git commands.
