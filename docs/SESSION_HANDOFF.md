# Hand off a session to another agent

When an agent runs out of usage, **Hand off** continues its work with a
different agent. Open it from the **Hand off to …** bar that appears above the
input when the latest turn was refused for lack of usage, or choose another
agent in the session's **Fork…** dialog.

A fork can only continue the same agent, because it resumes that agent's own
conversation state. Claude cannot load a Codex thread, and Codex cannot load a
Claude session, so a handoff starts a fresh session of the other agent on the
same computer and queues a note as its first input. The note tells the new agent:

- which session it is taking over, and that the source has been stopped;
- how to read the source's transcript (`agent-portal message peek` for a quick
  look, `agent-portal message history … -o /tmp/…` for all of it);
- to check `git status` and `git diff` when it shares the source's directory,
  or where the source's checkout is when it does not;
- the source's own handoff summary, when it wrote one (below);
- the start of your most recent request and of the source's last reply;
- your instructions, or by default, to continue the most recent request.

The dialog carries over the source's permission preset (skip permissions or
auto mode) to the new agent, and lets you pick its model. Other CLI flags are
specific to each agent and are not carried over.

**The source is stopped and paused by default.** Two agents in one checkout
overwrite each other's work, and a usage-limited Claude resumes on its own
when its window resets. Resume the source later if you want it back.

The source is stopped only once the new session's note is durably queued.
If queuing fails, the new session has still launched, but the source keeps
running untouched and the dialog says so; retry the handoff or brief the new
session yourself.

## Handoff summaries

The transcript records what an agent did, not always why. So when an agent
reports that it is close to its usage limit, Portal asks it for a short handoff
summary while it still can:

- **Claude** sends a rate-limit event with status `allowed_warning`.
- **Codex** sends `account/rateLimits/updated` with each token count during a
  turn, built from the usage headers on OpenAI's responses; without those
  headers there is no warning and no summary request. Updates are sparse,
  so the Codex session merges them per `limit_id` and forwards one complete
  snapshot the first time a window reaches 90%.

The request goes into the session's work queue, **once per usage window**, so
it is sent after the current turn ends rather than interrupting it. You can
see it in the queue and delete it there; a deleted request is not made again
for that window. The queue holds while a session is out of usage, so the
request is never sent to an agent that can no longer answer.

The agent replies under the heading `## Handoff summary`. A later handoff takes
the newest such reply from the transcript and puts it in the note. Handing off
also deletes any summary request still waiting in the source's queue, so it
cannot fire when the source is resumed.

## Implementation

- `POST /api/sessions/{id}/handoff` (`backend/src/handlers/session_handoff.rs`)
  launches through the ordinary launch path, queues the note, and stops the
  source.
- `backend/src/handlers/handoff_summary.rs` queues summary requests from
  output ingest and finds summaries for the note.
- `shared/src/usage_limit.rs` holds the rules for usage-limit refusals and
  warnings, shared by the backend and the browser.
