# Restore from History

Open **History**, select a conversation, then choose **Restore from History**.
If that session (or its recovery) is already in Portal, **Open existing session**
takes you there. A paused session can be resumed from its normal controls.

For an archive whose live row has been deleted, choose a connected host and an
existing working directory under that host's home folder:

- **Continue saved native conversation** checks the original host for saved
  Claude or Codex state. Resume branches that conversation into the restored
  session, preserving the original archive and native conversation. Claude
  requires its original directory; Codex can branch into the selected directory.
  Missing or unreadable native state is an error, with no empty-session fallback.
- **Recover from archived context** starts a new conversation using a recent
  excerpt of archived user/assistant records (at most 64 KiB). It supports
  Claude, Codex and Muse on a connected host. This is not an exact native resume:
  project files, attachments, running tools and unsaved native state are not
  recovered. An archive without conversation records requires native state.

Restoration creates a **paused** session. Open it, choose **Resume**, then send a
new instruction. Historical commands and requests are never put in the input
queue. Context recovery attaches the excerpt to the first real user message
following each process start, with a notice identifying it as historical
reference material. Slash commands do not consume it; subsequent inputs in the
same process do not repeat it. The excerpt explicitly defers to later messages.

The restored transcript copies up to 8 MiB / 50,000 recent records for display;
a Portal notice reports how many were copied and links to the complete original
archive. Recovered sessions follow normal sharing and retention rules. Original
membership, usage totals, queued work, cron settings and permission-bypass flags
are not copied. Only the archive owner can restore; archive viewer/editor and
admin visibility alone does not grant execution rights on the owner's hosts.

Reconnect an offline original host for native continuation, or explicitly select
archived context and another connected host. Directory and native-state failures
appear before creating the session. The selected launcher must advertise
`session.history_restore_v1`; both the UI and backend ask for an update otherwise.

## Persistence and retries

`GET /api/history/sessions/{user}/{session}/restore` returns ownership eligibility,
the existing live/recovered session ID, and transcript availability. `POST` takes
`launcher_id`, `working_directory`, and `mode` (`native` or `context`) and returns
`session_id`. The server performs an owner-gated, read-only launcher preflight.

The recovery ID is deterministic over archive owner and source session, so
concurrent requests or retries cannot create multiple sessions. The paused row,
owner membership, display history, and `sessions.history_restore` launch recipe
commit together. Repeated requests return the same session even if a caller
changes its host or recovery choice; they do not reset a conversation that has
already been recovered. The original archive is never written by restoration.

Resume and launcher reconciliation carry the durable recipe to the launcher.
Native recovery uses the existing fork implementation and saved native-ID maps;
after that first branch, relaunch resumes the restored session's own native state.
No private Codex database or rollout format is rewritten. Back up the launcher
configuration and native agent storage if exact native continuity is required.

Tests: `cargo test -p backend --test history_api`, `cargo test -p agent-portal
history_restore`, and the session-lib tests. With the frontend built and Python
Playwright installed, `python3 scripts/test-history-restore-ui.py` tests the UI
against fake APIs in Chromium at desktop/mobile widths.
