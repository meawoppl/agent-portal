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
