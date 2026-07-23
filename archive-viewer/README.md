# archive-viewer (`portal-archive`)

A standalone CLI for browsing and summarizing agent-portal's long-term session
archive — the objects written by the backend's archival sweep, whose on-disk /
on-S3 format lives in the `archive-format` crate. It links only
`archive-format`, never the backend, so it can run anywhere the archive is
reachable: a local filesystem root or an S3-compatible bucket.

## Build

```bash
cargo build -p archive-viewer            # produces target/debug/portal-archive
```

## Target selection

Every command reads from one archive target, resolved in this order:

1. `--local-root <path>` — a local filesystem archive root.
2. `--s3-bucket <bucket> [--s3-prefix <prefix>]` — an S3 (or S3-compatible)
   bucket; region/credentials/endpoint come from the standard `AWS_*`
   environment variables.
3. Otherwise the `PORTAL_SESSION_ARCHIVE_*` environment variables (the same
   ones the backend uses) are read.

If none resolve, the command exits with a clear error.

## Commands

```bash
# List archived sessions (one row each), most-recently-active first.
# Manifest-only — transcripts are never read.
portal-archive --local-root /srv/archive list
portal-archive --local-root /srv/archive list \
    --user alice --agent claude \
    --from 2026-07-01 --to 2026-07-31 --name refactor

# Aggregate manifest metrics into a grouped table.
portal-archive --local-root /srv/archive rollup --group-by user   # or agent | model
portal-archive --local-root /srv/archive rollup --from 2026-07-01

# Export every flattened manifest row (all fields, incl. provenance).
portal-archive --local-root /srv/archive export --format csv -o sessions.csv
portal-archive --local-root /srv/archive export --format json

# Print a readable transcript digest for one session (short id prefix ok).
portal-archive --local-root /srv/archive cat aaaaaaaa
portal-archive --local-root /srv/archive cat aaaaaaaa --raw   # dump NDJSON verbatim
```

### `list`

Columns: session id (short), name, agent, status, user email, hostname,
created, last activity, message count, cost, models, media count. Sorted by
last activity, descending. Filters: `--user` (email substring or session/user
UUID prefix), `--agent`, `--name` (substring), and `--from`/`--to` (RFC3339 or
`YYYY-MM-DD`, matched against last activity).

### `rollup`

Aggregates from manifests alone. `--group-by user|agent|model` (default
`user`) with the same date filters. A session that used multiple models
contributes its full totals to *each* model's row (manifests carry no per-model
token split), so per-model totals can exceed the grand total.

### `export`

A full dump (no filters) of every manifest row as CSV (default) or JSON,
including the token breakdown, turn stats, and provenance (`client_version`,
`launcher_id`/`launcher_version`, `scheduled_task_id`, `archived_by_version`).
Writes to stdout, or to a file with `-o`.

### `cat`

Resolves a session by full id or unique short prefix across all users, then
prints a one-line-per-message digest (local-time timestamp, role, and a compact
content summary; `thinking` blocks are skipped). `--raw` dumps the stored
NDJSON transcript verbatim instead.

## Graceful degradation

A corrupt manifest is warned about on stderr and skipped; a session with no
archived transcript reports so plainly; an empty archive prints a friendly
message rather than an empty table.
