# Testing Scripts

Helper scripts for local development and testing.

## First Time Setup

```bash
# Install all dependencies (diesel CLI, trunk, etc.)
./scripts/install-deps.sh
```

This will install:
- diesel CLI (for database migrations)
- trunk (for building WASM frontend)
- Check for Docker/Docker Compose

## Quick Start

```bash
# Dev mode (easiest) - no OAuth required
./scripts/dev.sh start

# Full OAuth mode - requires .env with Google OAuth
./scripts/test-oauth.sh

# Clean up everything
./scripts/clean.sh

# Open database shell
./scripts/db-shell.sh
```

## dev.sh

**Development environment manager** - No OAuth required

```bash
./scripts/dev.sh start    # Start DB + migrations + frontend build + backend
./scripts/dev.sh status   # Show status of all services
./scripts/dev.sh logs     # Tail backend logs (or: logs db)
./scripts/dev.sh stop     # Stop all services
./scripts/dev.sh restart  # Restart all services
./scripts/dev.sh build    # Rebuild frontend only
./scripts/dev.sh nuke-db  # Delete all database data and start fresh
```

`start` runs the backend in dev mode (auto-authenticates as
testing@testing.local) and leaves everything running in the background.

## test-oauth.sh

**Full OAuth testing** - Requires Google OAuth credentials

This script:
- Starts PostgreSQL in Docker
- Runs database migrations
- Builds frontend
- Starts backend with OAuth enabled
- Starts proxy (will display OAuth device code)

**Prerequisites:**
```bash
# 1. Create .env file
cp .env.example .env

# 2. Get Google OAuth credentials
#    https://console.cloud.google.com/apis/credentials

# 3. Add to .env:
GOOGLE_CLIENT_ID=your_id.apps.googleusercontent.com
GOOGLE_CLIENT_SECRET=your_secret
```

**Usage:**
```bash
./scripts/test-oauth.sh
```

Follow the OAuth device code flow displayed in terminal.

## clean.sh

**Cleanup** - Stops everything and removes artifacts

This script:
- Kills all running processes (backend, proxy, trunk)
- Stops Docker containers and removes volumes
- Cleans cargo build artifacts
- Removes log files
- Optionally removes ~/.config/agent-portal/

**Usage:**
```bash
./scripts/clean.sh
```

## db-shell.sh

**Database access** - Opens psql shell

Opens an interactive PostgreSQL shell to inspect data.

**Usage:**
```bash
./scripts/db-shell.sh

# Then run SQL:
claude_portal=# SELECT * FROM users;
claude_portal=# SELECT * FROM sessions;
claude_portal=# \q
```

## Manual Testing (Without Scripts)

### 1. Start Database Only

```bash
docker compose -f docker-compose.test.yml up -d db

# Wait for it to be ready
docker compose -f docker-compose.test.yml exec db pg_isready -U claude_portal
```

### 2. Run Migrations

The dev database URL lives in `scripts/lib.sh` (`DEV_DATABASE_URL`) and must
match the credentials in `docker-compose.test.yml`:

```bash
export DATABASE_URL="postgresql://claude_portal:dev_password_change_in_production@localhost:5432/claude_portal"
cd backend && diesel migration run && cd ..
```

### 3. Build Frontend

```bash
cd frontend && trunk build --release && cd ..
```

### 4. Start Backend

```bash
# Dev mode
export DEV_MODE=true
cargo run -p backend -- --dev-mode

# OR with OAuth (requires .env)
cargo run -p backend
```

### 5. Start Proxy

```bash
cargo run -p claude-portal -- \
  --backend-url ws://localhost:3000 \
  --session-name "my-session"
```

## Log Files

Scripts write logs to `/tmp/`:
- `/tmp/claude-portal-backend.log` - Backend logs

View logs:
```bash
tail -f /tmp/claude-portal-backend.log
```

## Troubleshooting

### "Database connection failed"

```bash
# Check if database is running
docker ps | grep claude-portal

# If not, start it
docker compose -f docker-compose.test.yml up -d db

# Check logs
docker compose -f docker-compose.test.yml logs db
```

### "Port 5432 already in use"

```bash
# Check what's using it
lsof -i :5432

# If it's another postgres, either stop it or change the port in docker-compose.test.yml
```

### "Port 3000 already in use"

```bash
# Find process
lsof -i :3000

# Kill it
kill -9 <PID>
```

### "diesel: command not found"

```bash
cargo install diesel_cli --no-default-features --features postgres
```

### "trunk: command not found"

```bash
cargo install trunk
```

### Scripts fail with "permission denied"

```bash
chmod +x scripts/*.sh
```

## CI/CD Integration

These scripts are designed to work in CI environments:

```yaml
# .github/workflows/test.yml
- name: Run tests
  run: |
    ./scripts/dev.sh start
    curl http://localhost:3000/
    ./scripts/dev.sh stop
```

### Remote review of a PR

From a checkout of this repository, run:

```bash
./scripts/dev.sh preview 1234 --forward --ssh-host user@build-host
```

This builds the PR's **fetched head**, in a new detached worktree, first with
Trunk and then Cargo (the backend embeds that frontend). It starts a fresh
Postgres container with no persistent volume, applies the PR's migrations at
backend startup, waits for health, then prints a private Portal URL. Your
current branch, working changes, development database and existing servers are
untouched. The printed SHA identifies exactly what is being reviewed; restart
the command to fetch subsequent PR changes. Cold builds can take several minutes.

Prerequisites: Python 3, Git, Docker, Rust with the WASM target, Trunk, and an
authenticated `origin` remote. `--forward` additionally needs `agent-portal` in
an active session with no existing forward. Only run **trusted PR code**: build
scripts run with your account's permissions, just as with `cargo build`.

For the reviewer, sign in to the regular Portal in the same browser, then open
the printed URL. Give the reviewer session access through Share Session first.
The forward remains private; this command never makes a dev-mode backend public.
Do not use a logged-out incognito window: the Portal's access gate still applies.
A new forward gets a fresh automatic subdomain, so previous service workers
cannot control it. Use the printed URL, not a stable custom alias. The preview itself uses the isolated `testing@testing.local` account.

For same-machine review, omit `--forward` and open the printed `127.0.0.1` URL.
For remote review without Portal, the command prints a ready-to-paste `ssh -L`
command. SSH authenticates the reviewer and carries traffic to the loopback-only
backend; no public dev-mode listener is created. If the reviewer's local port is
occupied, change only the first port in `-L` and the browser URL. All printed URLs
use explicit IPv4 loopback, avoiding IPv6 `localhost` ambiguity.

Keep the command running while reviewing. Ctrl-C stops its backend, removes its
forward (if requested), removes its worktree/build output and destroys its
preview database. The small `backend.log` in the printed temporary directory
remains for diagnosis. Build directories live under
`$XDG_CACHE_HOME/agent-portal/previews` (normally `~/.cache`), rather than a
potentially memory-backed `/tmp`. Debug symbols and incremental artifacts are
disabled for these throwaway builds. Do not change this session's forward while previewing.
No production data or services are used. Do not connect a real launcher to this
disposable preview. To survive terminal disconnection, run the command in a
terminal multiplexer or a user systemd service; stop that service to clean up.
