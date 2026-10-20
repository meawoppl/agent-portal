# Restore a session from history

Open an archived session in **History** and choose **Restore session**. Only
the session's owner sees the button, and only Claude and Codex sessions can be
restored.

A restore is always **full**: the restored session continues the original
agent conversation, with its whole context. Deleting a Portal session never
removes the agent's own conversation from the computer it ran on (Claude's
transcript under `~/.claude/projects`, Codex's saved thread), so that
conversation is usually still there. If it is gone, for example because the
agent pruned it or the computer changed, the restore is refused rather than
starting a fresh agent.

The restored session starts **paused** in the dashboard with the archived
transcript copied in. Press **Resume** to continue. If the original session is
still open, Restore just opens it. Restoring the same record again opens the
restored session instead of making another one.

## How it works

`POST /api/history/sessions/{user}/{session}/restore` (owner-only):

1. Picks the launcher the session last ran on, or another of the owner's
   connected launchers on the same host. It must advertise
   `session.restore_v1`.
2. Sends `PrepareSessionRestore`. The launcher checks that the native
   conversation exists and binds it to the new session id: Claude through
   `claude_conversations.json` (so `--resume` follows it), Codex by moving the
   thread from the archived id to the new one in `codex_threads.json`. It also
   records the pair in `restored_sessions.json`.
3. Creates the paused session and copies the archived transcript in one
   transaction. Only scrollback is limited (8 MiB / 50,000 messages, newest
   kept); the agent's own context is untouched.

The new session id is derived from the owner and the archived session id, so
retries and concurrent requests produce one session.

On every resume of a restored session the launcher re-checks the native
conversation and fails the launch if it has vanished, instead of the usual
fallback to a fresh conversation. Codex additionally sets
`SessionConfig::codex_require_resume`.
