# Changelog

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
