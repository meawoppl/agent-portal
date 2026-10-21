# End-to-End Testing Guide (Updated)

Test the complete system including OAuth device flow.

## Quick Test (Dev Mode - No OAuth)

```bash
# Terminal 1: Start database
docker-compose up -d db
sleep 5

# Run migrations
export DATABASE_URL="postgresql://claude_portal:dev_password_change_in_production@localhost:5432/claude_portal"
cd backend && diesel migration run && cd ..

# Terminal 2: Build frontend
cd frontend && trunk build --release && cd ..

# Terminal 3: Run backend in dev mode
cargo run -p backend -- --dev-mode

# Terminal 4: Run proxy
cargo run -p proxy -- --backend-url ws://localhost:3000

# Open browser: http://localhost:3000/
```

## Full Test (With OAuth Device Flow)

### Prerequisites

1. **Get Google OAuth credentials**:
   - Go to https://console.cloud.google.com/apis/credentials
   - Create OAuth 2.0 Client ID
   - Add redirect URI: `http://localhost:3000/api/auth/google/callback`
   - Copy client ID and secret

2. **Configure .env**:
   ```bash
   cp .env.example .env
   # Edit .env with your OAuth credentials
   ```

### Step 1: Start Services

```bash
# Terminal 1: Database
docker-compose up -d db
sleep 5

# Run migrations
export DATABASE_URL="postgresql://claude_portal:dev_password_change_in_production@localhost:5432/claude_portal"
cd backend && diesel migration run && cd ..
```

### Step 2: Build Frontend

```bash
# Terminal 2
cd frontend
trunk build --release
cd ..
```

### Step 3: Run Backend

```bash
# Terminal 3
source .env  # Load OAuth credentials
cargo run -p backend

# You should see:
# Listening on 0.0.0.0:3000
# Serving embedded frontend assets
```

### Step 4: Run Proxy (First Time)

```bash
# Terminal 4
cd ~/test-project  # Use any directory
cargo run -p proxy -- --backend-url ws://localhost:3000

# You'll see:
╔═══════════════════════════════════════════════════════╗
║           🔐 Authentication Required                  ║
╚═══════════════════════════════════════════════════════╝

  To authenticate this machine, please visit:

    http://localhost:3000/api/auth/device

  And enter the code:

    ABC-123

  ⏳ Waiting for authentication...
```

### Step 5: Authenticate in Browser

1. Open http://localhost:3000/api/auth/device?user_code=ABC-123
2. You'll be redirected to Google OAuth
3. Sign in with your Google account
4. Grant permissions
5. You'll be redirected back with "Authentication successful"

### Step 6: Verify in Terminal

Terminal 4 should show:
```
  ✓ Authentication successful!
  Logged in as: your.email@gmail.com

  Session registered with backend
  Claude CLI process spawned
```

### Step 7: Test Web Interface

1. Open http://localhost:3000/
2. Click "Sign in with Google"
3. Log in with same Google account
4. You should see your session portal on the dashboard
5. Click the portal to open terminal interface

## Test Config Persistence

```bash
# Check config file was created
cat ~/.config/agent-portal/config.json

# Should show:
{
  "sessions": {
    "/path/to/test-project": {
      "user_id": "...",
      "auth_token": "eyJhbGciOiJIUzI1NiIs...",
      "user_email": "your.email@gmail.com",
      "last_used": "...",
      "backend_url": "ws://localhost:3000"
    }
  },
  "preferences": {
    "default_backend_url": null,
    "auto_open_browser": false
  }
}
```

## Test Cached Authentication

```bash
# Stop proxy (Ctrl+C in Terminal 4)

# Run again - should use cached auth
cargo run -p proxy -- --backend-url ws://localhost:3000

# Should connect immediately without OAuth prompt!
```

## Test Different Accounts

```bash
# Logout from current directory
cargo run -p proxy -- --logout

# Run again to authenticate with different account
cargo run -p proxy

# Authenticate with a different Google account
```

## Test Multiple Projects

```bash
# Terminal 5: Another project
cd ~/different-project
cargo run -p proxy

# If you haven't authenticated this directory yet, you'll get OAuth flow
# After auth, both sessions should appear in web dashboard
```

## Verification Checklist

- [ ] Database migrations applied
- [ ] Frontend builds successfully
- [ ] Backend serves frontend at `/`
- [ ] Proxy shows OAuth device flow on first run
- [ ] Can open device URL and complete OAuth
- [ ] Proxy receives auth token after OAuth
- [ ] Config file created at `~/.config/agent-portal/config.json`
- [ ] Config contains auth token
- [ ] Second run uses cached auth (no OAuth prompt)
- [ ] Web UI shows login with Google
- [ ] Web UI OAuth login works
- [ ] Dashboard shows active proxy session
- [ ] Can open terminal interface
- [ ] Messages flow between web UI and proxy
- [ ] `--logout` flag removes cached auth
- [ ] `--reauth` flag forces new OAuth flow
- [ ] Multiple directories can have different accounts

## Troubleshooting

### OAuth redirect mismatch

Make sure your Google OAuth redirect URI exactly matches:
```
http://localhost:3000/api/auth/google/callback
```

### Device code not found

The code expires after 15 minutes. Get a new one by running the proxy again.

### Config file not created

Check permissions on `~/.config/` directory:
```bash
mkdir -p ~/.config/agent-portal
chmod 700 ~/.config/agent-portal
```

### Token not working

Delete config and re-authenticate:
```bash
rm ~/.config/agent-portal/config.json
cargo run -p proxy
```

## Clean Up

```bash
# Stop all services (Ctrl+C in each terminal)

# Remove database
docker-compose down -v

# Remove config (optional)
rm -rf ~/.config/agent-portal
```

### Migration structural roundtrip

The `Migration Roundtrip` CI job uses PostgreSQL 16 and pinned `diesel_cli`
2.3.11. `scripts/check-migration-roundtrip.py` creates its own randomly named
`ap_migration_check_*` database, applies every migration, checks Diesel's
printed schema against `backend/src/schema.rs`, reverts every migration, and
reapplies them. It compares both generated Rust schema and a full schema-only
PostgreSQL dump before/after, including indexes, constraints and functions.
The script never rewrites `schema.rs` or edits the migration registry itself.

To reproduce, install that Diesel CLI with PostgreSQL support, PostgreSQL
client tools, and the pinned toolchain's `rustfmt` component (Diesel uses it to
format generated schema), then run:

```sh
DATABASE_URL=postgresql://localhost/postgres python3 scripts/check-migration-roundtrip.py
python3 scripts/test_migration_roundtrip.py
```

`DATABASE_URL` is only the maintenance connection for creating/dropping the
owned test database; its existing database is never migrated or reverted. The
role needs `CREATEDB`; `dbname` query overrides are rejected. Cleanup drops only the database successfully created by
this run, including after migration/comparison failures, Ctrl-C or SIGTERM.
Cancellation while `createdb` itself is running can leave the newly named test
database behind: ownership is only recorded after successful creation, so a
failed creation never drops a database it cannot prove it created. SIGKILL also
cannot run cleanup; CI tears down its disposable service regardless. Use a local test
PostgreSQL instance, as CI does.

This checks structural reversibility on a fresh database, not restoration of
production data. Irreversible cleanups/backfills intentionally use `SELECT 1`
in their down migration: a comments-only file is rejected as an empty query by
PostgreSQL. Data-bearing upgrade fixtures and recovery testing remain separate.

## Long-transcript browser measurements

`scripts/benchmark-transcript.py` serves a built WASM bundle with deterministic
HTTP/WebSocket fixtures. It makes no model calls and writes no live session data.
Build the bundle you intend to compare (release is preferred for performance), then:

```bash
uv run --with playwright python scripts/benchmark-transcript.py \
  --label "release, current commit" --repeat 3
uv run --with playwright python scripts/benchmark-transcript.py \
  --label "release, current commit, narrow viewport" --width 390 --repeat 3 \
  --output builds/transcript-benchmark-mobile.json
```

Use `--dist PATH` to compare an existing artifact without rebuilding. Install the
Playwright Chromium browser first if it is not available. Compare the same build
profile, browser, hardware, viewport and fixture settings; a debug bundle is a
functional smoke test, not a production latency measurement.

The two default fixtures contain 100 counted messages, either alone or with 5,000
bodyless thinking-token markers inside that retained window. The server fixture
advertises a 100,000-row history but honors the dashboard's `render_limit=100`
contract: measuring an oversized REST response would test an unrealistic path.
A second batch arrives through the real history-replay renderer. Assertions verify
that the newest messages appear, old visible messages leave the render window,
and the page raises no JavaScript errors. Fixture version 2 asserts all 50 user
and 50 assistant bodies use typed renderers (no raw-JSON fallback), all 100
message sentinels remain, and the thinking chip exposes the expected token
estimate.

The hydration timer starts immediately before fulfilling the intercepted history
HTTP response, after the WASM app has loaded and requested it. Replay timing
starts immediately before injecting a `HistoryBatch` into the existing socket.
Both stop two animation frames after the newest sentinel enters the DOM. They
include local fixture serialization/transport and initial layout, not app
bootstrap, a disconnect/reconnect handshake, or completion of count-up animations.

The JSON report records these wall times, final whole-page DOM node count,
individual samples, medians, browser version and fixture version. Timing is deliberately not a CI pass/fail threshold: host contention and
browser instrumentation affect it. Record a baseline before changing retention,
grouping, or windowing, and compare both ordinary and marker-heavy transcripts.
This harness does not measure backend database query time or the archive viewer.

The initial release-bundle baseline is in
[`benchmarks/transcript-6228fe55.json`](benchmarks/transcript-6228fe55.json).
On the shared Linux build host, median desktop hydration/replay was 167/81 ms
for 100 ordinary messages and 509/341 ms with 5,000 markers. The narrow viewport
was 158/83 ms and 500/342 ms respectively. The marker burst adds processing
cost without thousands of extra DOM nodes. These measurements include fixture
serialization/transport and two animation frames, without isolating other host
activity; they establish a reproducible workload, not a production responsiveness
claim.
