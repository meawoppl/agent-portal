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

The `serve` subcommand (below) embeds the `viewer-frontend` WASM bundle. To
bundle the real UI, build it first:

```bash
cd viewer-frontend && trunk build        # writes viewer-frontend/dist/
cargo build -p archive-viewer            # embeds dist/ into the binary
```

If `viewer-frontend/dist/` is absent, the crate still builds (the CLI does not
need the UI): `serve` then runs the JSON API with an empty UI until you
`trunk build`. See `build.rs` for the rationale.

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

### `serve`

Runs a small HTTP server that exposes the archive over a JSON API and serves
the embedded web viewer (`viewer-frontend`). Point a browser at the printed URL
to browse users, sessions, and full transcripts (media included).

```bash
portal-archive --local-root /srv/archive serve                 # http://127.0.0.1:8890/
portal-archive --local-root /srv/archive serve --port 9000 --open
portal-archive --s3-bucket my-archive serve --refresh-secs 30  # cache scans for S3
```

Flags: `--port` (default `8890`), `--open` (open the browser once listening),
`--refresh-secs` (seconds to cache the scanned manifest list before rescanning;
`0` rescans every request — freshest, but slower on large S3 archives; default
`10`).

**⚠️ NO AUTHENTICATION — LOOPBACK ONLY, BY DESIGN.** `serve` performs no
authentication or authorization. It is an operator tool over
operator-controlled archive data (the same data you can already read off disk /
S3 with the other subcommands), so it **binds to `127.0.0.1` only** and never to
a routable address. Anyone who can reach the port can read every archived
session for every user. **Do not** port-forward it, place it behind a reverse
proxy, or bind it to `0.0.0.0`. Multi-user authenticated access is the portal
backend's job, not this tool's.

#### API

| Endpoint | Returns |
|----------|---------|
| `GET /api/users` | `[{user_id, owner_email?, owner_name?, session_count}]` (identity from each user's newest manifest) |
| `GET /api/sessions?user=&from=&to=&agent=&q=` | session summary rows (same flattening as `list`) |
| `GET /api/sessions/{user}/{session}/manifest` | the manifest JSON, verbatim |
| `GET /api/sessions/{user}/{session}/messages` | the transcript as NDJSON (zstd-decoded server-side, streamed) |
| `GET /api/media/{user}/{session}/{media_id}` | media bytes, with HTTP `Range` support (single + suffix ranges, `206`/`416`, `Accept-Ranges`) |
| `GET /api/rollup?group_by=user\|agent\|model&from=&to=` | rollup rows `{group, label?, session_count, message_count, total_cost_usd, …token/tool/media extras}` |

Missing objects return `404` with a plain-text message; corrupt manifests are
skipped with a warning (mirroring the CLI).

#### Screenshot

<!-- TODO(screenshot): add a screenshot of the viewer session browser here,
     e.g. archive-viewer/docs/serve-browser.png, once the UI styling settles. -->
_A screenshot of the web viewer will go here._

## Graceful degradation

A corrupt manifest is warned about on stderr and skipped; a session with no
archived transcript reports so plainly; an empty archive prints a friendly
message rather than an empty table. `serve` follows the same rules — a bad
manifest is skipped during the scan and missing objects return `404`.
