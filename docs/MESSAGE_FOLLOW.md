# Following a peer

`agent-portal message follow <session-id> --json` produces bounded, line-oriented
activity records. `message peek <id> --follow` is an alias. Run it under a
background-process monitor to wake a supervisor on output; it does not itself
inject messages or keep a supervising agent alive across its turn boundaries.

```sh
agent-portal message follow abc123 --batch-quiet 2s --max-wait 15s --json
agent-portal message follow abc123 --only text,error --grep 'PR|failed' --json
agent-portal message follow abc123 --since 'v1:FULL-SESSION-UUID:123' --until-idle
agent-portal message history abc123 --since 'v1:FULL-SESSION-UUID:123' --stdout
agent-portal message history abc123 --tail 100 --stdout
```

With no `--since`, following starts at the current transcript end. Save the last
**fully consumed record's** `cursor`, and pass it back unchanged after a restart.
A `batch` record includes `start_cursor`, `cursor`, and individual event cursors.
A `status` or `heartbeat` record also carries a checkpoint. Consumers must commit
their own work and checkpoint together: if a consumer crashes after processing a
batch but before saving its cursor, replay can duplicate its side effects.

The CLI holds a read-only HTTP request open until activity or a bounded timeout;
it does not repeatedly fetch a recent peek window. Server wakeups are bounded,
lossy hints; the database journal provides catch-up even after missed hints or a
backend restart. Each page reads at most 100 messages, ordered by transactional
per-session sequence, rather than timestamps or random UUIDs. Sequences are
assigned inside message insertion transactions; concurrent commits cannot slip
behind a cursor. Installation backfills retained messages only. The cursor does
not prove that pre-installation transcript retention never occurred.

Default kinds are `text,status,send,error`. `send` is a received inter-agent
message visible in this transcript, not proof that a remote send completed.
Opt in to `tool`, `tool_result`, `thinking`, `input`, `system`, or `unknown` as
needed. Tool outputs and thinking are never included by default. Summaries are
single lines capped at 240 characters, reusing peek's protocol-aware normalizer;
this is a digest, not the full assistant prose. `commit`, `pr`, and `file` are not
inferred from shell text: use the explicit tool opt-in or a git/GitHub watcher.
Unknown agent protocols degrade to summaries rather than raw payloads.

`--grep` is a Rust regular expression over summary text. Status transitions
bypass the regex unless `status` is explicitly excluded by `--only`. Batches flush
after `--batch-quiet` (default 2s), `--batch-count` (20, max 100),
`--batch-chars` (4000, max 24000), or `--max-wait` (15s), whichever comes first.
Status changes flush pending text immediately. A single event can exceed the
character target; its 240-character cap still applies. Filtered events advance
the checkpoint too.

Heartbeats (default 60s) include the cursor, observed activity state and
connectivity. These are **live snapshots**, not durable historical status events:
a brief busy/idle transition entirely within a disconnect may be missed.
`--until-idle` exits only after observing an active → idle transition on a
connected peer; starting on an idle or disconnected peer does not immediately
exit. It cannot guarantee observing every past completed turn.

Transport failures and server errors retry with capped backoff, reporting to
stderr. Healthy heartbeats stop while the server is unreachable; monitors should
alert if none arrives within the heartbeat interval plus a reasonable network
grace. Authorization and invalid/gapped cursor errors stop the process. The
server rechecks membership on every page and wake, at most 30 seconds apart.
Tokens are checked on each request. Revoked viewers cannot continue reading.

If retention removed a body, follow stops with an explicit gap, rather than
silently skipping it. `message history --since` uses the same retained sequence
index and unions archived and hot transcript bodies. It errors if either source
cannot supply every selected message. Incremental history requires the session's
index still to exist; after session deletion, download the full archive instead.
Raw history is intentionally not summary-safe. `--tail` applies after `--since`.
The small sequence/UUID index remains until session deletion; transcript bodies
still obey existing retention. No new full-transcript copy is kept.

Durable server-side subscriptions and automatic supervisor re-invocation remain
future work. A follow process must still be hosted somewhere persistent (for
example a systemd user unit) if it needs to survive its shell session.
