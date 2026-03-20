# Proxy Internals

This document describes the internal architecture and message flow of the `claude-portal` binary.

## Overview

The proxy acts as a bridge between:
1. **Claude CLI** - The local Claude Code command-line tool
2. **Backend WebSocket** - The agent-portal backend server
3. **Web Interface** - Users interacting via browser

```
┌─────────────┐     JSON Lines      ┌─────────────┐     WebSocket      ┌─────────────┐
│  Claude CLI │ ←────────────────→  │    Proxy    │ ←────────────────→ │   Backend   │
│  (stdin/out)│                     │             │                     │             │
└─────────────┘                     └─────────────┘                     └─────────────┘
                                                                              ↕
                                                                        ┌─────────────┐
                                                                        │  Frontend   │
                                                                        │  (Browser)  │
                                                                        └─────────────┘
```

## Message Types

### Claude CLI Protocol (via `claude-codes` crate)

The proxy communicates with Claude CLI using the JSON Lines protocol defined in the `claude-codes` crate:

**Input Messages (`ClaudeInput`)**:
```rust
ClaudeInput::User(UserMessage {
    message: MessageContent {
        role: "user",
        content: vec![ContentBlock::Text(TextBlock { text: "..." })]
    },
    session_id: Some("session-uuid")
})
```

**Output Messages (`ClaudeOutput`)**:
- `ClaudeOutput::System` - Initialization and metadata
- `ClaudeOutput::User` - Echoed user messages
- `ClaudeOutput::Assistant` - Claude's responses with content blocks:
  - `TextBlock` - Plain text responses
  - `ThinkingBlock` - Claude's reasoning (with signature)
  - `ToolUseBlock` - Tool invocations (name, id, input)
  - `ToolResultBlock` - Results from tool execution
- `ClaudeOutput::Result` - Session completion with cost/timing info

### WebSocket Protocol (Typed Endpoints)

The proxy communicates with the backend using typed per-endpoint enums defined in `shared/src/endpoints.rs`:

- **`ProxyToServer`**: Messages the proxy sends (Register, SequencedOutput, PermissionRequest, SessionUpdate, InputAck, SessionStatus, Heartbeat)
- **`ServerToProxy`**: Messages the backend sends (RegisterAck, SequencedInput, ClaudeInput, PermissionResponse, OutputAck, FileUploadStart, FileUploadChunk, ServerShutdown, Heartbeat)

## Message Flow

### Startup Sequence

1. **Parse Configuration**
   - Load saved auth from `~/.config/agent-portal/config.json`
   - Parse CLI arguments (`--init`, `--backend-url`, etc.)

2. **Authentication**
   - If `--init` provided: Extract JWT token and save to config
   - If `--dev` mode: Skip authentication entirely
   - Otherwise: Use cached token or run device flow OAuth

3. **Connect to Backend**
   ```
   Proxy → Backend: WebSocket connect to /ws/session
   Proxy → Backend: ProxyToServer::Register { session_id, session_name, auth_token, working_directory, ... }
   ```

4. **Spawn Claude CLI** (via `claude-session-lib`)
   - The session library spawns the claude binary with flags:
     - `--output-format stream-json` - JSON output
     - `--input-format stream-json` - JSON input
     - `--verbose` - Required for streaming
     - `--session-id <uuid>` - Unique session identifier

### Message Forwarding

**User Input (Frontend → Claude)**:
```
Frontend → Backend: ClientToServer::ClaudeInput { content, send_mode }
Backend: Assigns sequence number, stores in pending_inputs
Backend → Proxy: ServerToProxy::SequencedInput { session_id, seq, content, send_mode }
Proxy → Claude: JSON line to stdin
Proxy → Backend: ProxyToServer::InputAck { session_id, ack_seq }
```

**Claude Response (Claude → Frontend)**:
```
Claude → Proxy: JSON line from stdout (ClaudeOutput)
Proxy → Backend: ProxyToServer::SequencedOutput { seq, content }
Backend: Stores in DB, broadcasts to web clients
Backend → Proxy: ServerToProxy::OutputAck { session_id, ack_seq }
Backend → Frontend: ServerToClient::ClaudeOutput { content }
```

## Async Task Structure

The proxy uses `claude-session-lib` for managing the Claude CLI process.
It spawns concurrent tasks coordinated via channels:

```rust
// Create a Claude session via claude-session-lib
let claude_config = SessionConfig {
    session_id,
    working_directory: PathBuf::from(&config.working_directory),
    session_name: config.session_name.clone(),
    resume: config.resume,
    claude_path: None,
    extra_args: config.claude_args.clone(),
    agent_type: config.agent_type,
};
let mut claude_session = Session::new(claude_config).await?;

// Main loop uses tokio::select! to coordinate:
// - Reading Claude stdout (JSON lines) and forwarding as SequencedOutput
// - Reading WebSocket messages (SequencedInput) and forwarding to Claude stdin
// - Heartbeat keepalive
// - Git branch change detection
```

## Configuration Storage

Config is stored in the OS-standard config directory (via the `directories` crate):
- Linux: `~/.config/agent-portal/config.json`
- macOS: `~/Library/Application Support/com.anthropic.agent-portal/config.json`

```json
{
  "sessions": {
    "/path/to/project": {
      "user_id": "uuid",
      "auth_token": "jwt-token",
      "user_email": "user@example.com",
      "last_used": "2024-01-01T00:00:00Z",
      "backend_url": "wss://server.com",
      "session_prefix": "my-prefix"
    }
  },
  "directory_sessions": {
    "/path/to/project": {
      "session_id": "uuid",
      "session_name": "hostname-timestamp",
      "created_at": "...",
      "last_used": "..."
    }
  },
  "preferences": {
    "default_backend_url": null,
    "auto_open_browser": false
  }
}
```

Auth is keyed by working directory, allowing different credentials per project.

## Error Handling

- **Connection failures**: Logged and cause graceful shutdown
- **Parse errors**: Logged as warnings, raw content still forwarded
- **Claude CLI crashes**: Detected via stdout EOF, triggers cleanup
- **WebSocket close**: Triggers session status update in backend

## Rich Content Support

The `ClaudeOutput` types preserve rich content for frontend rendering:

- **Thinking blocks**: Can be shown/hidden in UI
- **Tool use**: Display tool name, inputs, and results
- **Cost tracking**: `ResultMessage` includes `total_cost_usd`
- **Model info**: `AssistantMessage` includes model name
- **Usage stats**: Input/output token counts

The backend stores these as raw JSON, allowing the frontend to render appropriate UI for each content type.
