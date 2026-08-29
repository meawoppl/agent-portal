# Capture harness for the README animations

These scripts re-shoot the animated WebP clips in `docs/media/`. They drive a
**real portal instance** with headless Chrome over CDP — the clips are screen
recordings of the running app, not illustrations — and they run against a
**scratch instance**, never your normal dev database.

## What each script shoots

| Script | Clip | Story |
|--------|------|-------|
| `cap-launch.js` | `feature-launch-session.webp` | Launch dialog → machine, directory, model, worktree → session appears and the agent boots |
| `cap-permission.js` | `feature-permission-card.webp` | Prompt → Read/diff cards → **Permission Required** form → Allow → edit lands |
| `cap-forward.js` | `feature-port-forward.webp` | Forward registered (chip red) → server starts (chip green, names the process) → click → live app in the preview panel |
| `cap-message.js` | `feature-agent-message.webp` | One session messages another; the message lands as a turn and that agent starts working |

## Setup

```bash
export DEMO_ROOT=/tmp/readme-demo          # scratch root (default)
export DEMO_URL=http://localhost:3100      # scratch backend (default)

mkdir -p "$DEMO_ROOT" && cd "$DEMO_ROOT" && npm install puppeteer-core

# Isolated database — do NOT point this at your dev DB
docker exec claude-portal-test-db psql -U claude_portal -d postgres -c "CREATE DATABASE readme_demo;"

DATABASE_URL="postgresql://claude_portal:dev_password_change_in_production@localhost:5432/readme_demo" \
  PORT=3100 HOST=127.0.0.1 PORTAL_FORWARD_DOMAIN="localhost:3100" \
  cargo run -p backend -- --dev-mode
```

A scratch `HOME` keeps your real home directory (and your agent's global
`CLAUDE.md`, account email, and MCP config) out of frame — the launcher only
browses directories under its own home, and the agent only reads the config it
finds there:

```bash
mkdir -p "$DEMO_ROOT/home/.claude"
ln -s ~/.claude/.credentials.json "$DEMO_ROOT/home/.claude/.credentials.json"
# minimal ~/.claude.json with no oauthAccount email / displayName / org name
git init "$DEMO_ROOT/home/acme-api"        # the staged repo the demos work in

cd "$DEMO_ROOT/home" && HOME="$DEMO_ROOT/home" \
  agent-portal --backend-url ws://127.0.0.1:3100 --dev --no-update --name demo-workstation
```

## Shooting

```bash
./reset.sh                                   # wipe sessions, prune worktrees/branches
./launch-api.sh rate-cache true              # seed a session (name, worktree)
node cap-permission.js                       # writes $DEMO_ROOT/frames/perm/
./trim.py "$DEMO_ROOT/frames/perm" "$DEMO_ROOT/frames/perm-trim" 6 173
./encode.sh "$DEMO_ROOT/frames/perm-trim" "$DEMO_ROOT/out-permission" 16 900
```

`encode.sh` writes both an animated WebP and an APNG. **Ship the WebP** — same
quality at roughly a tenth the bytes (283 KB vs 3.0 MB for the permission clip).

## How the harness works

- `lib.js::record()` runs `Page.startScreencast` while an `action` callback
  drives the UI, then resamples the frames Chrome pushed (which arrive only when
  pixels change) onto a fixed fps grid. `action` receives a `mark(label)`
  function; each mark is reported as a frame index so you can trim to the beat.
- `lib.js::stage()` hides the dev-mode banner and injects a synthetic cursor.
  Headless Chrome does not draw a pointer, and without one every click looks
  like the UI moving on its own. `clickAt`/`clickNth` glide the cursor to the
  target, pulse it, then dispatch the real click.
- Frames come back at `deviceScaleFactor` resolution because Chrome is launched
  with `--force-device-scale-factor`; shoot at 2× and downscale to 900 px.

## Gotchas

- **Browse the portal on the same origin as `BASE_URL`** (`localhost`, not
  `127.0.0.1`). The forward preview iframe is allowed by
  `frame-ancestors 'self' {portal origin}`; a mismatched origin silently blocks it.
- **Private forwards can't preview on `*.localhost`** — the browser treats the
  forward origin as cross-site, so the `SameSite=Lax` cookie is not sent.
  `cap-forward.js` marks the forward public, which needs no cookie.
- **Reset between takes.** A worktree branch left behind fails the next launch
  with `a branch named '…' already exists`; `reset.sh` prunes it.
- **Check for leaked identity before publishing.** The agent's own config can
  put your email in a thinking block:
  `psql -d readme_demo -c "SELECT count(*) FROM messages WHERE content::text ILIKE '%yourdomain%'"`.
