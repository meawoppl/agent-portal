# Changelog

## 2.5.39

- **Decommingle `claude-session-lib` into three crates.** `claude-session-lib` previously housed both `claude_io_task` and `codex_io_task` along with codex-specific helpers and an `IoEvent::CodexPermissionRequest` variant — the crate name lied. Split:
  - New **`session-lib`** owns the agent-agnostic core: `Session<A: Agent>` generic, `trait Agent` with `spawn_io_task`, the unified `IoCommand` / `IoEvent` / `SessionEvent` enums, `SessionConfig` / `SessionSnapshot` / `PendingPermission`, `OutputBuffer`, `PendingOutputBuffer`, `HeartbeatTracker`, `probe_all_agents`, `SessionError`.
  - **`claude-session-lib`** now contains only Claude-specific bits: `pub struct ClaudeAgent` with `impl session_lib::Agent`, `claude_io_task` (incl. the upstream-429 rate-limit retry state machine), `spawn_claude`, and `proxy_session/*` (wiggum mode, portal reminder, image upload, ws_reader). The `proxy_session` connection loop stays here and is now generic over `A: Agent`, so both Claude- and Codex-backed sessions can use it; splitting it is deferred to a future PR.
  - New **`codex-session-lib`** contains `pub struct CodexAgent`, `codex_io_task`, and `handle_codex_server_message` (the existing stringly-typed `match method.as_str()` dispatch is preserved verbatim — issue #723 will refactor to typed `ServerRequest` matching in a follow-up PR).
- Launcher's `process_manager.rs` adds an `AnySession` enum (`Claude(Session<ClaudeAgent>)` / `Codex(Session<CodexAgent>)`) so a single in-process map can hold mixed-agent sessions; the dispatch happens at `match config.agent_type` in `AnySession::new`.
- Proxy keeps using `Session<ClaudeAgent>` directly — the proxy binary never spawns Codex.
- `SessionError::ClaudeError(claude_codes::Error)` is gone — per-agent crates now collapse SDK errors into `SessionError::Agent(String)` so `session-lib`'s error surface stays SDK-free.

## 2.5.38

- CLAUDE.md: codify typed-interface preference + upstream-first rule, lifted from the codex `item/fileChange/requestApproval` regression in 2.5.38 / PR #721.

## 2.5.37

- **Bump `codex-codes` 0.129.2 → 0.129.3 and absorb the SDK rewrite.** Upstream 0.129.3 (PR #138) regenerated every wire type from the schema and shipped [SDK #134](https://github.com/meawoppl/rust-code-agent-sdks/issues/134)'s structured `ParseError`. Three concrete consequences for agent-portal:
  - **`codex_io_task` rewrite** in `claude-session-lib/src/session.rs` against the new API: `ThreadStartParams` lost its `Default` impl (15 Option fields, all `skip_serializing_if`), so we now build it via `serde_json::from_value(json!({}))` matching the SDK's own usage example; `ThreadStartResponse::thread_id()` is gone — use `resp.thread.id`; `TurnInterruptParams` now requires both `thread_id` and `turn_id`, so we track the active turn id from each `turn/started` notification and pass it on interrupt; `TurnStartParams::reasoning_effort` is now `effort` and the struct gained more Option fields, so we build it the same way; `UserInput::Text` gained a required `text_elements: Option<...>` we set to `None`.
  - **`CodexFrameCaptureLayer` retired** (the tracing-snooping workaround from 2.5.30 — which didn't actually capture in production anyway). The `Error::Json` arm in `codex_io_task` becomes `Error::Deserialization(ParseError)` and reads `raw_line` / `raw_json` / `method` / `error_message` directly off the struct. The portal message gets a real fenced ```json block with the offending frame, complete with the JSON-RPC `method` name. Supersedes the long-pending #708 revert PR.
  - **Cleanup:** removed `claude-session-lib/src/codex_frame_capture.rs`, its module export from `lib.rs`, the install lines in `launcher/src/main.rs` and `proxy/src/main.rs`, and the `tracing-subscriber` dep from `claude-session-lib/Cargo.toml`. Per-layer EnvFilter rewiring reverted to the original registry-level EnvFilter.
- **Net diff** is a small set of one-line API shape changes + one architectural pull-out, exactly proportional to a major SDK refactor.

## 2.5.36

- Session pill agent-type watermark (Anthropic asterisk / OpenAI knot) is now 10% smaller — 88×88 → 80×80 — with the left offset adjusted from −28px to −24px so the icon's horizontal center stays at the same x=16 in pill coords. Vertical center is unchanged.

## 2.5.35

- **Rate-limit retry: bump cap from 4 attempts/30s to 30 attempts/60s.** The original 2.5.29 settings gave up after ~1 minute of sustained throttling, which was too short for the rate-limit windows Anthropic actually applies (multi-minute holds are common). With 30 attempts at full-jitter exponential backoff capped at 60s, the upper bound on retry wall time is ~30 minutes of patience before we surface the "send your message again" portal note — and any earlier success short-circuits, so well-behaved limits unblock as soon as the window opens. Counter still resets on every fresh user input and every successful turn.

## 2.5.34

- **Bump `codex-codes` 0.129.1 → 0.129.2** and wire it through `codex_io_task`. The dep bump picks up [SDK #135](https://github.com/meawoppl/rust-code-agent-sdks/issues/135) (`AppServerBuilder::config_override` + `extra_args`). The call-site change parses each codex session's `SessionConfig::extra_args` (the existing "Extra args" launch-dialog text input — already wired through to the launcher) into two buckets:
  - tokens of the form `-c key=value` / `--config key=value` become `builder.config_override(k, v)` (rendered as `-c k=v` **before** the `app-server` subcommand, since `-c` is a global codex flag);
  - everything else becomes `builder.extra_args(...)` (appended **after** `--listen stdio://`).
  - Lets a user type e.g. `-c sandbox_mode=workspace-write -c approval_policy=on-request --strict-config` in the launch dialog and have each token land in the right slot on the spawned codex command line. No structured UI for sandbox/approval pickers yet — that's a follow-up.

## 2.5.33

- **Route the codex 0.130+ message types to user-facing UI instead of letting them fall through to "Unknown Codex request".** Followup to 2.5.32 which got the typing in. Each new message type now has a purpose-built dispatch arm:
  - **Approval requests** wire into the existing `CodexPermissionRequest` event with sensible \`tool_name\`s and trimmed input payloads, so the user gets the existing approve/deny dialog for each:
    - \`execCommandApproval\` → \`tool_name: "ExecCommand"\` (command + cwd + parsedCmd)
    - \`applyPatchApproval\` → \`tool_name: "ApplyPatch"\` (file-change map + grant-root + reason)
    - \`item/permissions/requestApproval\` → \`tool_name: "Permissions"\` (cwd + permissions profile + reason)
    - \`item/tool/requestUserInput\` → \`tool_name: "AskUserQuestion"\` (reuses the Claude AskUserQuestion renderer — the Codex question shape is structurally compatible)
    - \`mcpServer/elicitation/request\` → \`tool_name: "McpElicitation"\` (server name)
  - **Internal/system requests** (no user action to take) emit a portal text message so the user sees what was requested without blocking the agent:
    - \`item/tool/call\`, \`account/chatgptAuthTokens/refresh\`, \`attestation/generate\`
  - **Notifications**:
    - \`deprecationNotice\` / \`guardianWarning\` → portal text (high-visibility)
    - \`item/plan/delta\`, \`turn/plan/updated\`, \`turn/diff/updated\`, \`item/reasoning/summaryPartAdded\`, \`item/reasoning/textDelta\`, \`item/fileChange/patchUpdated\` → typed structured events the frontend can opt into rendering (currently fall through to the existing raw-codex display but with a stable \`type\` tag for follow-up renderers)
    - Pure-status notifications (\`mcpServer/oauthLogin/completed\`, \`account/login/completed\`, etc.) stay silent (debug log)
- **Frontend**: extended \`format_permission_input\` in \`frontend/src/pages/dashboard/types.rs\` so the new tool_names (\`ExecCommand\`, \`ApplyPatch\`, \`Permissions\`, \`McpElicitation\`) get readable one-line summaries in the permission card instead of dumping a JSON blob.

## 2.5.32

- **Bump `codex-codes` 0.128.0 → 0.129.1.** Upstream added eight new `ServerRequest` variants and roughly ten new `Notification` variants modeling codex CLI 0.130+ protocol additions (tool-input prompts, MCP elicitations, generic permission requests, dynamic tool calls, ChatGPT auth-token refresh, attestation, apply-patch approval, exec-command approval, plus plan/diff/reasoning delta notifications). Extended the proxy's `ServerRequest` match in `claude-session-lib/src/session.rs` to cover all of them — each new variant serializes its typed param struct back to `Value` so the downstream string-dispatch keeps working. New approval-type methods (`ApplyPatchApproval`, `ExecCommandApproval`, `PermissionsRequestApproval`, etc.) currently fall through to the existing `_ => warn!("Unknown Codex request: {}", method)` arm, which surfaces them as raw codex frames in the transcript — purpose-built UIs for each new approval type are a follow-up. New notifications flow through `notif.into_envelope()` and hit the existing string-dispatch's `_ => debug!` arm without code changes.

## 2.5.31

- **Probe installed agent CLIs when the launch dialog opens.** Previously the only signal that a launcher was missing `codex` or `claude` was a session that spawned then vanished within a second with a misleading `exited normally (code 0)` log. The launch dialog now asks the selected launcher to scan its PATH the moment the user opens it (and again on every launcher dropdown change), and surfaces the result inline:
  - Agent dropdown labels show `Claude (not installed)` / `Codex (not installed)` when the binary isn't on the host's PATH.
  - A red inline warning sits under the agent picker when the selected agent is missing.
  - A grey "Checking installed agents..." note shows while the probe is in flight.
- Plumbing:
  - Extracted the `which` + `--version` probe into `claude_session_lib::probe` so it's reusable outside the spawn-time diagnostic.
  - New `shared::AgentInstall { agent_type, installed, resolved_path, version }`.
  - New WS request/response pair: `ServerToLauncher::ProbeAgents { request_id }` → `LauncherToServer::ProbeAgentsResult { request_id, agents }`.
  - Backend correlates responses via a parallel `pending_probe_requests` map (mirrors the existing directory-listing pattern).
  - New REST endpoint `GET /api/launchers/{id}/probe-agents` that triggers the round-trip and returns the install state with a 5 s timeout.

## 2.5.30

- **Attach the raw Codex frame to typed-decode-failure portal messages.** When a codex frame fails our bundled `codex-codes` schema (the canonical `missing field 'callId'` mismatch with codex CLI 0.130.0's approval requests, see #703), the proxy interrupted the turn but left the user without the offending frame's content — making upstream bug reports hard to file. A new `CodexFrameCaptureLayer` tracing layer watches codex-codes' `[CLIENT] Received: <raw>` DEBUG events at runtime and stashes the last 8 frames in a process-wide ring buffer. On `Error::Json` the codex I/O task drains the most-recent entry (overwhelmingly the offending frame, since capture happens microseconds before the typed-decode failure on the same thread) and emits it as a portal message with a fenced JSON block, ready to copy-paste. The layer declares per-callsite `Interest::always()` only for `codex_codes::client_async` DEBUG events, so it sees them even when `RUST_LOG=info,…` would otherwise suppress them — without flooding journald (the fmt-layer EnvFilter is now per-layer, not global). Installed in both `launcher/src/main.rs` and `proxy/src/main.rs`.

## 2.5.29

- **Auto-retry upstream-429 turns with full-jitter exponential backoff.** When Anthropic's API rate-limits a request, `claude --print` doesn't fail — it streams an assistant message starting with `API Error: Server is temporarily limiting requests (not your usage limit) · Rate limited` and emits a `Result` with `is_error: true`. From the user's POV the session just ate a turn and went quiet. `claude_io_task` now caches the last user input, watches each turn for that exact shape, and on a match auto-retries with `delay ∈ [0, min(30, 2^attempt)]` seconds (full-jitter prevents N concurrent stuck sessions from re-firing in lockstep when the limit window resets). Up to 4 retries per user input; each retry emits a portal message announcing the wait and attempt count; on max-out we surface a final portal note asking the user to resend. If a new user input arrives during the backoff, it cancels the retry and runs normally — the user can always override automation. Counter resets on every fresh user input and on every successful turn.

## 2.5.28

- **Stop rendering protocol-agnostic messages as "Codex Raw" blocks on Codex sessions.** The synthetic user-echo (PR #691, fixing the "Codex Raw" pile-up) and the backend's `Portal` envelope both have `type: "user"` / `type: "portal"` and parse cleanly as `ClaudeMessage` — but the old dispatch sent every message on a Codex session straight to the Codex renderer, which only knows codex-specific shapes (`item.*`, `turn.*`) and falls anything else through to the "raw JSON" catch-all. The user saw their own input echoed as a JSON dump. Dispatch now matches on the message shape first (any recognized `ClaudeMessage` variant renders the same on both agents) and only routes the remaining unknown-shape messages to the Codex renderer.

## 2.5.27

- **Agent-type watermark behind session pills.** Each pill now shows the actual brand mark of the agent backing it — Anthropic's stylized burst behind Claude sessions, OpenAI's hex-knot behind Codex sessions — anchored to the left edge of the pill, clipped to the rounded silhouette via the existing `overflow: hidden`, sized to overflow the corner a bit. Light grey at 0.22 opacity, sits below the foreground text and indicators so legibility is unchanged. The old "Codex" text badge is removed since the watermark carries the same signal.
- New static assets: `frontend/assets/anthropic-mark.svg` (recolored to white from the source SVG so it tints down to the Tokyo Night palette cleanly) and `frontend/assets/openai-mark.png` (alpha-cut from the source PNG, RGB-negated so the logo reads light on the dark pill).

## 2.5.26

- **Fix #703 — codex sessions no longer wedge after a typed-decode failure.** The 2.5.22 patch for #695 kept the session alive after `codex-codes` failed to deserialize a frame, but silently dropped the frame. When the lost frame was a server→client approval request (the `callId`-missing variant in codex 0.130.0's `item/{commandExecution,fileChange}/requestApproval`), codex blocked the turn waiting for a reply it would never get; the proxy stayed in `turn_active = true` and rejected every subsequent user prompt with `Received input while Codex turn is active`. From the user's POV the session looked alive but ignored them. Now, on `codex_codes::Error::Json` during an active turn, `claude-session-lib` (a) emits a `turn.failed` event to the frontend so the hang is visible, and (b) sends `turn/interrupt` so codex unblocks and emits `turn/completed` (Interrupted), which clears the flag through the normal path. If `turn_interrupt` itself errors, the flag is force-cleared to avoid a permanent wedge.

## 2.5.25

- Awaiting pill border + indicator back to red (`--error`) instead of orange. The pulse animation stays gone (#684's complaint was the strobing, not the color).

## 2.5.24

- **Session pill colors: blue focus ring, no more pulsating red on awaiting pills.** The 2.5.11 "bright-red active pill indicator" was too loud; the awaiting-pulse animation made it worse for any tab the agent was still chewing on. Now:
  - **Focused pill**: accent-blue border + soft blue background tint + 2 px blue ring.
  - **Awaiting pill** (agent still working): subtle orange border, no animation. The existing in-pill indicator glyph still picks up an orange tint to flag the state without the page strobing.
  - **Focused + awaiting**: focus state dominates — keeps the blue ring/background; the indicator glyph alone signals "still working."
- Dropped the `@keyframes awaitingPulse` rule.

## 2.5.23

- **Fix #692 — portal reminder no longer bloats the user transcript.** Two parts:
  - Reflowed `claude-session-lib/portal_reminder.md` to single-line paragraphs and bullets. The previous 72-column hand-wrapping turned every continuation into a new visual line in the rendered preview; long-line markdown lets the renderer reflow to the available width.
  - Dropped the user-bound `PortalMessage::reminder` emission entirely. The collapsed "Portal features" block on the frontend was content the user already knew (they built the portal), and the actually-useful side — re-priming the agent's affordance knowledge after a fresh start / compaction — is the stdin injection, which stays. The `PortalContent::Reminder` enum variant and its frontend renderer are kept around so historical DB rows still deserialize cleanly.
- **Filter Claude's user-message echo of `<system-reminder>` wrappers.** When the proxy injects the reminder via stdin, Claude echoes it back on stdout as a `ClaudeOutput::User` whose content is the raw `<system-reminder>…</system-reminder>` text. Forwarding that to the frontend leaked the wrapper text into the transcript as if the user had typed it. The output forwarder now detects user-message echoes whose first text block starts with `<system-reminder>` and drops them before forwarding.

## 2.5.22

- **Fix #695 — codex sessions no longer die on typed-decode errors.** Codex CLI 0.130.0 emits at least one frame type (likely a notification with a `callId` field our bundled `codex-codes` 0.128.0 doesn't model) that fails strict deserialization. Previously the proxy turned that into `SessionError::CommunicationError` and killed the session; from the user's POV the agent just vanished mid-turn. Now `codex_io_task` matches on `codex_codes::Error::Json` specifically, logs a warning, and continues the loop so the rest of the session stays alive. Other error variants (I/O, protocol, server-closed) still terminate as before.

## 2.5.21

- **Fix #678 — macOS ARM64 CI no longer flakes on broken-shim runner images.** The 2.5.8 PATH workaround was based on a wrong theory: on affected `macos-14` images, the files at `~/.cargo/bin/{cargo,rustc}` are *both* actually `rustup-init`, not toolchain shims. `dtolnay/rust-toolchain@1.92.0` exits 0 on these images but leaves the install half-broken — `rustup run` couldn't help either, because cargo internally invokes `rustc -vV` and got `rustup-init 1.29.0` back. Replaced the dtolnay step on all four macOS jobs (`build-proxy-macos-arm64`, `build-launcher-macos-arm64`, `build-macos-arm64`, `build-macos-intel`) with an explicit `rm -rf ~/.cargo ~/.rustup` followed by a fresh `curl | sh -s -- -y --default-toolchain 1.92.0 --profile minimal` install. Plain `cargo build` works after that. Dropped the obsolete PATH workaround.

## 2.5.20

- **Add a triple-click "Update & Restart" button to the Launchers panel.** Lets an operator push a launcher to fetch the latest agent-portal release from GitHub and restart itself, straight from the dashboard. Three-stage confirmation prevents fat-finger accidents:
  - **Click 1**: gray "Update & Restart" → yellow "Wait, really?"
  - **Click 2**: yellow → red "Are you absolutely positively sure?"
  - **Click 3**: red → muted "Restarting…" (POSTs `/api/launchers/:id/update`)
- Backend translates the POST into a new `ServerToLauncher::UpdateAndRestart` WebSocket message; the launcher receives it, calls `portal_update::check_for_update` (the same path as the `agent-portal update` CLI subcommand), then `service::restart()` via systemctl/launchctl. If the launcher isn't running under a service manager it just exits and relies on an external supervisor to respawn.

## 2.5.19

- **Fix "Codex Raw" pending messages piling up at the bottom of the transcript.** Codex's app-server protocol doesn't echo user input back the way Claude's CLI does, so the frontend's optimistic-send pending entry never matched anything and accumulated on every send. The pending entries were rendered through the Codex renderer (which doesn't recognize the `{type: "user", _pending: true}` shape) and fell into the "Codex Raw" catch-all. Fix: `codex_io_task` now emits a synthetic user echo (parses as `ClaudeOutput::User` with a top-level `content` field) right before kicking off the Codex turn, so the frontend's existing content-match pending-clear path fires identically for both agents. Portal-reminder injections wrapped in `<system-reminder>` tags are filtered out of the echo path so they stay invisible to the user.

## 2.5.18

- **Bump `codex-codes` 0.101.1 → 0.128.0.** Upstream restructured `ServerMessage` from the loose `{ method, params }` envelope into typed enums (`Notification(Notification)` and `Request { id, request: ServerRequest }`). Adapted the proxy's Codex dispatcher in `claude-session-lib/src/session.rs` to:
  - Match `ServerMessage::Notification(notif)` and recover `(method, params)` via `notif.into_envelope()` — keeps the existing string-based downstream dispatch intact.
  - Match `ServerMessage::Request { id, request }`, pull the method via `request.method()`, and serialize the typed param struct (`CmdExecApproval`, `FileChangeApproval`, or `Unknown`) back to a `Value` so the rest of the approval-flow code is unchanged.
- Three new `Notification` variants are now modeled (`AccountRateLimitsUpdated`, `McpServerStartupStatusUpdated`, `RemoteControlStatusChanged`); they fall through to our existing `_ => Skip` arm via `notif.into_envelope()`, so they're handled implicitly without further code.

## 2.5.17

- **Inject a "portal features reminder" into the agent at session start and after each context compaction.** Reminds the agent which portal-specific features are available (auto-formatted links, KaTeX, image rendering, AskUserQuestion, session sharing, etc.) so it can actually use them after a fresh start or a compaction-induced amnesia. The reminder is wrapped in `<system-reminder>` tags on the agent side and emitted as a collapsed `PortalContent::Reminder` block on the frontend so it doesn't clutter the transcript.
- The reminder body lives in `claude-session-lib/portal_reminder.md` as a readable markdown file (baked into the binary via `include_str!`), and operators can override it at runtime by pointing `PORTAL_REMINDER_FILE` at a readable path. Missing/unreadable overrides log a warning and fall back to the bundled default. Documented in `Dockerfile`, `docs/DEPLOYING.md`, `docs/DOCKER.md`, and `CLAUDE.md`.
- Extracted the duplicated "is this a compaction-end marker?" predicate into `shared::is_compaction_boundary(&CCSystemMessage)`. The Claude CLI emits this boundary under several subtype spellings (`compact_boundary`, `compaction`, `context_compaction`, `summary`) and three call sites were inlining the disjunction — they now all go through the shared helper.

## 2.5.16

- **Fix #684 — KaTeX hasn't actually been rendering math on any page load.** The Subresource Integrity hash on the `<script>` tag for KaTeX's `auto-render.min.js` in `index.html` did not match the file the CDN actually serves. Browsers silently refuse to execute scripts whose SRI hash mismatches, so `window.renderMathInElement` never landed on the page, and every call to our `renderMathInNode` helper silently no-op'd. Verified by a standalone harness (`frontend/dev-tools/katex-isolated.html`) that loads the same scripts from the same CDN and reports "renderMathInElement not on window". Replaced the stale hash with the real `sha384-43gviWU0YVjaDtb/GhzOouOXtZMP/7XUzwPTstBeZFe/+rCMvRwr4yROQP43s0Xk`.
- Also hardened `katex-helper.js` with a queue-and-retry path for the unlikely cold-load case where the helper fires before the CDN scripts finish loading, and added diagnostic logging so future failures surface in the browser console instead of being silently swallowed.
- Added a regression test confirming math in messages mixing `<thinking>` HTML blocks + fenced ```latex``` code + display `$$…$$` survives the markdown placeholder round-trip into `Event::Text`. This disproved earlier hypotheses that the bug was in our markdown pipeline.

## 2.5.15

- Pills are now 180 px wide (was 150 px, +20%) and the vertical rail tray shrinks from 240 → 200 px to match — no more dead space on the right of the column.

## 2.5.14

- Surface speech-to-text errors instead of silently ending the recording. Previously, when the backend speech recognition task failed early (bad credentials, recognizer setup error, Google API rejecting the stream, …), the result channel just closed and the WebSocket handler blindly emitted `VoiceEnded`, so the user saw the mic button "return immediately" with no explanation. Now the recognition task reports its final status via a oneshot channel, and the handler emits `VoiceError` with the actual error string when something went wrong.

## 2.5.13

- Session pills now have an explicit **150 px** width in both horizontal and vertical rail modes — previous content-driven sizing kept getting pushed by long folder names hitting min-content widths inside the flex children. `.pill-name` is `flex: 1; min-width: 0` so it fills the remaining space and ellipsizes cleanly. Vertical-mode `width: 100%` override removed.
- Trim the gap between the last visible message and the input bar: dropped the 0.5 rem bottom padding I added in 2.5.11 and shrunk the input form's top padding from 0.5 → 0.3 rem.

## 2.5.12

- Actually shrink the session pill width — #680's `.pill-name` change only knocked ~14% off the outer pill since the surrounding chrome stayed the same size. Pill-name width 120 → 100 px, pill horizontal padding 0.75 → 0.55 rem, and inter-item gap 0.5 → 0.35 rem, for ~25% total outer-width reduction in horizontal mode.

## 2.5.11

- Trim message stream right/bottom padding to match the previously-halved left side (1.5 rem → 0.75 rem right; bottom 0 → 0.5 rem)
- Tighten the `>` input prompt on both sides (negative left margin to match the existing negative right margin)
- Bright-red active pill indicator (was accent blue) so the focused session is immediately obvious
- Pill text width shrunk by 20 % (150 → 120 px) and the `▼` menu toggle now sits above the text in stacking order (z-index 2) so it stays readable when folder/branch labels are long

## 2.5.10

- Fix #676 — LaTeX math no longer drops when a message mixes equations with markdown that contains `_` (e.g. `$\sigma_{1D}$`). Math regions (`$…$`, `$$…$$`, `\(…\)`, `\[…\]`) are now extracted before markdown parsing so pulldown-cmark can't split equation text on emphasis-meta characters; the math is restored verbatim in `Event::Text` so KaTeX auto-render can find the delimiters in a single DOM text node. Skips inline-code, fenced-code, and `$5`-style dollar amounts.
- Fix #675 — Copy button icon swapped to the canonical Lucide two-overlapping-rectangles glyph so the copy affordance is immediately readable.
- Tighten horizontal spacing between the `>` prompt and the input textarea (negative margin-right of 0.35 rem on `.input-prompt`).

## 2.5.9

- Expand the rail position setting from horizontal/vertical to four choices: **Top**, **Bottom**, **Left**, **Right**. Top is unchanged (default); Bottom places the rail under the input bar; Left and Right place the rail as a 240 px column on either side. Existing `horizontal`/`vertical` localStorage values are auto-migrated to `top`/`left`.
- Halve the left padding on the message stream (1.5 rem → 0.75 rem) so message bodies sit closer to the left edge.

## 2.5.8

- CI: prepend `$HOME/.cargo/bin` to PATH on macOS jobs in `ci.yml` and `release.yml` to work around a `macos-14` runner image variant where a preinstalled `rustup-init` shadowed the rustup-managed `cargo` shim, causing intermittent `error: unexpected argument 'build' found` failures

## 2.5.7

- Bump `claude-codes` 2.1.117 → 2.1.140 — fixes a latent proxy data-loss bug where the structured `tool_use_result` field (e.g. AskUserQuestion's `{questions, answers}`, Bash's `{stdout, stderr, exit_code}`) was being dropped by the typed serde round-trip in the proxy on the way to the frontend
- Also picks up `UserMessage.timestamp` (CLI-emitted ISO-8601 timestamp echoed alongside tool results)

## 2.5.6

- Force pills to actually stack vertically in vertical-rail mode (the previous fix laid out the wrapper as a left column but pills inside the rail still flowed horizontally on some browsers). Adds `!important` on `flex-direction: column` and `overflow-x/y` for the vertical rail, plus `align-items: stretch` so pills take full column width.

## 2.5.4

- Fix vertical pill rail orientation: previous CSS selector targeted the wrong element (`.session-rail` is wrapped in `.session-rail-container`, so a direct-child combinator never matched); now styles the wrapper for the left-column layout

## 2.5.3

- Stabilize expand/collapse state of message components across new messages: bash command toggle, `ExpandableText` "... more chars", and image viewer no longer reset when later messages arrive
- Keys for message groups and grouped messages now derive from `_created_at` instead of position, so inserting a user message between assistant groups stops invalidating later groups' identity

## 2.5.2

- Pause auto-scroll when the user scrolls up to read older output; new messages no longer yank the view to the bottom
- Show a floating "Jump to live ↓" pill in the messages area when tailing is paused; click to resume

## 2.5.1

- Add Appearance tab in Settings with a horizontal-vs-vertical toggle for the session pill rail
- Preference persists in browser localStorage (`claude-portal-rail-orientation`)
- Vertical layout puts the pill rail down the left side; horizontal keeps the current top-bar layout

## 2.5.0

- Chunked image uploads over a new `/ws/session/upload` stream (#666)
- Large images (>= 1 MB on disk) now stream from proxy to backend as raw binary chunks, bypassing the WS frame size cap that previously stuck sessions in reconnect loops
- Avoids base64 encoding on the wire for large images — chunks are sent as native binary WebSocket frames
- Backend reassembles chunks in an in-memory upload buffer, enforces `PORTAL_MAX_IMAGE_MB`, then publishes the image at `/api/images/{uuid}` like before
- Proxy now rejects images over the cap locally (textual portal message) instead of queueing oversized payloads that fail forever on replay

## 2.4.37

- Move "time ago" label from message header to small unobtrusive footer at bottom-right of assistant messages

## 2.4.36

- Left-justify copy button in user/portal message headers (was pushed to far right)

## 2.4.35

- Show live-updating "X minutes ago" label on assistant message headers, with exact local time tooltip

## 2.4.34

- Place copy button next to model name in assistant headers (was at far right)

## 2.4.33

- Add clipboard copy button to message headers (assistant, user, portal) to copy raw markdown text

## 2.4.32

- Fix splash page not scrollable on mobile (overflow:hidden was on body instead of session container)

## 2.4.31

- Add stable keys to grouped assistant messages to preserve expanded state on new messages

## 2.4.30

- Disable Google STT single-utterance mode to prevent immediate session end on quiet/silent mic inputs (common on macOS)

## 2.4.29

- Clear pending send queue when assistant/result arrives (fixes stuck pending state for slash commands)
- Match pending sends by content on user echo so lost messages don't consume unrelated entries

## 2.4.28

- Up/down arrows in input now move cursor between lines; history navigation only triggers at the first/last line

## 2.4.27

- Render LaTeX math in messages via KaTeX (`$inline$`, `$$display$$`, `\(...\)`, `\[...\]`)

## 2.4.26

- Serve large images via HTTP instead of WebSocket (partially addresses #655)
- Backend extracts base64 images >64KB from portal messages, stores in memory, replaces with `/api/images/{uuid}` URLs
- Frontend renders both URL and base64 image sources
- Eliminates WS frame size issues for large images

## 2.4.25

- Add 5-second query timeout to retention cleanup tasks (#616)
- Cap retention deletes at 1000 messages per cycle to avoid long-running queries

## 2.4.24

- Limit launcher registrations to 10 per user (#617)

## 2.4.23

- Reduce spend broadcast interval from 5s to 30s and skip when no clients connected (#618, #614)
- Use single aggregate query instead of per-user queries for spend updates

## 2.4.22

- Fix stuck reconnect loop: drop pending messages after 5 consecutive replay failures (#650)

## 2.4.21

- Click truncated bash commands to expand full command text

## 2.4.20

- Fix false autolinks: angle-bracket Rust paths like `<crate::Type>` no longer render as clickable URLs

## 2.4.19

- Fix iOS keyboard gap: use position:fixed on mobile to track visual viewport

## 2.4.18

- Fix iOS Safari scroll bounce creating stuck dead space below messages

## 2.4.17

- Optimistic send: user messages appear instantly with a pending indicator, confirmed when server echoes back

## 2.4.16

- Linkify URLs in inline code spans, error messages, and file content previews

## 2.4.15

- Linkify URLs in code blocks, tool results, expandable text, and thinking blocks

## 2.4.14

- Show session init info bar with model, version, fast mode, MCP servers, and tool count

## 2.4.13

- Show truncation warning when assistant message hits max_tokens
- Add service tier and inference region to model name tooltip
- Add ephemeral cache details to usage tooltip

## 2.4.12

- Show cost, stop reason, fast mode, errors, and permission denials in result stats bar
- Add model usage breakdown to result timing tooltip

## 2.4.11

- Bump claude-codes to 2.1.117
- Render new content block types: ServerToolUse, WebSearchToolResult, McpToolUse, McpToolResult, CodeExecutionToolResult, ContainerUpload
- Render web search citations on text blocks
- Show unknown content blocks as collapsible JSON instead of silently dropping them

## 2.4.10

- Add token renewal button in settings credentials panel
- Add local-time tooltip on message headers

## 2.4.9

- Fix pill scroll-into-view when tabbing to off-screen sessions

## 2.4.8

- Purge expired device flow codes every 60 seconds (#615)

## 2.4.7

- Global thin scrollbar styling (6px, subtle, matches dark theme)

## 2.4.6

- Add sortable columns to admin users table (Email, Name, Status, Sessions, Spend, Created)

## 2.4.5

- Fix admin stats 500: cast SUM(bigint) to ::bigint in raw SQL queries (#627)
- Add raw SQL type safety guidance to CLAUDE.md

## 2.4.4

- Unify config directory: launcher now uses `~/.config/agent-portal/` (same as proxy)
- Migrate launcher config from TOML to JSON (`launcher.toml` -> `launcher.json`)
- Auto-migrate old `~/.config/claude-portal/launcher.toml` on startup
- Both proxy and launcher use `directories::ProjectDirs` for consistent paths across platforms
- Update install script to use new config path and JSON format

## 2.4.3

- Add GitHub link to minimal splash page footer

## 2.4.2

- Add `SPLASH_TEXT` env var for minimal login page (heading + sign-in button + version + bug link)
- When unset, the full marketing splash page is shown as before

## 2.4.1

- Replace rust-embed + startup brotli/gzip compression with memory-serve (build-time compression, zero startup cost)
- Remove rust-embed, brotli, flate2, mime_guess dependencies
- Closes #613

## 2.4.0

- Upgrade axum 0.7 to 0.8, ws-bridge 0.1 to 0.2, tokio-tungstenite 0.24 to 0.28
- Upgrade tower-cookies 0.10 to 0.11, tower_governor 0.4 to 0.8
- Migrate route paths from `:param` to `{param}` syntax

## 2.3.15

- Add `agent-portal service logs` command with `-n` (line count) and `-f` (follow) options

## 2.3.14

- Resume existing Claude sessions on launcher restart instead of creating new ones

## 2.3.13

- Fix admin stats page crashing on non-401/403 error responses

## 2.3.12

- Render each message in assistant groups as its own component so only new messages re-render
- Revert thread_local expanded state hack (no longer needed)

## 2.3.11

- Fix expanded "... more chars" content collapsing when new messages arrive

## 2.3.10

- Upload progress bar fills over 1.5s minimum and collapses with animation after completion

## 2.3.9

- Defer stale session cleanup on backend startup to give proxies time to reconnect, fixing sessions disappearing from the pills menu after a backend restart

## 2.3.8

- Add `agent-portal service start`, `stop`, and `restart` subcommands

## 2.3.7

- Fix awaiting-input detection to skip noise message types (portal, error, system, rate_limit_event)
- Extract shared `is_claude_awaiting` function used by both REST load and WebSocket paths

## 2.3.6

- Auto-delete completed cron sessions to prevent UI clutter (costs preserved)
- Hide cron sessions by default in the session rail

## 2.3.5

- Fix admin stats endpoint returning empty body due to SQL type mismatch (COALESCE returns numeric, Diesel expects float8)

## 2.3.4

- Fix send bar not clearing after sending with attachments

## 2.3.3

- Consolidate remaining duplicate user ID extraction into shared auth module
- Convert scheduled_tasks and proxy_tokens handlers to use AppError

## 2.3.2

- Add unified AppError type for backend handlers with structured error responses
- Consolidate duplicate auth extraction into shared `auth::extract_user_id`
- Convert sessions, messages, launchers, and sound_settings handlers to use AppError

## 2.3.1

- Remove stale cli-tools crate and dead API types
- Remove unused dependencies from backend, proxy, launcher, and shared

## 2.3.0

- Auto-renew launcher auth tokens over WebSocket when within 7 days of expiry
- Add token expiry warning icon on session pills for sessions from launchers with expiring tokens
- Add Launchers tab to Settings page with manual token renewal button
- Add POST /api/launchers/:id/renew-token endpoint for manual token renewal

## 2.2.3

- Fix Codex sessions failing to start from launcher: resolve binary path via `which` before spawning

## 2.2.2

- Add favicon and browser tab icon for link previews

## 2.0.4

- Fix multiline user input getting flattened when rendered in message history

## 2.0.3

- Bake user's PATH into systemd/launchd service so spawned agents can find `claude`

## 2.0.2

- Fix install script: `agent-portal install` → `agent-portal service install`

## 2.0.1

- Version bump to 2.0.1

## 1.3.49

- Add Linux aarch64 support (CI builds, auto-update, install script)

## 1.3.48

- Fix session reconnect race: old connection cleanup no longer overwrites newer connection's registration

## 1.3.47

- Fix oneshot drop race causing launcher sessions to not reconnect on server restart

## 1.3.46

- Show proxy version badge in session pill, color-coded by staleness

## 1.3.44

- Break up settings.rs into sub-components (TokensPanel, SessionsPanel, SoundsPanel)

## 1.3.43

- Update claude-codes to 2.1.51 (typed enums for message fields)
- Handle unparsable CLI messages gracefully instead of crashing sessions

## 1.3.42

- Detect subagent task completion via tool_result fallback when task_notification is missing (--print mode)

## 1.3.41

- Add repo URL to pill menu with 3-state display: PR link, repo link, or "No Repository Detected"
- Proxy detects GitHub repo URL via `gh repo view` and sends it alongside branch/PR info

## 1.3.40

- Break up admin.rs into sub-components per tab (overview, users, sessions, raw messages)
- Review and update all docs for accuracy across 15 files
- Fix subagent completion handling in history loading path to preserve task data
- Replace catch-all status mapping with explicit CCTaskStatus variant matching

## 1.3.38

- Add "Add machine" button to launch dialog for setting up new launchers
- Remove Service section from settings Credentials tab

## 1.3.37

- Move install under service subcommand (`agent-portal service install`)

## 1.3.36

- Add agent install setup under Service section in Credentials settings tab

## 1.3.35

- Add bash-style Tab completion to launch dialog path input

## 1.3.34

- Fix launch dialog bugs and refactor DirBrowser

## 1.3.33

- Prevent duplicate launchers per host-user and fix tilde expansion

## 1.3.32

- Launcher cleanup: fix task abort, URL dedup, send error logging, config path

## 1.3.31

- Show session details (name, host, directory, branch, agent) in portal message on connect/reconnect

## 1.3.30

- Unify admin and settings page layout styles

## 1.3.29

- Fix transparent admin/settings overlay background

## 1.3.28

- Move bug report to bottom-right link with bug emoji

## 1.3.26

- Fix Shift+Tab keyboard hint text

## 1.3.25

- Keep server shutdown banner until first message received from reconnected server

## 1.3.24

- Remove unused dead code methods from CommandHistory

## 1.3.23

- Fix result message duplicating previous assistant message text

## 1.3.22

- Fix launcher crash: install ring crypto provider for rustls 0.23

## 1.3.21

- Fix sparkline tick subpixel rendering artifacts via GPU compositing

## 1.3.20

- Fix tasks drawer pull-tab clipped by overflow:hidden on drawer container

## 1.3.19

- Add `agent-portal login` subcommand for explicit authentication
- Add `agent-portal install` subcommand to install as system service
- Add `agent-portal update` subcommand to update binary and restart service
- Install script no longer auto-installs system service
- Updated frontend setup instructions with 3-step flow (install, login, service)

## 1.3.18

- Differentiate task and portal message colors (tasks=purple, portal=teal)
- Sparkline tick colors now match their message type colors

## 1.3.17

- Rename agent-launcher to agent-portal
- Make launcher the default install path (install script downloads agent-portal, installs as service)
- Launch button available to all users (not just admin)
- Launch dialog shows install instructions when no launchers are connected
- agent-portal recommends service install when run interactively

## 1.3.16

- Backend sends max image size to proxies via RegisterAck; proxy uses it instead of local env var
- Remove frontend image size check (backend/proxy is authoritative)

## 1.3.15

- Fix stale subagent entries persisting across page reloads by clearing task state on history reload
- Tasks sidebar panel now slides in/out with the drawer instead of instantly appearing/disappearing

## 1.3.14

- Refactor sender attribution: store actual sender user_id in DB, reconstruct display info at query time

## 1.3.13

- Add user name attribution to messages in shared sessions

## 1.3.12

- Increase default image max size from 2 MB to 10 MB

## 1.3.11

- Update claude-codes to 2.1.49 (String-to-enum migration for subtype, stop_reason, status)
- Re-export `CCSystemSubtype` from shared

## 1.3.10

- Use typed claude-codes structs for task parsing instead of raw JSON field access
- Parse task_type, task_status, and task_usage via typed deserialization in both component logic and renderers

## 1.3.9

- Tasks sidebar: header bar toggles open/close (removed separate X button)
- Tasks sidebar: show running task count in title bar

## 1.3.8

- Add widget protocol specification (`docs/WIDGET_PROTOCOL.md`)

## 1.3.7 and earlier

- See git history for previous changes.
