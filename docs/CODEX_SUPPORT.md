# Codex Support Plan

This document plans what would need to change in agent-portal to support OpenAI Codex CLI sessions alongside Claude Code sessions. Both agent SDKs ([claude-codes](https://crates.io/crates/claude-codes) and [codex-codes](https://crates.io/crates/codex-codes)) are maintained in the same [repository](https://github.com/meawoppl/rust-code-agent-sdks).

## Protocol Comparison

The two CLIs have fundamentally different output protocols.

### Claude Code: Role-Based Messages

Claude emits a flat stream of role-tagged JSON objects (`--output-format stream-json`):

```
{"type":"system","subtype":"init","session_id":"...","model":"claude-sonnet-4-20250514",...}
{"type":"assistant","message":{"content":[{"type":"text","text":"..."}],...}}
{"type":"result","subtype":"success","duration_ms":5000,"total_cost_usd":0.05,"usage":{...}}
```

Top-level enum: `ClaudeOutput` (8 variants: System, User, Assistant, Result, ControlRequest, ControlResponse, Error, RateLimitEvent).

Key characteristics:
- Content blocks (`TextBlock`, `ThinkingBlock`, `ToolUseBlock`, `ToolResultBlock`, `ImageBlock`) are nested inside messages
- Tool use and tool results are content blocks within Assistant/User messages
- `ResultMessage` carries cumulative usage, cost, and turn count
- Permission flow is inline via `ControlRequest`/`ControlResponse`
- `RateLimitEvent` provides utilization percentages and reset times

### OpenAI Codex: Event-Driven Thread Model

Codex emits JSONL thread lifecycle events (`--json` flag):

```
{"type":"thread.started","thread_id":"th_abc123"}
{"type":"turn.started"}
{"type":"item.started","item":{"type":"command_execution","id":"cmd_1","command":"ls","aggregated_output":"","status":"in_progress"}}
{"type":"item.completed","item":{"type":"command_execution","id":"cmd_1","command":"ls","aggregated_output":"file.txt\n","exit_code":0,"status":"completed"}}
{"type":"item.completed","item":{"type":"agent_message","id":"msg_1","text":"Here are the files..."}}
{"type":"turn.completed","usage":{"input_tokens":1000,"cached_input_tokens":500,"output_tokens":200}}
```

Top-level enum: `ThreadEvent` (8 variants: ThreadStarted, TurnStarted, TurnCompleted, TurnFailed, ItemStarted, ItemUpdated, ItemCompleted, Error).

Item types: `ThreadItem` (8 variants: AgentMessage, Reasoning, CommandExecution, FileChange, McpToolCall, WebSearch, TodoList, Error).

Key characteristics:
- Tool actions are first-class item types (not nested content blocks)
- Items have explicit lifecycle: `item.started` -> `item.updated` -> `item.completed`
- Usage is per-turn only (no cumulative cost tracking)
- No permission request flow in the JSONL stream (handled by `ApprovalMode` config)
- No rate limit events (not exposed in Codex's protocol)
- Reasoning/thinking is a first-class `ReasoningItem`, not a content block

### Feature Matrix

| Feature | Claude Code | Codex | Portal Support Needed |
|---------|------------|-------|----------------------|
| Text messages | `AssistantMessage.content[TextBlock]` | `AgentMessage.text` | Normalize to common render |
| Thinking/reasoning | `ThinkingBlock` content block | `ReasoningItem` thread item | Normalize to common render |
| Tool use (bash) | `ToolUseBlock` + `ToolResultBlock` | `CommandExecutionItem` | Distinct renderers OK |
| File edits | `ToolUseBlock(Write/Edit)` | `FileChangeItem` with patches | Distinct renderers OK |
| MCP tools | `ToolUseBlock` (generic) | `McpToolCallItem` (typed) | Distinct renderers OK |
| Web search | `ToolUseBlock` (generic) | `WebSearchItem` | Distinct renderers OK |
| Todo lists | Not exposed | `TodoListItem` | New renderer (Codex only) |
| Images | `ImageBlock` | Not in protocol | Claude only |
| Permission requests | `ControlRequest`/`ControlResponse` | Not in JSONL stream | Claude only (for now) |
| Rate limits | `RateLimitEvent` | Not in protocol | Claude only |
| Usage/tokens | `ResultMessage.usage` (cumulative) | `TurnCompletedEvent.usage` (per-turn) | Accumulate for Codex |
| Cost tracking | `ResultMessage.total_cost_usd` | Not available | Claude only |
| Session identity | `ResultMessage.session_id` | `ThreadStartedEvent.thread_id` | Map thread_id to session |
| Error model | `AnthropicError` + `ResultMessage.errors` | `TurnFailedEvent` + `ErrorItem` | Normalize |

## Architecture Changes

### Layer 1: Agent-Agnostic Session Library

The current `claude-session-lib` crate is tightly coupled to `claude_codes::ClaudeOutput`. The first step is to make the session library support both agents.

#### Option A: Trait Abstraction (Recommended)

Define a common trait in `claude-session-lib` that both agent types implement:

```rust
/// Output from any code agent CLI (Claude, Codex, etc.)
enum AgentOutput {
    /// Raw JSON to forward to the backend as-is
    RawMessage(serde_json::Value),
    /// Agent finished a turn
    TurnComplete {
        usage: Option<CommonUsage>,
        is_error: bool,
        duration_ms: Option<u64>,
    },
    /// Agent is requesting permission (Claude only currently)
    PermissionRequest { ... },
    /// Agent process exited
    ProcessExited { exit_code: Option<i32> },
}

/// Common usage stats normalized across agents
struct CommonUsage {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
}

/// Trait for spawning and communicating with a code agent
trait AgentDriver {
    /// Spawn the agent process
    async fn spawn(config: &SessionConfig) -> Result<Self>;
    /// Read the next output event
    async fn next_output(&mut self) -> Option<AgentOutput>;
    /// Send user input
    async fn send_input(&mut self, input: &str) -> Result<()>;
    /// Send permission response (no-op for agents that don't support it)
    async fn send_permission_response(&mut self, response: PermissionResponse) -> Result<()>;
}
```

Then implement `AgentDriver` for both:
- `ClaudeDriver` — wraps `claude_codes::AsyncClient`, converts `ClaudeOutput` → `AgentOutput`
- `CodexDriver` — wraps a raw `tokio::process::Child`, parses JSONL into `codex_codes::ThreadEvent`, converts to `AgentOutput`

**Note**: `codex-codes` does not currently ship an `AsyncClient` like `claude-codes` does. This is something we could request upstream since both crates are maintained by the same author, or we build the Codex process management ourselves using `tokio::process::Command` and JSONL line parsing.

#### Option B: Enum Dispatch

Instead of a trait, use a concrete enum:

```rust
enum AgentSession {
    Claude(ClaudeSession),
    Codex(CodexSession),
}
```

This is simpler but less extensible. Given there are only two agents today, this may be pragmatic.

### Layer 2: Protocol Changes

The WebSocket protocol between proxy and backend currently forwards raw `ClaudeOutput` JSON in `SequencedOutput.content`. This works because the backend and frontend both know how to parse Claude's format.

For Codex support, two approaches:

#### Option A: Agent-Tagged Raw Forwarding (Recommended)

Add an `agent_type` field to session registration and tag output messages:

```rust
enum AgentType {
    Claude,
    Codex,
}

// In ProxyToServer::Register
Register {
    // ... existing fields
    agent_type: AgentType,  // NEW
}
```

The backend stores `agent_type` on the session. Output messages continue to carry raw JSON — the frontend uses the session's `agent_type` to pick the right parser/renderer.

This requires:
- `shared/src/lib.rs`: Add `AgentType` enum
- `shared/src/endpoints.rs`: Add `agent_type` to `Register`
- `backend/src/models.rs`: Add `agent_type` column to `Session`
- Database migration: `ALTER TABLE sessions ADD COLUMN agent_type VARCHAR(16) NOT NULL DEFAULT 'claude'`
- Frontend: Dispatch to different message parsers based on `agent_type`

#### Option B: Normalize to Common Format

Convert all output to a unified internal format before forwarding. Higher effort, loses agent-specific details, but simplifies the frontend.

Not recommended — the two protocols are different enough that normalization would lose information.

### Layer 3: Frontend Rendering

The frontend's `message_renderer.rs` currently defines `ClaudeMessage` with Claude-specific variants. For Codex:

#### New File: `frontend/src/components/codex_renderer.rs`

```rust
enum CodexMessage {
    ThreadStarted { thread_id: String },
    AgentMessage { id: String, text: String },
    Reasoning { id: String, text: String },
    CommandExecution { id: String, command: String, output: String, exit_code: Option<i32>, status: String },
    FileChange { id: String, changes: Vec<FileUpdateChange>, status: String },
    McpToolCall { id: String, server: String, tool: String, arguments: Value, status: String },
    WebSearch { id: String, query: String },
    TodoList { id: String, items: Vec<TodoItem> },
    TurnCompleted { usage: Usage },
    Error { message: String },
    Unknown,
}
```

Rendering approach:
- `AgentMessage` → Markdown rendering (same as Claude's assistant text)
- `Reasoning` → Collapsible thinking block (same visual as Claude's `ThinkingBlock`)
- `CommandExecution` → Terminal-style output (similar to Claude's Bash tool results)
- `FileChange` → Diff view (similar to Claude's Write/Edit tool results)
- `TodoList` → Checklist rendering (new, Codex-specific)
- `TurnCompleted` → Stats bar with token counts (similar to Claude's result message, but no cost)

#### Shared Rendering Components

Some renderers can be shared:
- Markdown text rendering
- Code block / terminal output styling
- Collapsible sections for thinking/reasoning
- Stats bars for usage display

These could be extracted to `frontend/src/components/shared_renderers.rs`.

### Layer 4: Session Awaiting Detection

The current `CheckAwaiting` logic searches backwards for `type == "result"`. For Codex, the equivalent is `type == "turn.completed"` or `type == "turn.failed"`. The detection needs to be agent-type-aware:

```rust
// In component.rs CheckAwaiting handler
let awaiting_types = match agent_type {
    AgentType::Claude => &["result"],
    AgentType::Codex => &["turn.completed", "turn.failed"],
};
```

### Layer 5: Proxy CLI Changes

The `claude-portal` proxy binary needs to support launching Codex instead of Claude.

#### CLI Arguments

```
claude-portal --agent codex [--codex-path /usr/local/bin/codex] ...
claude-portal --agent claude [--claude-path /usr/local/bin/claude] ...  # default
```

Or detect from the session configuration on the backend.

#### Codex CLI Invocation

```bash
codex --json --full-auto "prompt here"
```

Key differences from Claude:
- `--json` flag (vs `--output-format stream-json`)
- `--full-auto` or `--auto-edit` (vs `--permission-prompt-tool stdio`)
- No `--session-id` equivalent (Codex uses thread IDs internally)
- No `--resume` equivalent
- Input is passed as a positional argument, not via stdin streaming
- Codex doesn't support interactive stdin for follow-up messages in the same way

This last point is the biggest architectural challenge: Claude supports continuous bidirectional stdin/stdout streaming, while Codex is more request/response oriented. For multi-turn conversations with Codex, the proxy would need to restart the process for each user message or use the (not-yet-stable) SDK threading API.

### Layer 6: Database Schema

Minimal schema changes needed:

```sql
-- Add agent type to sessions
ALTER TABLE sessions ADD COLUMN agent_type VARCHAR(16) NOT NULL DEFAULT 'claude';

-- Cost columns become nullable for Codex (no cost tracking)
-- Actually, they already default to 0, so no change needed
```

### Layer 7: Backend Message Handling

`store_result_metadata` currently extracts Claude-specific fields. For Codex:

```rust
fn store_codex_turn_metadata(conn, session_id, content: &Value) {
    // Extract usage from turn.completed events
    if let Some(usage) = content.get("usage") {
        // Accumulate tokens (Codex reports per-turn, not cumulative)
        diesel::update(sessions::table.find(session_id))
            .set((
                sessions::input_tokens.eq(sessions::input_tokens + usage.input_tokens),
                sessions::output_tokens.eq(sessions::output_tokens + usage.output_tokens),
                // Codex has cached_input_tokens, Claude has cache_read_input_tokens
                sessions::cache_read_tokens.eq(sessions::cache_read_tokens + usage.cached_input_tokens),
            ))
            .execute(conn);
    }
    // No cost tracking for Codex
}
```

## Upstream Requests (rust-code-agent-sdks)

Issues to open on the shared repository to make this easier:

1. **AsyncClient for codex-codes**: The `claude-codes` crate has `AsyncClient` for process management. `codex-codes` currently only has types. Adding a parallel `AsyncClient` that spawns `codex`, passes `--json`, and yields `ThreadEvent` would eliminate boilerplate in the proxy.

2. **Common trait crate**: A third crate (e.g., `code-agent-common`) that defines the `AgentDriver` trait and `CommonUsage` types. Both `claude-codes` and `codex-codes` could implement it. This would let consumers write agent-agnostic code without manual dispatch.

3. **Input handling for codex-codes**: Document or expose how to send follow-up messages to a running Codex thread. The current JSONL output is read-only — understanding the input side is critical for multi-turn support.

## Implementation Phases

### Phase 1: Foundation (Low Risk) -- COMPLETED

All Phase 1 items have been implemented:

1. `AgentType` enum in `shared/src/lib.rs`
2. `agent_type` field on `RegisterFields` and `SessionInfo`
3. Database migration `2026-02-21-204646_add_agent_type_to_sessions`
4. Backend stores and returns `agent_type`
5. Frontend displays agent type on sessions
6. Proxy supports `--agent codex` CLI flag
7. Launcher supports per-session `agent_type` in config

### Phase 2: Codex Rendering (Frontend Only)

1. Create `codex_renderer.rs` with renderers for all `ThreadItem` types
2. `message_renderer.rs` dispatches based on session's `agent_type`
3. Update `CheckAwaiting` for Codex turn completion detection
4. Extract shared rendering components

Can be developed and tested with static fixture data before the proxy supports Codex.

### Phase 3: Codex Proxy Driver

1. `codex-codes` dependency already in workspace (v0.101.0)
2. Implement `CodexDriver` in `claude-session-lib` (or a new `codex-session-lib`)
3. Process management: spawn `codex --json`, parse JSONL stdout
4. Handle Codex's request/response model vs Claude's streaming model
5. Accumulate per-turn usage into session totals

### Phase 4: Multi-Agent Launcher

1. Launcher supports starting Codex sessions (new `--agent codex` flag or per-session config)
2. Backend session creation specifies agent type
3. Frontend launch dialog offers agent choice

## Open Questions

1. **Multi-turn Codex**: Does Codex support persistent interactive sessions, or is each invocation a single turn? This fundamentally affects whether the proxy can maintain a long-running Codex process like it does with Claude.

2. **Permission handling**: Codex has `ApprovalMode` but doesn't expose permission requests in the JSONL stream. Can we intercept approval prompts, or do we need to always run in `--full-auto`?

3. **Session resume**: Claude has `--resume` and `--session-id` for persistent sessions. Does Codex have an equivalent for thread continuity?

4. **Wiggum mode**: The iterative autonomous loop currently checks Claude's `ResultMessage` for "DONE". The Codex equivalent would check `AgentMessage` text in `TurnCompleted` events.

5. **Mixed sessions**: Should a single portal user be able to run Claude and Codex sessions simultaneously? Probably yes — sessions are already independent.
