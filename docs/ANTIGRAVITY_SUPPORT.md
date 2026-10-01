# Antigravity support

## Goal

Run Google Antigravity conversations as first-class Agent Portal sessions while
preserving Portal's invariants: typed agent selection, durable final output,
ephemeral streaming updates, interruption, resume, performance telemetry, and
an honest install/credential status in the UI.

Antigravity is materially different from the other runtimes. Its
`localharness` process uses a length-prefixed protobuf handshake on stdio and
then moves to an API-key-authenticated loopback WebSocket. Portal therefore
integrates it as a fourth `Session<A>` backend through `antigravity-codes`, not
as a Claude/Codex JSON adapter.

## Preview delivered in this slice

- `AgentType::Antigravity` is carried through shared wire types, the launcher,
  scheduling, launcher probes, settings, session pills, and performance data.
- `antigravity-session-lib` launches `localharness` through
  `antigravity-codes 0.1.20`, assembles streaming steps, and maps them onto
  neutral `IoEvent`s.
- The default model is `gemini-flash-latest`; the shared picker emits
  `--model <name>`. Unknown models remain usable through Extra arguments.
- Final steps are durable; active step snapshots are ephemeral and do not
  inflate history. User-input echoes are suppressed because Portal already
  owns that row.
- A stable `antigravity_step` envelope keeps both a renderer-friendly summary
  and the typed native `StepUpdate` under `update` for later richer cards.
- Turn terminals are explicit, so awaiting state and metrics attach to the
  correct turn. Gemini prompt/candidate/cache/thought token deltas flow into
  existing performance views.
- Conversations use the Portal session UUID as Antigravity's cascade id and a
  persistent per-user data directory, allowing launcher restarts to resume.
  The harness's returned id must match exactly; a mismatch fails closed rather
  than risking attachment to a different conversation.
- Portal's generic stop path aborts an agent task, while Antigravity persists
  its cascade only after acknowledging `session_end`. The preview therefore
  shuts the harness down with a 10-second bound after every accepted terminal
  turn and relaunches the same cascade for the next input. The SDK warms its
  step assembler from replayed initialize history without re-emitting it, so
  this is durable without duplicating the Portal transcript.
- Installation uses the PyPI package. Discovery checks
  `ANTIGRAVITY_HARNESS_PATH`, `localharness` on `PATH`, and the binary embedded
  in an installed Python wheel.
- Credential readiness currently means a non-blank `GEMINI_API_KEY` in the
  launcher's environment.

## Safety boundary

The preview enables only the harness's read-only built-in tools. Antigravity
can request tool confirmation and user questions, but `antigravity-codes::Client`
answers those through in-process handlers rather than exposing a pending request
to Portal. Its safe default refuses confirmation. Enabling create/edit/shell
before bridging those requests into Portal's permission UI would bypass the
product's approval contract, so the launch and schedule forms deliberately do
not offer a skip-permissions switch for Antigravity.

## Error paths

| Failure | Detection | User-visible behavior | Recovery |
|---|---|---|---|
| Wheel/harness absent | launcher probe and SDK launch | “not installed” matrix state or a persisted launch error | Install `google-antigravity`, then refresh probes |
| `GEMINI_API_KEY` absent | before process spawn | explicit persisted error naming the missing variable | configure the launcher service environment and restart it |
| Handshake/socket/init failure | `Client::launch` | harness diagnosis (including captured stderr) is persisted | correct binary/model/credentials; resume/relaunch |
| Prompt rejected | `Client::send` | delivery acknowledgement fails and a transcript error is emitted | correct configuration and retry |
| Turn failure | `Turn::next_step` | completed output remains visible, followed by failure and a failed terminal | retry a new turn |
| Interrupt | `Client::cancel`, then drain to cancelled/idle with a 10 s bound | cancelled terminal; timeout becomes a visible error, then bounded shutdown | send another turn; it relaunches the persisted cascade |
| Persistence acknowledgement stalls | bounded `Client::shutdown` after a terminal turn | visible persistence error; dropping the client kills the harness | retry from the last acknowledged cascade state |
| New/unknown native fields | preserved native update in `antigravity_step.update` | known summary still renders; evidence remains in history | extend SDK types/renderer from captured payload |

## Follow-up work before removing “experimental”

1. Add a neutral pending-question/confirmation channel and map Antigravity's
   handlers onto Portal modals. Only then offer write/shell tools.
2. Support Vertex ADC with provider/project/location fields instead of assuming
   the Gemini Developer API.
3. Persist the actual cascade id learned at initialization rather than relying
   on the requested Portal UUID remaining canonical across upstream versions.
4. Render action-specific cards for file/search/command/subagent steps and
   expose live in-progress overlays without duplicating final history.
5. Account for per-trajectory subagent usage and add model context-window data
   when the upstream protocol reports it.
6. Exercise Windows/macOS wheel discovery and a real authenticated end-to-end
   run in CI or an opt-in harness test lane.

## Feedback for `antigravity-codes`

The crate made the transport integration small and correctly preserves raw
protobuf-JSON evidence. Two API improvements would materially help host apps:

1. `Client::send` returns `Turn<'_>`, which exclusively borrows the client,
   while cancellation is `Client::cancel`. A UI must drop the turn, call
   cancel, then manually drain raw events to the main trajectory's terminal
   state. A `Turn::cancel_and_drain()` (or split command/event handles) would
   preserve step assembly, handlers, and usage accounting during interrupts.
2. The harness PID is not exposed through `Client`/`RawClient`/`Harness`.
   Portal can still stop it through ownership/drop, but cannot include it in
   its process-group shutdown and orphan diagnostics. A read-only `pid()`
   accessor would close that observability gap.

Model catalog ownership is correctly absent from the protocol crate; Portal
therefore offers only the SDK-documented known-good alias and leaves arbitrary
model names in Extra arguments.
