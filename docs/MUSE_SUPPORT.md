# Muse Code Support Plan

This document plans the changes needed to run Meta Muse Code sessions in
agent-portal alongside Claude Code and Codex, as a sequence of independently
shippable PRs. The SDK layer already exists:
[muse-codes](https://crates.io/crates/muse-codes) (same
[repository](https://github.com/meawoppl/rust-code-agent-sdks) as
claude-codes / codex-codes), and the integration advisement in
[#1560](https://github.com/meawoppl/agent-portal/issues/1560) is the
protocol-level source for this plan.

Crate pin for all PRs below: `muse-codes >= 0.1.3` (tested against Muse
Code 0.1.0, build `0.1.0-R708.1`). Pin the rev in `Cargo.lock`, name it in
the commit, log it at launcher boot — the standing provenance contract.

## Protocol Comparison

Muse is a third protocol shape, distinct from both existing agents:

| | Claude Code | Codex | Muse Code |
|---|---|---|---|
| Wire | role-tagged JSON messages | thread/turn/item events | **event-sourced journal** (envelope + payload) |
| Process model | spawn per turn, `--resume`/session id | long-lived app-server, many turns | **spawn per turn**, `muse exec --session-id <uuid>` |
| Ordering/dedup | uuid bookkeeping | implicit | composite `(stream_id, id)`; both `id` and `sequence` repeat across streams/turns — see corrections |
| Tools | content blocks in messages | first-class items | **task streams** with a lifecycle state machine |
| Approvals | `control_request` round-trip | `ServerMessage::Request` round-trip | **none headless** — policy decisions are journaled (`side_effect_intent.policy_decision`), not asked |
| Streaming text | assistant deltas | `agent_message` items | `run.output.delta` (ephemeral) reconciled by `run.terminal.completed`'s full final text |
| Model identity | init message | thread config | **in-band**: `run.model.configured` at run start |
| Credential-free testing | no | no | **yes** — `--provider echo` emits the full event stream |

Envelope (every stdout line, typed as `MuseRecord`): `schema_version`,
`stream {kind: session|run|task, id}`, `sequence`, `recorded_at` (µs),
`record_type` (reconciliation|event|status), `durability`
(durable|ephemeral), `causation_id`, `payload_type`, raw `payload` lifted
on demand via `typed_payload()` → `MusePayload` (20 typed payload kinds;
`MusePayload::Unknown` preserves anything new with its dotted type label).

**Render rules that fall out of the journal shape** (from #1560):

- Durable events are the transcript; ephemeral/status records
  (`run.output.delta`, `task.lifecycle.status`) are streaming UI only —
  never persisted as transcript rows.
- Tasks render as a collapsible tree keyed on `task_id`
  (`task.stream.linked` opens a node; `task.lifecycle.*` walks
  `proposed → accepted → started → (scheduled → side_effect_intent →)
  completed | cancelled | rejected | failed`; `tool.result` rows carry
  `correlation_facts {outcome, tool_name}` and optional `edit_facts`).
- Unknown frames must still name themselves: render `payload_type` as the
  label plus raw JSON body (the `conversation_reset` lesson).

**Out of scope, by design boundary**: the on-disk session journal
(`~/.local/share/muse/sessions/**/session.jsonl`) uses a *different* nested
wrapper format. It is a replay/forensics concern, not the live-session
proxy path. Do not conflate the two; if wanted later it is a separate
module in muse-codes, not an extension of `MuseRecord`.

## PR sequence

Each PR is independently shippable and verifiable; later PRs depend on
earlier ones as noted. Echo-provider runs make every PR testable in CI with
zero credentials — use that everywhere.

### PR 1 — `shared`: `AgentType::Muse` + launcher probe

Smallest possible enum-and-probe change, unblocks everything else.

- `shared`: add `Muse` to `AgentType` (`as_str() = "muse"`, parse, serde).
  Audit every `match` on `AgentType` — the compiler finds the sites
  (~58 references across backend/launcher/frontend today); most gain a
  `Muse` arm that mirrors Claude's (spawn-per-turn family), a few
  short-circuit (no approval relay).
- `launcher`: extend `ProbeAgents` to detect the `muse` binary
  (`muse --version` → `"Muse Code 0.1.0 (0.1.0-R708.1)"`), and the login
  cell via `muse_codes::auth::credentials_present()` + `AuthFile` parse —
  label is `"logged in (meta)"` (+ optional `via env` when `META_API_KEY`
  set); no account name exists at the CLI level (0.1.0 has no whoami).
- **Probe the sandbox**: `muse` tool execution requires bubblewrap on
  Linux. A host where `muse` is installed but the sandbox probe fails
  should surface as *installed-but-degraded* in the computer×agent matrix,
  not as healthy. (Observed failure mode: runs complete, every tool call
  returns `tool.result` with `outcome: failure` — confusing without the
  matrix warning.)
  - **Agreed shape** (with the matrix owner): `shared::AgentInstall` gains
    an additive `sandbox_ok: Option<bool>` — `None` for agents with no
    sandbox concept (claude/codex serialize unchanged), `Some(false)` =
    installed-but-degraded, `Some(true)` = ready. Degraded is NEVER
    modeled as `installed: false`.
- Coordinates with the login-matrix work (Settings pane PR1): the matrix
  cell shapes for muse land here.

Verify: probe unit tests; matrix renders the muse column on a host with and
without the binary.

Sequencing note: the Settings-pane matrix (#1561), login buttons (#1562),
and install helpers shipped claude/codex-only ahead of this plan; once this
PR lands, the muse arms fold into those surfaces as a portal-side follow-up
(device-code slots into the existing `LoginPresentable::DeviceCode` +
poll shape).

### PR 2 — `muse-session-lib`: the session proxy crate

New workspace member mirroring `codex-session-lib`'s module layout
(`agent.rs`, `classifier.rs`, `events.rs`, `handler.rs`, `io_task.rs`,
`helpers.rs`), but modeled on the **Claude process pattern**, not the
Codex one:

- `MuseAgent`: one `muse exec --json --session-id <uuid>` spawn per user
  turn via `MuseExecBuilder` (cwd, provider, model); `ExecRun` streams
  `MuseRecord`s; process ends at `run.terminal.*`; next turn respawns with
  the same session id. Kill-on-drop covers interrupts (there is no
  interrupt protocol — killing the child *is* the interrupt; the journal's
  restart-safety makes this clean).
- `classifier.rs`: `MuseRecord` → portal event records. Mapping table:
  - `turn.input.user` → user-message echo (dedup against the submitted
    prompt, like claude's replay ack)
  - `run.output.delta` → streaming text frame (ephemeral)
  - `run.terminal.completed` → final assistant message (reconcile: replace
    accumulated deltas with terminal `text`) + turn end
  - `run.model.configured` → session header metadata (model/profile)
  - `task.*` family → task-tree events (see PR 4)
  - `tool.result` → tool outcome event
  - `MusePayload::Unknown` → passthrough event `{label: payload_type,
    body: payload}`
- Identity plumbing — **corrected by measurement, see below**: key events on
  the composite `(stream_id, id)` (the record `id` is a counter that
  repeats across sessions), group a turn by `causation_id`, and use
  `sequence` only to order records *within* one turn.
- Tests: **echo-provider round-trips as the integration suite** (spawn
  real `muse`, no credentials) + the muse-codes committed corpus replayed
  through the classifier as fixtures.

Depends on PR 1 (AgentType). Verify: `cargo test -p muse-session-lib` incl.
live echo runs in CI (install muse in the workflow the same way the
sdk repo's `muse-schema-drift.yml` does).

### PR 3 — backend: session lifecycle + persistence

- Route `AgentType::Muse` sessions to `muse-session-lib` in the session
  supervisor; session id minted portal-side and passed via
  `--session-id` (muse accepts caller-supplied uuids — same pattern as
  claude's `--session-id`).
- Persistence: transcript rows from durable events only, keyed on the
  composite `(stream_id, id)` — **not** `id` alone and **not**
  `(stream_id, sequence)`; both collide (see the corrections section),
  with `causation_id` for turn grouping and `sequence` for intra-turn
  order. DB migration: agent column already stores a string —
  confirm no enum constraint blocks `"muse"`.
- Turn semantics: a turn = submit → spawn → terminal record → exit. Child
  exit without a terminal record surfaces as a typed failure (the
  muse-codes client already folds exit code + stderr into the error).
- No approval relay: muse's `side_effect_intent.policy_decision` is
  recorded and rendered but never blocks (no round-trip exists headless).

Depends on PR 2. Verify: backend integration test drives a full
echo-provider session through the HTTP/WS surface.

### PR 4 — frontend: rendering

- Streaming pane: deltas stream, terminal text reconciles (mirror the
  claude delta/result pattern).
- **Task tree component** (the genuinely new UI): collapsible nodes keyed
  on `task_id`, lifecycle badge per state (incl. `cancelled`/`rejected`
  with reasons), `status` events as transient progress lines (they carry
  model-stream retry telemetry), `output` chunks inside the node,
  `tool.result` rows with outcome/tool-name and `edit_facts` diffs when
  present.
- Session header: model/profile/provider from `run.model.configured`.
- Unknown passthrough renderer: label + JSON body, matrix-style graceful.
- Policy decisions rendered as audit rows (distinct styling from
  approvals, since the user was never asked).

Depends on PR 3. Verify: storybook/fixture renders from the committed
muse-codes corpus (all 20 payload types appear in it).

### PR 5 — login flow (folds into Settings-pane PR2)

Muse's is the easiest of the three flows: `DeviceLoginFlow::start()` →
`device_code(timeout)` → relay `{verification_url, code}` (serde) to the
browser → `wait_approved(timeout)` → matrix cell flips. Plus
`auth_set(api_key)` as the CI/API-key path (key travels stdin, never
argv). Cancellation = drop. All five relay-contract constraints already
hold (serde presentables, parkable handle, reaping drop, caller timeouts,
version pins).

Depends on PR 1; independent of PRs 2–4. Lands as the muse arm of the
existing login-buttons plan.

### PR 6 — e2e, docs, deploy

- End-to-end: browser-driven echo-provider session on a staging launcher
  (create session → prompt → task tree renders → terminal text). This is
  the only agent where full e2e needs no vendor credentials — make it the
  CI gate.
- Meta-provider smoke test stays manual/staging (needs a real credential;
  keep it out of CI).
- Docs: DEVELOPING/DEPLOYING notes — muse install (installer script,
  no self-updater at 0.1.0: re-run installer to update), bubblewrap
  requirement on launcher hosts, credential paths.
- Boot provenance line gains the muse-codes rev.

## Measured corrections (echo-provider experiments, 2026-08-06)

Two assumptions in the first draft of this document were tested against the
real CLI before any classifier code was written. One held; one did not.

**`sequence` is NOT unique across turns — do not key persistence on it.**
Three turns on a single `--session-id` produced: turn 1 `seq 1..33`, turn 2
`seq 2..34`, turn 3 `seq 3..35` — all carrying the same `stream.id` (the
session id). Consecutive turns therefore collide on **32 of 33** sequence
values. An earlier draft of this plan called `(stream_id, sequence)` the
"native dedup/ordering key"; that would have silently overwritten or
dropped prior-turn records. The record **`id`** is unique
*within* a stream (99/99 distinct across the three turns) — but **not
across streams**, see below. The persistence key is therefore the
composite **`(stream_id, id)`**; `causation_id` identifies the turn; and
`sequence` is intra-turn ordering only.

**Record `id` is a UUID-shaped counter, not a UUID — never key on it
alone.** Two runs under *different* session ids produced **byte-identical
id lists**: 33 of 33 collisions, every session starting at
`018f0000-0000-7000-8000-00000000c350` and incrementing (`…c351`,
`…c352`, a hex counter in the low bits under a constant prefix). Keying on
bare `id` would not merely risk a collision — it would collide on every
record of every session and silently overwrite one transcript with
another. The UUIDv7 *shape* is a trap for anyone who assumes global
uniqueness from appearance; always composite with `stream_id`.

**Interrupt-as-kill is safe.** A run SIGKILLed 60 ms in (before it emitted
any output) left the session store usable: the next turn on the same
session id ran to a clean `run.terminal.completed`. No corruption, and no
"session resumed / possible gap" seam is required. (A first attempt killed
at 350 ms proved nothing — the echo run had already completed in ~250 ms —
and was re-run rather than counted as a pass.)

## Risks / open questions

- **Beta CLI, day-one protocol**: Muse Code is days old; expect wire
  drift. Mitigation already in place: the sdk repo's nightly fingerprint
  workflow files issues on stream changes, and `MusePayload::Unknown`
  means new payload types render (labeled) instead of breaking. Portal
  should treat Unknown-rendering as normal operation, not an error state.
- **Multi-turn `--session-id` semantics** are verified flag-level but not
  yet exercised across many turns with a live provider — PR 3 should
  include a two-turn meta-provider staging test before enabling broadly.
- ~~**Interrupt = kill**: confirm muse tolerates mid-run kills.~~
  **Resolved** — measured safe; see corrections above. Keep the echo
  kill/respawn test in plan-PR-2's suite as a regression guard.
- **Sandbox variance across launcher fleet**: the bubblewrap requirement
  makes muse the first agent whose *tool* capability depends on host
  packages. The PR 1 degraded-state cell is the mitigation; deploy docs
  must list the package.
