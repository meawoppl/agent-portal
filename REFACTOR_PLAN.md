# agent-portal Refactor Plan

The 10 highest-impact refactor/cleanups, with an agreed work breakdown.
**Consensus plan — Claude + Codex** (each independently analyzed the repo; the
findings converged ~1:1, and the breakdown below is mutually agreed).

## Ownership (split by directory to avoid collisions)

| Owner | Area |
|---|---|
| **Claude** | `backend/`, `shared/`, `claude-session-lib/`, `codex-session-lib/`, `session-lib/`, `proxy/`, `launcher/` |
| **Codex** | `frontend/` |
| **Coordinated** | the agent-neutral types in `shared/` (item #2) — handled as a *contract PR* (Claude drafts, Codex reviews) before dependent work, like the RPS protocol PR |

## The 10 items

| # | Item | Owner | Effort | Risk |
|---|------|-------|:---:|:---:|
| 1 | Cross-agent **session abstraction** — neutral `AgentInput/Output/PermissionRequest/TurnMetrics` at the session boundary; collapse claude/codex-session-lib duplication (~2k LoC) | Claude | L | Med |
| 2 | **Agent-neutral protocol naming** — neutral envelopes + serde aliases for wire compat | Claude (contract) + Codex (fe consumers) | L | High* |
| 3 | **Frontend renderer unification** — one parsed `AgentFrame` + renderer registry; merge `message_renderer/*` and `codex_renderer.rs` (~1.7k LoC) | Codex | L | Med-High |
| 4 | **Split `backend/main.rs`** into bootstrap/config/routes/background-jobs | Claude | M | Med |
| 5 | **Finish shared protocol modularization** — split `endpoints.rs`, finish `api/` split, add per-endpoint serde round-trip tests | Claude | S/M | Low |
| 6 | **Backend service/repository layer** for sessions/messages/metrics; ownership/retention/replay centralized, typed errors, `get_db_conn` helper | Claude | L | Med |
| 7 | **proxy ↔ shim consolidation** — shared reliable-output-forwarder/process bridge | Claude | L | High |
| 8 | **`markdown.rs` pipeline split** + focused tests (math/code/link interactions) | Codex | M | Med |
| 9 | **Dashboard state → reducers + hooks** (+ a `use_fetch<T>()` hook) | Codex | L | Med-High |
| 10 | **Panic/clone cleanup** in production paths + a clippy deny policy | Both (own area) | M | Low-Med |

\* #2 is low-risk *if* staged behind serde aliases + the #5 round-trip tests.

## Phases (risk- and dependency-ordered)

- **P0 — opener (low risk):** #5, *pure module extraction + serde round-trip
  tests only — **no public type renames** in this phase* (keeps it safe and
  makes the later #2 review clear). → Claude.
- **P1 — contract:** #2 *neutral names first, old wire names preserved via
  aliases/compat constructors, then migrate consumers* (so frontend and backend
  stay independently movable). Claude drafts the `shared/` contract PR; **Codex
  reviews it before any serious renderer surgery (#3)**. Then #1 session
  abstraction (Claude).
- **P2 — parallel:** #4 backend main split (Claude) ‖ #3 renderer unification (Codex).
- **P3 — parallel:** #6 service layer (Claude) ‖ #8 markdown + #9 dashboard (Codex).
- **P4 — last (careful):** #7 proxy/shim. Highest chance of subtly breaking real
  user/VS-Code workflows; benefits from all the earlier session/protocol cleanup.
- **Continuous:** #10 — agree one small **clippy deny-policy PR up front**, then
  each owner cleans their own area incrementally.

## PR discipline

- Small, focused PRs per sub-task (match the repo's `#952`–`#956` style).
- **The other agent reviews before merge**; CI must be green; rebase often.
- **Announce shared-surface PRs in chat** before opening them.
- For **#3 and #8 (renderer/markdown), add fixture/snapshot-style rendering
  tests *before* large movement** — renderer refactors are too easy to green-CI
  while silently changing UI behavior.

## Open questions — resolved

1. **Keep #2 separate from #1?** Yes — naming + serde-alias is mechanical;
   the abstraction is structural. Run #2 immediately before #1.
2. **#7 scheduling?** Stays rank-7 by impact but **scheduled last** for risk.
3. **Frontend ownership?** Codex owns all of `frontend/`; no trade needed.

## Near-misses (folded in as sub-tasks)

device_flow HTML/handler split (→ #4/#6), exponential-backoff consolidation into
a shared util (→ #1), the child-dispatcher registration pattern (frontend),
`performance_panel` query extraction (→ #9).

---

# Round 2 — Next 10 (consensus 2026-06-25, Claude + Codex)

Round 1 (above) + token-accounting, SDK modernization, pill-ordering, and the
record-level message-provenance rebuild have all landed. This round was scoped
by **reading the code broadly** (4 parallel survey agents over
backend / session-libs / frontend / proxy+launcher) + a Codex repo/CI scan, not
just the issue tracker. Organized by **outcome**, not issue number.

## Reliability (top priority — user-visible dead sessions / leaks)
1. **WS data-plane reconnect after backend restart** (#926) — sessions go dead
   silently.
2. **Child-process / zombie cleanup on stop/cancel** (#927) — survey found 4
   concrete leak sites: `proxy/src/shim.rs` stdin-EOF orphans the stdout/stderr
   reader tasks (and reconnect `continue` doesn't cancel the reader task);
   `launcher/src/process_manager.rs:~238` `task.handle.abort()` cancels the
   tokio task but doesn't SIGKILL the claude child; `scheduler` keeps stale
   `running` entries when `SessionExited` is lost. Fix with a `CancellationToken`
   per connection + explicit `kill()` before abort + a scheduler age-out.
3. **UI→proxy delivery semantics / ack idempotency** (#939) — plus
   `launcher/src/connection.rs` can drop a `SessionExited` on disconnect, so the
   scheduler relaunches a dead session. Wants a buffered at-least-once story.

## Delivery velocity (dev-loop multipliers)
4. **Version bump on merge, not per-PR** (#1096) — killed most of our parallel-PR
   coordination tax. **+ CI frontend-rebuild 3×:** the backend embeds
   `frontend/dist`, so the clippy / test / backend jobs each `trunk build` the
   frontend every run. Add a no-dist Rust-check mode (or a built-dist cache
   artifact) so pure-Rust checks don't rebuild WASM.

## Testability
5. **Migration apply/revert harness** on a disposable DB + invariants for
   destructive migrations (folds #922) — the provenance `DELETE FROM messages`
   was safe but relied on *manual* FK reasoning. Plus **characterization tests**
   for the session core (`session-lib/src/session.rs::next_event` has 1 test)
   and the missing backend auth/registration coverage
   (`registration.rs::user_is_authorized_for_session` is untested).

## Architecture
6. **io_task-fold → generic `Session<A>` → collapse `codex-session-lib`** (#1) —
   survey: ~60-80 lines of duplicated terminator/finalization logic across the
   two `io_task.rs`. Characterization-tests-first. *(Holds on Matt's go for the
   detection→adapter / retry-action→io_task split.)*
7. **Dashboard state → reducers + hooks** (#920/#9) — `page.rs` carries 24
   `use_state` + ~114 cloning callbacks → O(sessions) full-tree re-renders per
   event; move per-session state into `SessionView`.
8. **Renderer split by family + `AgentFrame` as the only parse point** (#859) —
   survey: JSON re-parsed per render (`dispatch.rs`) and the sparkline tick
   re-renders all pills every 100ms.

## Observability
9. **Structured lifecycle events/metrics** — reconnect attempts, input-ack
   latency, child spawn/kill, message provenance. Not an existing issue but the
   thing that makes #926/#927/#939 *diagnosable*. Split by event source
   (backend / proxy / launcher = Claude; frontend = Codex).

## Correctness / polish (opportunistic, picked by domain — don't block architecture)
10. **#1067** worktree branch detection (pin-to-launch + best-effort + SDK
    issue), **#827** Codex file-change permission filenames, **#1076**
    AskUserQuestion checkbox-clear.

## Cross-cutting policy (continuous, not ranked projects)
- Panic/`unwrap` cleanup in production paths (survey flagged `heartbeat.rs`
  mutex-poison via `.lock().unwrap()`, `proxy_tokens.rs` token-expiry unwrap,
  `background.rs:151` swallowed DB error via `.unwrap_or_default()`); add a small
  clippy deny baseline, then clean per touched area.
- Typed SDK upstreaming (claude-codes / codex-codes) over JSON-poking.
- Split oversized modules (`shared/api`, `endpoints`, frontend monoliths) **when
  touched**, not as a standalone sweep.
- Keep `node_modules` / generated assets out of analysis/line-count paths.

## Ownership
- **Claude:** reliability (#926/#927/#939), CI+migrations (#1096, #4, #5),
  session abstraction (#1), backend observability.
- **Codex:** dashboard reducers (#920), renderer split (#859), frontend
  correctness (#827/#1076), frontend observability.
- **Both:** #1067 by domain; cross-cutting policy in each owner's area.

## Sequencing (agreed)
- **Reliability ships in thin, independently-mergeable slices, first:**
  #926 (reconnect — characterization then fix) → #927 (process cleanup) →
  #939 (delivery semantics). Don't mix hot reliability fixes with the generic
  `Session<A>` refactor.
- **#1 io_task-fold stays behind the reliability trio** (and behind its own
  characterization tests).
- **Observability (#9) is folded into the reliability fixes** where it makes them
  diagnosable — not one giant metrics PR.
- **Codex (parallel):** #920 reducers → #859 renderer split, with #1076 as a
  small interleave.
