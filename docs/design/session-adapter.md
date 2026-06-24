# Design: agent-neutral Session boundary (refactor item #1)

**Status:** design slice for review (Claude draft → Codex review). No `session.rs`
changes until the trait shape is agreed.

## Goal

`Session<A>` core must stop matching concrete `claude_codes` / `codex_codes`
types. Today `session-lib/src/session.rs::next_event` matches
`ClaudeOutput::{Result, ControlRequest, ControlResponse}` directly, and
`send_input`/`respond_permission` build `ClaudeInput` / route claude-vs-codex
`IoCommand` variants. That structural coupling is why `codex-session-lib` carries
a parallel `io_task`/`handler` instead of reusing `Session`.

Fix: an **adapter** owns all protocol parse/serialize and exposes only **neutral
decisions**. Concrete protocol enums stay *inside* the adapter (never surface
into `session.rs`, not even via associated types).

## What `session.rs` decides today (the classification to neutralize)

From `next_event` on each `ClaudeOutput`:
- `Result` with `is_error` + "No conversation found" → `SessionNotFound`.
- `ControlRequest(CanUseTool)` → emit `PermissionRequest { request_id, tool_name, input, suggestions }`, set `WaitingForPermission`, stash `PendingPermission`.
- `ControlResponse` → skip (internal ack, no-op).
- anything else → user-visible `Output` (buffer + forward).

Input side: `send_input` → `ClaudeInput::user_message(text, id)`;
`respond_permission` → claude `PermissionResponse` vs codex `CodexApproval`.

## Proposed neutral types

```rust
/// One neutral decision produced by the adapter from a unit of agent output.
pub enum AgentOutput {
    /// User-visible output to forward/persist verbatim (opaque wire JSON).
    Visible(serde_json::Value),
    /// A permission/control request awaiting a response.
    PermissionRequest {
        request_id: String,              // neutral correlation id
        tool_name: String,
        input: serde_json::Value,
        suggestions: Vec<PermissionSuggestion>, // see "open question" below
    },
    /// End-of-turn terminator + optional result summary/metrics.
    TurnEnd { result: Option<TurnResult> },
    /// Hard session limit with reset info.
    SessionLimit { reset_at: String, source_message: String, prompt: String },
    /// Fatal "conversation not found" → maps to SessionNotFound.
    NotFound,
    /// Internal ack / nothing for the consumer — Session skips it.
    Noop,
}

/// Neutral permission decision (already ~PermissionResponse, minus claude types).
pub struct PermissionDecision {
    pub allow: bool,
    pub modified_input: Option<serde_json::Value>,
    pub remember: Vec<RememberRule>,     // see "open question"
    pub reason: Option<String>,
}

/// Opaque transport payload the I/O task writes to the agent. The adapter
/// produces it; Session never inspects it.
pub struct TransportPayload(/* bytes or serde_json::Value, opaque */);
```

## Proposed trait

```rust
pub trait AgentAdapter: Send + 'static {
    /// Parse + classify one unit of agent stdout into 0..n neutral decisions.
    /// The adapter owns protocol parsing; `raw` is the opaque transport unit.
    fn classify(&mut self, raw: RawUnit) -> Vec<AgentOutput>;

    /// Build the payload for plain user text.
    fn user_input(&self, text: &str, session_id: Uuid) -> TransportPayload;

    /// Build the payload responding to a prior PermissionRequest.
    fn permission_response(&self, request_id: &str, decision: PermissionDecision)
        -> TransportPayload;

    /// Optional control inputs (interrupt, etc.).
    fn interrupt(&self) -> Option<TransportPayload> { None }
}
```

`Session<A: AgentAdapter>` then:
- `next_event` matches on `AgentOutput` variants only (no `ClaudeOutput`), keeping
  the same state transitions (`WaitingForPermission`, `Exited`, `SessionNotFound`)
  and buffering.
- `send_input` calls `adapter.user_input(..)`; `respond_permission` calls
  `adapter.permission_response(..)`. The `IoCommand` union
  (`PermissionResponse` vs `CodexApproval`) collapses into one
  `IoCommand::Send(TransportPayload)`.

## Migration (behavior-preserving, staged)

1. **This design** → review/agree the trait shape. (no code)
2. **ClaudeAdapter only**: move the claude classification (the `next_event`
   matches) and `ClaudeInput` construction into `ClaudeAdapter`; rewrite
   `session.rs` to be adapter-generic. Verified behavior-identical (existing
   tests + proxy smoke). Claude path only.
3. **CodexAdapter**: implement `AgentAdapter` for the codex app-server protocol,
   replacing `RawOutput`/`CodexPermissionRequest`/`CodexApproval` special-casing.
4. **Collapse**: route `codex-session-lib` through the shared `Session<CodexAdapter>`
   and delete its parallel `io_task`/`handler`.

## Open questions for review

- **`PermissionSuggestion` / `Permission`**: these are `claude_codes` types that
  currently surface in `SessionEvent::PermissionRequest` and `PermissionResponse`.
  Fully neutralizing means introducing neutral `Suggestion`/`RememberRule` types
  and converting in the adapter. Do we neutralize these now (bigger blast into the
  proxy/backend consumers of `SessionEvent`) or keep them opaque (`serde_json::Value`)
  for slice 1 and neutralize in a follow-up? (Claude leans: opaque `Value` for
  slice 1 to keep it contained.)
- **Who parses?** Proposal puts parsing in `adapter.classify(raw)`. Alternative
  keeps the I/O task parsing and passes typed-but-opaque handles — rejected, as it
  re-leaks concrete types.
- **`RawUnit`/`TransportPayload` concrete form**: bytes vs `serde_json::Value`.
  (Claude leans `serde_json::Value` for JSON-line protocols; revisit if a binary
  transport appears.)
