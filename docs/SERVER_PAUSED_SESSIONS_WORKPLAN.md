# Server-Side Paused Sessions Workplan

> **STATUS: COMPLETED** — implemented: `sessions.paused` and `sessions.claude_args` columns exist in `backend/src/schema.rs`, and `POST /api/sessions/{id}/pause` / `POST /api/sessions/{id}/resume` routes are registered in `backend/src/main.rs`. Kept as a historical design record.

## Goal

Prevent launcher/backend restarts from automatically resuming Claude sessions that the user has paused in the dashboard. Paused sessions must remain resumable on demand, but should not spawn `claude --resume <session_id>` during reconnect/startup because that reloads the full conversation and burns many tokens.

## Current State

- Launcher persists restartable sessions locally in `launcher.json` as `ExpectedSession`.
- On reconnect/startup, launcher asks the backend to relaunch every expected session.
- If an expected session has a `session_id`, the proxy starts Claude in resume mode.
- Dashboard "hidden" state is browser-local only, stored in `claude-portal-hidden-sessions`.
- Backend and launcher do not know a session is hidden/paused.
- Existing "Stop Session" terminates the process and removes the launcher expected-session entry, so it is not a resumable pause.

## Target Design

- Add a durable `sessions.paused` boolean owned by the backend.
- Existing sessions are backfilled as paused by default so the first rollout
  cannot accidentally auto-resume every historical session before the browser
  has a chance to migrate its local state.
- Add `sessions.claude_args` so the backend can relaunch a paused session without depending only on launcher-local metadata.
- Add pause/resume APIs:
  - `POST /api/sessions/:id/pause`
  - `POST /api/sessions/:id/resume`
- Pause:
  - verifies mutator access;
  - sets `paused = true`;
  - stops the running process without removing the launcher's expected-session record.
- Resume:
  - verifies mutator access;
  - sets `paused = false`;
  - requests the owning launcher to launch the session in resume mode.
- Launcher startup/reconnect:
  - before relaunching an expected session, asks backend with `RequestLaunch { last_session_id }`;
  - backend refuses paused sessions by returning a launch failure without sending `LaunchSession`.
- Frontend:
  - surfaces Pause/Resume in the session menu;
  - includes paused sessions in the rail;
  - migrates old browser state by resuming only sessions that were not in the
    local hidden set, leaving hidden sessions paused.

## Implementation Steps

1. Add migration and model/shared fields for `paused` and `claude_args`.
2. Extend launcher protocol with non-destructive pause and explicit resume launch metadata.
3. Add backend endpoints and launcher gating.
4. Update frontend session rail/dashboard behavior.
5. Run `cargo fmt`, targeted backend/frontend checks, and protocol tests where practical.

## Risk Notes

- `launcher_id` is currently generated on launcher startup, so resuming after a launcher restart must target a currently connected launcher by hostname/user if the stored `launcher_id` is stale.
- Existing launcher-local `ExpectedSession` remains the compatibility path; backend-owned resume metadata is additive.
- Hidden and paused are related but not identical. The migration treats current hidden sessions as paused once to address the immediate token burn problem.
