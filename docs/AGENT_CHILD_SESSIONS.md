# Agent child sessions

An agent can request a child through `agent-portal session start`. The default
policy holds the first request for the session owner: open **Children** in the
parent's header, inspect the exact host, directory, launch settings, plugins and
brief, then Approve or Deny. Approval launches that exact durable request. Later
requests from that parent launch automatically within its policy limits. Choose
**Off** to disable new requests or **Auto** to explicitly waive first approval.
Only the owner's signed browser session can change this policy or approve it;
a shared-session editor cannot approve on the owner's behalf.

```sh
agent-portal session start --dir /home/me/project --agent codex \
  --name parser-review --worktree scratch --prompt 'Review the parser changes' --json
agent-portal session start --dir /home/me/project --host build-server \
  --agent claude --model sonnet --worktree new-branch:parser-fix \
  --prompt-file brief.txt --plugins yapcad,-visilog --wait
agent-portal session list --json
agent-portal session stop CHILD_UUID --json
```

`--host` accepts an exact unique hostname or launcher UUID, defaulting to the
parent's launcher. It must be connected, belong to the same owner, and advertise
`launcher.cli_spawn`. Claude, Codex and Muse are supported; Antigravity is
rejected until its tool harness can carry the session credential.

`--json` writes one result/error object to stdout and exits nonzero on errors,
including `approval_required`, `child_limit`, `depth_limit`, and `rate_limit`.
An approval-required response includes a `request_id`. Repeating the identical
command with `--request-id UUID` reads/retries that request without creating a
second child. Changing a request's payload under that id is rejected. `--wait`
waits at most 120 seconds for an already-approved child to connect; it does not
wait for human approval or the agent's task to finish. After approval, use
`session list` or repeat the identical request with its id.

The parent's transcript includes an approval notice and a link to the reserved
child. The child's first input contains the parent's name/id and the brief;
it is committed with the session and replays after reconnect. The rail groups
children next to their parent and labels their parent; explicit host grouping
keeps a cross-host child under its host. Plugin overrides are installed before
the first process starts, survive reconnect, and do not overwrite later owner
changes. Unspecified plugins follow the normal detection rules.

## Limits and lifecycle

Defaults are four lifetime children, maximum depth two (root is depth one),
and a 30-minute idle timeout. Pending approval requests reserve child slots.
Denied/failed requests release slots; stopping a successful child does not.
The owner can set 1–32 children, depth 1–8 and idle timeout 60–604800 seconds.
Every owner has a fixed limit of twelve new requests per minute across all
parents. The launcher's normal configured maximum session count still applies.
Concurrent requests are serialized by the owner's database row lock.

Children inherit the parent policy's idle timeout and optional stop-on-parent-end
setting at creation. Idle means no recent input/output and no active turn; a
busy or rate-limited turn does not expire. The periodic liveness sweep pauses
expired children so launcher reconciliation cannot resurrect them. With
stop-on-parent-end enabled, pausing, replacing or deleting the parent also stops
its children on the next sweep. Disconnecting temporarily is not ending a parent.
The immutable depth survives parent deletion. Failed initial launches are marked
failed and paused; a caller can submit a new request after correcting the cause.
Initial launches recovered after a backend restart retain their worktree,
plugins and first prompt, rather than trying to resume a nonexistent agent
conversation. Request records retain the approving owner and outcome, even if
the parent is deleted; operational logs record requests and lifecycle actions.

## Credential boundary

The launcher supplies `PORTAL_SESSION_ID`, `PORTAL_SESSION_BACKEND` and
`PORTAL_SESSION_TOKEN` to the agent process. The CLI never falls back to the
launcher's owner token for these commands. The server verifies that the token
is bound to the parent session, not just that it belongs to the same user.
Runtime credentials are excluded from session snapshots and Debug output.

Session tokens cannot use the manual launch/fork APIs, mint or renew owner
credentials, register a launcher socket, or register a different Portal session UUID to bypass spawn policy. Legacy stop/pause/delete calls accept only self or direct children for session credentials; manually resuming requires an owner credential. Missing native Claude history starts a new native conversation under the same Portal UUID with an explicit transcript notice. Browser
consent endpoints reject Authorization headers and require the owner's signed
cookie, including in development mode. Owner personal API tokens retain their
broader manual-launch capability. This is an API credential boundary, **not an
OS sandbox**: agents sharing the owner's Unix account can potentially read
owner credentials from that account's files. Do not treat this approval system
as isolation from arbitrary shell access to those credentials.
