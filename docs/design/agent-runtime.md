# Design: AgentRuntime — neutralizing the `Session<A>` ↔ I/O-task boundary (#1165 item 2, phase A)

**Status:** **agreed** (Claude draft → Codex consensus, 2026-06-29). Scope is
fixed to slices 1+2 below; slice 3 (shared runtime) is deferred indefinitely.
High-blast-radius (touches every `Session` consumer + both agent backends), so
each slice lands behind characterization tests + cross-review.

## Consensus (resolved 2026-06-29)

Codex review of the open questions:

1. **Classify moves into the io-task** — agreed; it holds the native unit, and
   it matches the post-#1190 Codex reality. **Caveat (must honor):** preserve
   the existing raw/visible buffering semantics exactly — `AgentOutput::Visible`
   still carries the original renderable raw value, and multi-output `classify`
   ordering needs **characterization tests on BOTH Claude and Codex**.
2. **Take the LIGHTER path:** neutralize the command/event enums and keep
   per-agent io-tasks. **No `async_trait` / no shared `AgentDriver` runtime in
   the first coding pass** — Codex input/permission is stateful enough that an
   async driver trait risks being leaky. No new dep until we know we need `dyn`
   async dispatch.
3. **Keep `TurnMetricsReady` / `CodexThreadId` / `SessionLimitReached` as
   explicit neutral `IoEvent` variants** — orchestration/lifecycle signals, not
   model-visible output; keeping them out of `AgentOutput` stops the
   output/renderer contract becoming a junk drawer. `PermissionRequest` in
   `AgentOutput` is fine.
4. **Stop at slices 1+2.** Acceptance line: *`Session` has no `claude_codes`/
   `codex_codes` types and no `agent_type` branch for `send_input`/
   `respond_permission`.* Separate Claude/Codex io-tasks are acceptable
   indefinitely if the boundary is neutral. Slice 3 is a later opt-in only if
   duplication stays painful.

The sketch of an `async AgentDriver` trait below (§"The driver trait") is
therefore **deferred** — recorded for slice 3, NOT part of the agreed work.

## Where we are after phase B

Phase B (the classification boundary) is complete:
- `Session<A>` owns an `AgentAdapter` via `Agent::adapter()` (#1181).
- `AgentOutputClassifier` (output mapping) is split from `AgentAdapter` (input
  side), with an associated `Raw` type — Claude classifies `serde_json::Value`,
  Codex classifies the typed `ServerMessage` (which is not `Deserialize`) (#1186/#1188).
- `CodexClassifier` is the single source of Codex output mapping; `handler.rs`
  delegates to it (#1190).

What remains coupled — the reason `codex-session-lib` still carries a parallel
path instead of reusing the generic core:

1. **`IoEvent::Output(Box<ClaudeOutput>)` is Claude-typed.** Session's generic
   classify path (`next_event`'s `Output` arm → `adapter.classify`) therefore
   only fires for Claude. Codex bypasses it entirely: its io-task emits
   `IoEvent::RawOutput` + `IoEvent::CodexPermissionRequest` directly, and
   `Agent::adapter()` returns `None` for Codex. So Session carries codex-specific
   arms (`RawOutput`, `CodexPermissionRequest`) alongside the generic one.

2. **`IoCommand::Input { input: ClaudeInput, .. }` is Claude-typed.** The codex
   io-task receives a `ClaudeInput` it doesn't want and digs the prompt text out
   with `extract_prompt_text`. `Session::send_input` hardcodes
   `ClaudeInput::user_message`.

3. **`Session::respond_permission` branches on `config.agent_type`** (Claude
   `ControlResponse` vs Codex `CodexApproval`) — concrete-protocol knowledge in
   the generic core.

`AgentAdapter::{user_input, permission_response, interrupt}` exist (Claude impl)
but are **dead** — nothing calls them; the live input path is the hardcoded
`send_input`/`respond_permission` above.

## Goal

Make `Session<A>` fully agent-agnostic: no `claude_codes`/`codex_codes` types,
no `agent_type` branch, no codex-specific event arms. Both backends reduce to a
small per-agent **driver**: spawn the process, translate neutral commands → the
agent's wire protocol, and classify the agent's output → neutral `AgentOutput`.

## Key design choice: classify in the I/O task, not in `Session`

Phase B put `classify` in `Session` (Claude path). That can't generalize,
because `IoEvent::Output` would have to carry every agent's native unit
(`ClaudeOutput` vs `ServerMessage`, the latter not even `Deserialize`).

**Proposal: move classification into the I/O task** (which already holds the
typed unit) and have it emit *neutral* `AgentOutput`. `Session` then only maps
`AgentOutput → SessionEvent` and never sees a protocol type.

- Claude io-task: calls `ClaudeAdapter::classify(value)` and emits the results.
- Codex io-task: already classifies via `CodexClassifier` (#1190) — switch its
  emit from `RawOutput`/`CodexPermissionRequest` to the same neutral channel.

This deletes `IoEvent::Output(ClaudeOutput)`, `IoEvent::RawOutput` (as a
codex-specific bypass), and `IoEvent::CodexPermissionRequest`, replacing them
with one neutral `IoEvent::Classified(AgentOutput)` (name TBD). Session's
`Output`/`RawOutput`/`CodexPermissionRequest` arms collapse into one
`AgentOutput`-matching arm (the logic already exists in `next_event`).

## Neutral command enum (input side)

Replace the Claude-typed `IoCommand` with:

```rust
pub enum IoCommand {
    UserInput {
        text: String,
        display_event: Option<Box<serde_json::Value>>, // unchanged semantics
        delivered: Option<oneshot::Sender<Result<(), String>>>,
    },
    Permission(PermissionDecision),   // neutral; the driver serializes it
    Interrupt,
}
```

Each driver translates: Claude builds `ClaudeInput::user_message` / a
`ControlResponse` and writes stdin; Codex calls `turn_start` / `respond` RPCs.
This is exactly what `AgentAdapter::{user_input, permission_response}` were meant
to do — so those (currently dead) methods become the driver's translation hooks,
**but async** for Codex (its input is an `await`ed RPC, not a sync payload).

## The driver trait (sketch — the real "AgentRuntime")

```rust
#[async_trait]
pub trait AgentDriver: Send {
    type Raw;                                   // ClaudeOutput-value / ServerMessage
    fn classifier(&mut self) -> &mut dyn AgentOutputClassifier<Raw = Self::Raw>;
    async fn send_user_input(&mut self, text: &str, ...) -> Result<(), SessionError>;
    async fn respond_permission(&mut self, decision: PermissionDecision) -> Result<(), SessionError>;
    async fn interrupt(&mut self) -> Result<(), SessionError>;
}
```

The generic io-task loop (one copy, in `session-lib`) owns the
command_rx/event_tx select, the turn-lifecycle bookkeeping that's agent-neutral,
and calls the driver for protocol specifics. Agent-specific *orchestration* that
genuinely differs (Codex's turn_active/queued_prompts/echo synthesis, the
TurnCompleted metrics finalize, CodexThreadId) stays in the codex driver until
proven generalizable — don't force it early.

## Migration slices (incremental, each behavior-preserving + cross-reviewed)

1. **Output neutralization.** Add `IoEvent::Classified(AgentOutput)`. Claude
   io-task classifies and emits it; Session grows the neutral arm (keep the old
   arms working in parallel). Then switch Codex io-task to emit `Classified`
   and delete `CodexPermissionRequest` + the codex Session arms. Finally delete
   `IoEvent::Output(ClaudeOutput)`. (Several sub-slices.)
2. **Input neutralization.** Introduce neutral `IoCommand::{UserInput,
   Permission, Interrupt}`; route `Session::send_input`/`respond_permission`
   through them; each io-task translates. Remove `ClaudeInput` from `IoCommand`
   and the `agent_type` branch. Revive `AgentAdapter`'s input methods (async) as
   the translation hooks, or fold them into the driver.
3. **Runtime collapse.** Extract the agent-neutral io-task loop into
   `session-lib`; reduce `claude-session-lib`/`codex-session-lib` to driver
   impls. This is where "collapse codex-session-lib" becomes honest — only do it
   once 1–2 prove the neutral boundary holds.

## Open questions for Codex

1. **Classify in io-task vs Session?** This doc proposes io-task (makes
   `IoEvent` neutral cleanly, unifies both agents). It moves Claude's classify
   out of `Session` — acceptable? Alternative keeps it in Session but then
   `IoEvent::Output` must stay agent-typed and codex can't converge.
2. **Async driver trait** — `async_trait` (extra dep) vs hand-rolled
   `Pin<Box<dyn Future>>` vs keeping the io-task per-agent and only sharing the
   neutral command/event enums (lighter; skips slice 3).
3. **Keep-as-is variants:** `TurnMetricsReady`, `CodexThreadId`,
   `SessionLimitReached` are orchestration signals, not output. Leave them as
   explicit `IoEvent` variants (agent-neutral envelopes)?
4. **Scope cut:** is slices 1+2 (neutral Session boundary, both backends still
   separate io-tasks) enough value to stop at, deferring slice 3 (the shared
   runtime) indefinitely? Slice 3 is the highest-risk, lowest-marginal-return.

## Non-goals

- Changing wire formats, the proxy↔backend protocol, or frontend rendering.
- Unifying turn-metrics/token accounting (separate concern, stays in drivers).
