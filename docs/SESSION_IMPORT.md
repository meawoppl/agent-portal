# Import existing Codex conversations

In **Launch Session**, select the computer where Codex stores your conversations,
then choose **Import existing**. Search by name or text, optionally enter an
exact workspace directory, select conversations, and choose **Import selected**.
Use **Load more** to browse older results. Imports are limited to directories
under the launcher user's home folder, like ordinary Portal launches.

Imported conversations start **paused**. Open one in Portal, finish any running
turn in the original CLI or IDE, close that conversation there, and select
**Resume** in Portal. This continues the same native Codex thread, using its
saved context and working directory. It does not transplant running shell
commands, attach to another client's live process, or move project files.

**Copy conversation history** adds a snapshot of saved messages to Portal.
Codex retains its complete native context whether or not this option is selected.
Local images and audio appear as references; attachment bytes are not uploaded.
If history is unsupported by the installed Codex version, exceeds 8 MiB or
50,000 records, or cannot finish within 90 seconds, the import reports an error.
Update Codex or retry with history copying unchecked. History is never silently
truncated. Copied messages are subject to normal Portal retention settings.

Repeated imports open the same Portal session. A failed or disconnected request
can be retried. An import does not start an agent or send it a prompt. Only the
connected computer's owner can browse or import its conversations. Copied history
becomes part of the new Portal session and follows its normal sharing controls.

## Compatibility and implementation

Both server and launcher must support `session.import_codex_v1`. An older
launcher receives no unknown import command; the picker asks for an update.
Codex is the first supported provider. The shared protocol carries an agent type
and a provider-native source ID, so another agent can add its own adapter without
changing the picker contract or teaching the backend its storage layout.

`codex-session-lib::imports` uses `thread/list`, `thread/read`, and (for paginated
history) `thread/turns/list` through the typed Codex SDK. It never reads or edits
Codex SQLite tables or rollout files. Discovery and import are read-only native
operations; `thread/resume` runs only when the user resumes the paused session.
The launcher advertises its capability and bounds concurrent native readers.
Background replies do not block launcher heartbeats; socket closure cancels them.

Portal IDs are deterministic over the user, launcher, agent, and source ID.
The launcher atomically saves the native binding before replying. The backend
commits the paused session, owner membership, and history in one transaction.
Concurrent retries cannot duplicate history or enqueue input. If the backend
commit fails, the saved binding is harmless and a retry completes the import.
The launcher thread map and additive `codex_imports.json` metadata must be backed
up with its normal configuration. Imported contexts receive the Portal reminder
on the first user input after resume; importing itself sends no prompt.

For imported sessions (those listed in `codex_imports.json`), a failed Codex
resume or a missing mapping is an explicit error: the launcher sets
`SessionConfig::codex_require_resume`, so the conversation is never silently
replaced with a new thread. The existing launcher failure/backoff handling
still applies. Ordinary Portal-created Codex sessions keep falling back to a
fresh thread, since their transcript lives in Portal.

API: `GET /api/launchers/{id}/external-sessions/codex` accepts `search`,
`working_directory`, and an opaque `cursor`; `POST` accepts `source_id` and
`include_history`, returning `session_id` and `already_imported`.
