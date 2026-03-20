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
