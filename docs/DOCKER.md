# Docker Deployment Guide

This guide covers running Claude Code Portal in Docker.

## Quick Start

```bash
# Build and start
docker-compose up -d

# View logs
docker-compose logs -f backend

# Stop
docker-compose down
```

## Building the Image

```bash
docker build -f backend/Dockerfile -t agent-portal-backend .
```

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

# Optional - Google Cloud Speech-to-Text (for server-side voice transcription)
# GOOGLE_APPLICATION_CREDENTIALS=/path/to/service-account.json

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
# MESSAGE_RETENTION_COUNT=100    # Max messages per session (default: 100)
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
    build:
      context: .
      dockerfile: backend/Dockerfile
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
docker-compose -f docker-compose.prod.yml up -d
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
    build:
      context: .
      dockerfile: backend/Dockerfile
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

Build for both AMD64 and ARM64:

```bash
docker buildx create --use

docker buildx build \
  --platform linux/amd64,linux/arm64 \
  -f backend/Dockerfile \
  -t your-registry/agent-portal-backend:latest \
  --push \
  .
```

## Environment Variables Reference

### Required

| Variable | Description |
|----------|-------------|
| `DATABASE_URL` | PostgreSQL connection string |
| `GOOGLE_CLIENT_ID` | Google OAuth client ID |
| `GOOGLE_CLIENT_SECRET` | Google OAuth client secret |
| `GOOGLE_REDIRECT_URI` | OAuth callback URL (e.g., `https://your-domain.com/api/auth/google/callback`) |
| `SESSION_SECRET` | Session encryption key (32+ chars recommended) |

### Optional

| Variable | Default | Description |
|----------|---------|-------------|
| `HOST` | `0.0.0.0` | Bind address |
| `PORT` | `3000` | Bind port |
| `BASE_URL` | Auto-detected | Public URL for OAuth callbacks |
| `APP_TITLE` | `Agent Portal` | Title shown in browser tab |
| `GOOGLE_APPLICATION_CREDENTIALS` | *(none)* | Path to GCP service account JSON for Speech-to-Text |
| `PROXY_BINARY_PATH` | Auto-detected | Path to `claude-portal` binary for downloads |
| `ALLOWED_EMAIL_DOMAIN` | *(none)* | Restrict sign-in to emails from this domain |
| `ALLOWED_EMAILS` | *(none)* | Comma-separated list of allowed email addresses |
| `MESSAGE_RETENTION_COUNT` | `100` | Maximum messages to keep per session |
| `MESSAGE_RETENTION_DAYS` | `30` | Delete messages older than N days (0 = disabled) |
| `SESSION_MAX_AGE_DAYS` | `14` | Delete sessions older than N days (0 = disabled) |
| `PORTAL_MAX_IMAGE_MB` | `10` | Max image size in MB for proxy inlining |

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

- [ ] Use a production PostgreSQL database (not the dev docker-compose db)
- [ ] Set up database backups
- [ ] Configure TLS/SSL (via Traefik, nginx, or cloud load balancer)
- [ ] Set strong `SESSION_SECRET`
- [ ] Configure access restrictions (`ALLOWED_EMAIL_DOMAIN` or `ALLOWED_EMAILS`)
- [ ] Set up monitoring and log aggregation
- [ ] Configure resource limits
- [ ] Enable auto-restart (`restart: always`)
