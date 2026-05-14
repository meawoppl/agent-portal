# Changelog

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
