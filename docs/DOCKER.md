# Docker Deployment Guide

This guide covers running Agent Portal in Docker. For a complete production
stack with HTTPS and backups, use [Self-Hosting](SELF_HOSTING.md).

## Quick Start

```bash
# Configure OAuth and SESSION_SECRET in .env first.
# Pull and start the released backend plus the local development database.
docker compose pull
docker compose up -d

# View logs
docker compose logs -f backend

# Stop
docker compose down
```

## Building the Image

The root `Dockerfile` packages a prebuilt Linux backend; it does not compile the
workspace. CI builds the frontend first and embeds it into that binary. To
package a compatible CI backend artifact locally:

```bash
mkdir -p build-output
cp /path/to/compatible-linux-backend build-output/backend
docker build -f Dockerfile -t agent-portal-backend .
```

The artifact must match the image architecture and Ubuntu 24.04's runtime ABI.
The Docker context intentionally includes only this binary and the Dockerfile.
For ordinary use, pull `ghcr.io/meawoppl/agent-portal:<version>` instead of
building an image; pin a release version for production.

## Running with Environment Variables

Create a `.env` file with your configuration:

```bash
# .env

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

# Optional - Customize app title shown in browser (default: "Agent Portal")
# APP_TITLE=Agent Portal

# Optional - Path to proxy binary for downloads (auto-detected if not set)
# PROXY_BINARY_PATH=/app/claude-portal

# Optional - Session cleanup
# SESSION_MAX_AGE_DAYS=14        # Delete sessions older than N days (default: 14, 0=disabled)

# Optional - Image size limit for proxies
# PORTAL_MAX_IMAGE_MB=10         # Max image size in MB to inline (default: 10)

# Optional - Access control (restrict who can sign in)
# ALLOWED_EMAIL_DOMAIN=yourcompany.com
# ALLOWED_EMAILS=user1@gmail.com,user2@example.com

# Optional - Message retention (data cleanup)
# MESSAGE_RETENTION_COUNT=1000   # Max messages per session (default: 1000)
# MESSAGE_RETENTION_DAYS=30      # Delete messages older than N days (default: 30, 0=disabled)
```

Run the container with the env file:

```bash
docker run -d \
  --name agent-portal-backend \
  -p 3000:3000 \
  --env-file .env \
  agent-portal-backend
```

Or pass variables directly:

```bash
docker run -d \
  --name agent-portal-backend \
  -p 3000:3000 \
  -e DATABASE_URL="postgresql://..." \
  -e GOOGLE_CLIENT_ID="..." \
  -e GOOGLE_CLIENT_SECRET="..." \
  -e GOOGLE_REDIRECT_URI="https://your-domain.com/api/auth/google/callback" \
  -e SESSION_SECRET="$(openssl rand -base64 32)" \
  agent-portal-backend
```

## Docker Compose

Create a `docker-compose.prod.yml`:

```yaml
services:
  backend:
    image: ghcr.io/meawoppl/agent-portal:${PORTAL_VERSION:?set a release version}
    container_name: agent-portal-backend
    ports:
      - "3000:3000"
    env_file:
      - .env
    restart: always
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:3000/"]
      interval: 30s
      timeout: 10s
      retries: 3
      start_period: 10s
```

Run with:

```bash
docker compose -f docker-compose.prod.yml up -d
```

## Production Setup with Traefik

Example with automatic TLS via Let's Encrypt:

```yaml
# docker-compose.prod.yml
services:
  traefik:
    image: traefik:v2.10
    command:
      - "--providers.docker=true"
      - "--entrypoints.web.address=:80"
      - "--entrypoints.websecure.address=:443"
      - "--certificatesresolvers.letsencrypt.acme.email=admin@example.com"
      - "--certificatesresolvers.letsencrypt.acme.storage=/letsencrypt/acme.json"
      - "--certificatesresolvers.letsencrypt.acme.httpchallenge.entrypoint=web"
    ports:
      - "80:80"
      - "443:443"
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock:ro
      - ./letsencrypt:/letsencrypt

  backend:
    image: ghcr.io/meawoppl/agent-portal:${PORTAL_VERSION:?set a release version}
    env_file:
      - .env
    labels:
      - "traefik.enable=true"
      - "traefik.http.routers.backend.rule=Host(`portal.yoursite.com`)"
      - "traefik.http.routers.backend.entrypoints=websecure"
      - "traefik.http.routers.backend.tls.certresolver=letsencrypt"
    restart: always
```

## Multi-Architecture Builds

Use the published multi-platform image for normal deployments. CI builds a
separate backend for each platform before packaging the corresponding image.
A custom multi-platform build likewise needs a matching prebuilt backend for
each target; passing two platforms to Docker while copying one native binary
does not produce two working architectures.

## Environment Variables Reference

### Required

| Variable | Description |
|----------|-------------|
| `DATABASE_URL` | PostgreSQL connection string |
| `GOOGLE_CLIENT_ID` | Google OAuth client ID |
| `GOOGLE_CLIENT_SECRET` | Google OAuth client secret |
| `GOOGLE_REDIRECT_URI` | OAuth callback URL (e.g., `https://your-domain.com/api/auth/google/callback`) |
| `SESSION_SECRET` or `SESSION_SECRET_FILE` | Session secret (64+ bytes recommended). See [Session secret](#session-secret). |

### Optional

| Variable | Default | Description |
|----------|---------|-------------|
| `HOST` | `0.0.0.0` | Bind address |
| `PORT` | `3000` | Bind port |
| `BASE_URL` | Auto-detected | Public URL for OAuth callbacks |
| `APP_TITLE` | `Agent Portal` | Title shown in browser tab |
| `PROXY_BINARY_PATH` | Auto-detected | Path to `claude-portal` binary for downloads |
| `ALLOWED_EMAIL_DOMAIN` | *(none)* | Restrict sign-in to emails from this domain |
| `ALLOWED_EMAILS` | *(none)* | Comma-separated list of allowed email addresses |
| `MESSAGE_RETENTION_COUNT` | `1000` | Maximum messages to keep per session |
| `MESSAGE_RETENTION_DAYS` | `30` | Delete messages older than N days (0 = disabled) |
| `SESSION_MAX_AGE_DAYS` | `14` | Delete sessions older than N days (0 = disabled) |
| `PORTAL_MAX_IMAGE_MB` | `10` | Max image size in MB for proxy inlining |
| `PORTAL_REMINDER_FILE` | _(unset)_ | Proxy/launcher-side: path to a markdown file that overrides the bundled portal-features reminder, which is prepended to the first user input at session start and after each compaction. Falls back to default if missing/unreadable. |

### Session secret

The session secret signs browser session cookies **and** the JWTs that proxies
and launchers authenticate with. Provide it one of two ways:

- `SESSION_SECRET` — the value itself.
- `SESSION_SECRET_FILE` — a path to a file holding the value, typically a
  read-only bind mount such as `/run/secrets/session.key`. Exactly one trailing
  `\n` (or `\r\n`) is stripped; the rest is used byte-for-byte as
  `SESSION_SECRET` would be, so moving an existing value from the env var into a
  file keeps every cookie and launcher token valid.

Setting both is a startup error. With `SESSION_SECRET_FILE`, a missing,
unreadable, empty or non-UTF-8 file also fails startup. The file must be a
regular file owned by root or the container user (UID 1001) and must not be
group- or world-writable — root-owned `0444` and UID-1001-owned `0400`/`0600`
both work:

```bash
openssl rand -hex 64 | sudo install -m 0400 -o 1001 -g 1001 /dev/stdin /etc/agent-portal/session.key
docker run ... \
  -v /etc/agent-portal/session.key:/run/secrets/session.key:ro \
  -e SESSION_SECRET_FILE=/run/secrets/session.key \
  agent-portal-backend
```

The value must be stable across restarts. If neither variable is set, the
backend generates an ephemeral secret and logs a warning, so every restart logs
everyone out. The startup log records which source was used (`env`, `file` or
`ephemeral`), never the value.

**Rotation blast radius:** changing the secret logs out every browser **and**
forces every launcher and proxy to re-authenticate (`agent-portal login`).

## Troubleshooting

### Container exits immediately

Check logs:
```bash
docker logs agent-portal-backend
```

Common causes:
- Missing required environment variables
- Database connection failed
- Invalid OAuth credentials

### Database connection failed

Test connectivity:
```bash
docker run --rm --env-file .env agent-portal-backend \
  bash -c 'psql $DATABASE_URL -c "SELECT 1"'
```

### Health check failing

Test the endpoint:
```bash
docker exec agent-portal-backend curl -f http://localhost:3000/
```

## Production Checklist

- [ ] Use a production PostgreSQL database (not the dev docker compose db)
- [ ] Set up database backups
- [ ] Configure TLS/SSL (via Traefik, nginx, or cloud load balancer)
- [ ] Set a strong, stable `SESSION_SECRET` (or `SESSION_SECRET_FILE`)
- [ ] Configure access restrictions (`ALLOWED_EMAIL_DOMAIN` or `ALLOWED_EMAILS`)
- [ ] Set up monitoring and log aggregation
- [ ] Configure resource limits
- [ ] Enable auto-restart (`restart: always`)
