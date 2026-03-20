# Deployment Guide

This guide covers deploying agent-portal to production.

## Prerequisites

- **PostgreSQL Database**
  - [NeonDB](https://neon.tech) (recommended, serverless)
  - Or any PostgreSQL 12+ instance

- **Google OAuth Credentials** (see [Google OAuth Setup](#google-oauth-setup) below)

## Environment Variables

Create a `.env` file or set these environment variables:

```bash
# Required
DATABASE_URL=postgresql://user:password@host:5432/database?sslmode=require
GOOGLE_CLIENT_ID=your-client-id.apps.googleusercontent.com
GOOGLE_CLIENT_SECRET=your-client-secret
GOOGLE_REDIRECT_URI=https://your-domain.com/api/auth/google/callback
SESSION_SECRET=generate-a-random-32-char-secret-here

# Optional - Server configuration
# HOST=0.0.0.0
# PORT=3000
# BASE_URL=https://your-domain.com

# Optional - Customize app title (default: "Agent Portal")
# APP_TITLE=Agent Portal

# Optional - Google Cloud Speech-to-Text (for server-side voice transcription)
# GOOGLE_APPLICATION_CREDENTIALS=/path/to/service-account.json

# Optional - Proxy binary path for downloads (auto-detected)
# PROXY_BINARY_PATH=/app/claude-portal

# Optional - Access control (restrict who can sign in)
# ALLOWED_EMAIL_DOMAIN=yourcompany.com
# ALLOWED_EMAILS=user1@gmail.com,user2@example.com

# Optional - Message retention (data cleanup)
# MESSAGE_RETENTION_COUNT=100    # Max messages per session (default: 100)
# MESSAGE_RETENTION_DAYS=30      # Delete messages older than N days (default: 30, 0=disabled)

# Optional - Session cleanup
# SESSION_MAX_AGE_DAYS=14        # Delete sessions older than N days (default: 14, 0=disabled)

# Optional - Image size limit for proxies
# PORTAL_MAX_IMAGE_MB=10         # Max image size in MB to inline (default: 10)
```

## Docker Deployment (Recommended)

```bash
# Build images
docker-compose build

# Start services
docker-compose up -d

# View logs
docker-compose logs -f backend

# Stop services
docker-compose down
```

See [DOCKER.md](DOCKER.md) for detailed Docker deployment instructions.

## Manual Deployment

### 1. Set up PostgreSQL database

Create a database and note the connection string.

### 2. Configure environment

```bash
export DATABASE_URL="postgresql://..."
export GOOGLE_CLIENT_ID="..."
export GOOGLE_CLIENT_SECRET="..."
export GOOGLE_REDIRECT_URI="https://yourdomain.com/api/auth/google/callback"
export SESSION_SECRET="$(openssl rand -base64 32)"
```

### 3. Build frontend

```bash
cd frontend
trunk build --release
cd ..
```

### 4. Run migrations

```bash
cd backend
diesel migration run
cd ..
```

### 5. Start backend

```bash
cargo run --release -p backend
```

### 6. Distribute proxy binary

```bash
cargo build --release -p proxy
# Copy target/release/claude-portal to dev machines
```

## Backend Command-Line Options

```bash
cargo run -p backend -- [OPTIONS]

Options:
  --dev-mode              Enable development mode (bypasses OAuth)
  -h, --help              Print help
```

Note: The frontend is embedded into the backend binary at compile time. There is no separate `--frontend-dist` option.

## Proxy Command-Line Options

```bash
claude-portal [OPTIONS] -- [CLAUDE_ARGS]

Options:
  --backend-url <URL>     Backend WebSocket URL [default: wss://txcl.io in release]
  --session-name <NAME>   Session name [default: hostname-timestamp]
  --auth-token <TOKEN>    Authentication token (skips OAuth flow)
  --init <TOKEN_URL>      Initialize with setup token from web UI
  --reauth                Force re-authentication
  --logout                Remove cached credentials and exit
  --new-session           Start fresh session (don't resume previous)
  --dev                   Development mode (bypass auth)
  --shim                  Shim mode for VS Code extension
  --agent <AGENT>         Agent CLI to use: "claude" (default) or "codex"
  --no-update             Skip automatic update check
  --update                Force update from GitHub releases
  --check-update          Check for updates without installing
  -v, --verbose           Enable debug-level logging

  # All arguments after -- are forwarded to the agent CLI
```

## Admin Setup

To grant admin privileges to a user:

```bash
# Open a database shell
./scripts/db-shell.sh

# Or connect directly with psql
psql $DATABASE_URL
```

```sql
-- Grant admin privileges to a user
UPDATE users SET is_admin = true WHERE email = 'your@email.com';
```

Admins can access the admin dashboard at `/admin` which provides:
- System statistics (users, sessions, spend)
- User management (enable/disable, grant/revoke admin)
- Session management (view all sessions, force delete)

## Security Considerations

- **OAuth Tokens**: Stored securely in database, never exposed to frontend
- **WebSocket Auth**: All WebSocket connections require valid auth tokens
- **Session Isolation**: Users can only access their own sessions
- **HTTPS**: Use HTTPS in production (handled by reverse proxy)
- **Environment Secrets**: Never commit `.env` to version control
- **Database**: Use SSL/TLS for database connections in production
- **Data Retention**: Message data is automatically deleted based on `MESSAGE_RETENTION_DAYS` (default 30) and per-session limits (`MESSAGE_RETENTION_COUNT`, default 100). Adjust these values based on your compliance requirements.

## Platform Support

| Platform | Status | Notes |
|----------|--------|-------|
| Linux (x86_64) | Tested | Primary development platform |
| Linux (aarch64) | Untested | Builds in CI, PRs welcome |
| macOS (Apple Silicon) | Untested | Builds in CI, PRs welcome |
| macOS (Intel) | Untested | Builds in CI, PRs welcome |
| Windows (x86_64) | Untested | Builds in CI, PRs welcome |

Pre-built binaries for all platforms are available from [GitHub Releases](https://github.com/meawoppl/agent-portal/releases/latest).

## Troubleshooting

See [TROUBLESHOOTING.md](TROUBLESHOOTING.md) for common issues and solutions.

## Google OAuth Setup

To deploy your own instance, you need Google OAuth credentials.

### 1. Create a Google Cloud Project

1. Go to [Google Cloud Console](https://console.cloud.google.com/)
2. Create a new project or select an existing one
3. Enable the **Google+ API** (for user info)

### 2. Configure OAuth Consent Screen

1. Navigate to **APIs & Services > OAuth consent screen**
2. Choose **External** (or **Internal** for Google Workspace orgs)
3. Fill in the required fields:
   - App name: Your portal name
   - User support email: Your email
   - Developer contact: Your email
4. Add scopes: `email`, `profile`, `openid`
5. Add test users if in testing mode

### 3. Create OAuth Credentials

1. Navigate to **APIs & Services > Credentials**
2. Click **Create Credentials > OAuth client ID**
3. Application type: **Web application**
4. Add authorized redirect URIs:
   - `https://your-domain.com/api/auth/google/callback`
   - `http://localhost:3000/api/auth/google/callback` (for development)
5. Save the **Client ID** and **Client Secret**

### 4. Configure Environment

Add to your `.env` file:

```bash
GOOGLE_CLIENT_ID=your-client-id.apps.googleusercontent.com
GOOGLE_CLIENT_SECRET=your-client-secret
GOOGLE_REDIRECT_URI=https://your-domain.com/api/auth/google/callback
```

### Access Control

By default, any Google account can sign in. To restrict access:

**Option 1: Environment variables (recommended)**

Set one or both of these environment variables:

```bash
# Allow only emails from a specific domain
ALLOWED_EMAIL_DOMAIN=yourcompany.com

# Allow specific email addresses (comma-separated)
ALLOWED_EMAILS=user1@gmail.com,user2@example.com
```

If both are set, users matching either condition are allowed. Users not matching will see an "Access Denied" page.

**Option 2: Admin panel**

Use the admin panel (`/admin`) to disable unwanted users after they sign in.

**Option 3: Google Workspace**

For Google Workspace organizations, configure the OAuth consent screen as "Internal" to restrict to your domain.
