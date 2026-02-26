# Changelog

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
