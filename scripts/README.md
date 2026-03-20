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
./scripts/test-dev.sh

# Full OAuth mode - requires .env with Google OAuth
./scripts/test-oauth.sh

# Clean up everything
./scripts/clean.sh

# Open database shell
./scripts/db-shell.sh
```

## test-dev.sh

**Dev mode testing** - No OAuth required

This script:
- Starts PostgreSQL in Docker
- Runs database migrations
- Builds frontend
- Starts backend in dev mode (auto-creates test user)
- Starts proxy
- Opens browser to http://localhost:3000/

**Features:**
- ✅ Auto-authentication (testing@testing.local)
- ✅ No Google OAuth setup needed
- ✅ Perfect for quick testing and development

**Usage:**
```bash
./scripts/test-dev.sh
```

Opens browser and you're automatically logged in!

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
docker-compose -f docker-compose.test.yml up -d db

# Wait for it to be ready
docker-compose -f docker-compose.test.yml exec db pg_isready -U claude_portal
```

### 2. Run Migrations

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
cargo run -p proxy -- \
  --backend-url ws://localhost:3000 \
  --session-name "my-session"
```

## Log Files

Scripts write logs to `/tmp/`:
- `/tmp/agent-portal-backend.log` - Backend logs
- `/tmp/agent-portal-proxy.log` - Proxy logs

View logs:
```bash
tail -f /tmp/agent-portal-backend.log
tail -f /tmp/agent-portal-proxy.log
```

## Troubleshooting

### "Database connection failed"

```bash
# Check if database is running
docker ps | grep agent-portal

# If not, start it
docker-compose -f docker-compose.test.yml up -d db

# Check logs
docker-compose -f docker-compose.test.yml logs db
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
    ./scripts/test-dev.sh &
    sleep 10
    curl http://localhost:3000/
    ./scripts/clean.sh
```

## Environment Variables

Scripts respect these environment variables:

- `DATABASE_URL` - Override database connection
- `BACKEND_PORT` - Override backend port (default: 3000)
- `DEV_MODE` - Enable dev mode (default: true for test-dev.sh)

Example:
```bash
BACKEND_PORT=8080 ./scripts/test-dev.sh
```
