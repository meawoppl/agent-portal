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
| `cap-forward.js` | `feature-port-forward.webp` | The agent starts an `http.server` on the rizzma rustdoc and runs `agent-portal forward 8899` — both visible as tool cards — then the chip appears and the docs render live in the preview panel, navigable inside the frame |
| `cap-message.js` | `feature-agent-message.webp` | One session messages another; the message lands as a turn and that agent starts working |
| `cap-media.js` | `feature-show-media.webp` | `agent-portal show signals.riz` → poster in the transcript → play → the figure animates |
| `cap-metrics.js` | `feature-turn-metrics.webp` | The header sparkline building over turns, then switching which metric it plots |
| `cap-nav.js` | `feature-nav-mode.webp` | `Ctrl+K` → numbered pills, key legend, arrows and a number key jumping between sessions |
| `cap-agents.js` | `feature-multi-agent.webp` | The same dashboard switching between a Claude session and a Codex session, each in its own protocol's shape |
| `cap-handoff.js` | `feature-desktop-phone.webp` | Desktop and phone panes side by side on one session; the phone asks, both stream the answer |
| `cap-cast.js` | `feature-install-cast.webp` | Terminal cast of the real install script and `agent-portal login`, replayed from captured output |

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

The forward clip needs a little more: the agent shells out to the **real**
`agent-portal forward`, so the CLI needs a token, and it needs something worth
looking at on the other end.

```bash
# Something dynamic to serve — rustdoc, with its JS search and navigation
cargo doc -p rizzma --no-deps
mkdir -p "$DEMO_ROOT/home/rizzma-figs/target"
cp -r target/doc "$DEMO_ROOT/home/rizzma-figs/target/doc"

./reset-demo.sh                              # wipe sessions, THEN mint the CLI token
SID=$(curl -s -X POST "$DEMO_URL/api/launch" -H 'Content-Type: application/json' \
        -d '{"working_directory":"'"$DEMO_ROOT"'/home/rizzma-figs","launcher_id":"'"$LID"'",
             "claude_args":["--model","claude-haiku-4-5","--dangerously-skip-permissions"],
             "agent_type":"claude","name":"rizzma-figs","create_worktree":false}' \
      | python3 -c "import sys,json;print(json.load(sys.stdin)['session_id'])")
DEMO_SID=$SID node cap-forward.js
```

Permissions are skipped for that one on purpose — the clip is about forwarding,
and a permission card mid-sequence is noise the permission clip already covers.

`encode.sh` writes both an animated WebP and an APNG. **Ship the WebP** — same
quality at roughly a tenth the bytes (283 KB vs 3.0 MB for the permission clip).

## The two clips that are not plain captures

- **`cap-handoff.js`** drives **two browsers** and composites their frames.
  `record2()` screencasts both on one wall clock and resamples them onto a shared
  grid; `composite.py` lays the desktop and phone side by side. Two *tabs* in one
  browser do not work — see the gotchas.
- **`cap-cast.js`** records `cast.html`, a terminal that types the commands and
  replays **real captured output**. The text in it came from actually running
  `curl -fsSL https://txcl.io/api/download/install.sh | bash` and
  `agent-portal login` against production with a scratch `HOME`. Regenerate it by
  re-running those two commands and rebuilding the page from their output.

  The cast stops at `⏳ Waiting for authentication...` on purpose: approving the
  device code needs a real browser login, and `agent-portal service install`
  would install a user unit named `agent-portal.service` — the same name as the
  capture machine's own service. Neither is worth faking.

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
- **Mint the CLI token *after* wiping sessions.** `proxy_auth_tokens` carries a
  `session_id` FK, so `TRUNCATE sessions CASCADE` empties the whole token table —
  a token minted before the wipe leaves `agent-portal forward` failing with a
  puzzling 401 (`Token not found in database` in the backend log).
  `reset-demo.sh` does both in the right order.
- **The launcher config lives in `~/.config/agent-portal/`**, not
  `~/.config/claude-portal/`. Writing the token to the wrong one reads as
  "Not authenticated — run `agent-portal login` first".
- **Scrub before every launch, and check after.** Claude Code puts the account
  email in its context and its first-turn thinking often recites it
  ("6. The user's email is …"). `scrub.sh` strips `oauthAccount` identity fields
  from the scratch `~/.claude.json`; it must run **immediately before** the
  launch, because the CLI repopulates the file at startup — scrubbing before the
  read is what matters. Always verify afterwards:
  `psql -d readme_demo -c "SELECT count(*) FROM messages WHERE content::text ILIKE '%yourdomain%'"`,
  and reshoot if it is not zero.
- **Two tabs in one browser cannot both paint.** A page that is not the front tab
  serves stale pixels to the screencast — the transcript keeps updating in the
  DOM while the recording shows the old frame, and no combination of
  `--disable-renderer-backgrounding` / `--disable-backgrounding-occluded-windows`
  fixes it. Give each pane its own `puppeteer.launch()`.
- **A hidden element can survive in the screencast.** Setting `display: none` on
  the dev-mode banner changed style without changing layout, so the region never
  repainted and the banner stayed in the frames. `stage()` **removes** the node
  instead.
- **Some clicks need to be dispatched in-page.** Headless hit-testing misses the
  rizzma figure's overlay mount button; `cap-media.js` glides the cursor there
  and then calls `.click()` in-page. A real click on it does the same thing.
- **The launcher has a session cap** (20). Re-shoots accumulate in-process
  sessions even after the database rows are gone; when launches start failing
  with `At session limit (20/20)`, restart the launcher.
