# Local Development Setup

This guide covers using the `dev.sh` script to quickly set up a local development environment.

## Quick Start

```bash
# Clone and start
git clone https://github.com/meawoppl/agent-portal.git
cd agent-portal
./scripts/dev.sh start
```

That's it! Open **http://localhost:3000/** and you'll be logged in as `testing@testing.local`.

## What `dev.sh start` Does

The script handles the complete setup automatically:

1. **Starts PostgreSQL** via Docker Compose
2. **Waits for database** to be ready
3. **Runs migrations** using Diesel CLI (auto-installs if missing)
4. **Builds frontend** using Trunk (auto-installs if missing)
5. **Starts backend** in dev mode (OAuth bypassed)

## Available Commands

| Command | Description |
|---------|-------------|
| `./scripts/dev.sh start` | Start all services |
| `./scripts/dev.sh stop` | Stop all services |
| `./scripts/dev.sh restart` | Restart all services |
| `./scripts/dev.sh status` | Show status of all services |
| `./scripts/dev.sh logs` | Tail backend logs |
| `./scripts/dev.sh build` | Rebuild frontend only |

## Service Details

### PostgreSQL

- **Container**: `claude-portal-test-db`
- **Port**: 5432
- **Database**: `claude_portal`
- **User**: `claude_portal`
- **Password**: `dev_password_change_in_production`

Access the database directly:
```bash
./scripts/db-shell.sh
```

### Backend

- **URL**: http://localhost:3000
- **Log file**: `/tmp/agent-portal-backend.log`
- **Mode**: Dev mode (OAuth bypassed)
- **Test user**: `testing@testing.local`

### Frontend

- **Built to**: `frontend/dist/`
- **Served by**: Backend at http://localhost:3000

## Auto-Installed Dependencies

The script will automatically install these if missing:

| Tool | Purpose |
|------|---------|
| `diesel_cli` | Database migrations |
| `trunk` | WASM build tool |
| `wasm32-unknown-unknown` | Rust WASM target |

## Running Individual Components

For more control, you can run components separately:

### Terminal 1: Database
```bash
docker-compose -f docker-compose.test.yml up -d db
```

### Terminal 2: Backend
```bash
export DATABASE_URL="postgresql://claude_portal:dev_password_change_in_production@localhost:5432/claude_portal"
cargo run -p backend -- --dev-mode
```

### Terminal 3: Frontend (with hot reload)
```bash
cd frontend
trunk serve
# Opens at http://localhost:8080 with auto-reload
```

### Terminal 4: Proxy (optional)
```bash
cargo run -p claude-portal -- --backend-url ws://localhost:3000
```

## Hot Reload Development

For rapid iteration with auto-reload:

```bash
# Backend with auto-reload (requires cargo-watch)
cargo install cargo-watch
cargo watch -x 'run -p backend -- --dev-mode'

# Frontend with hot reload
cd frontend
trunk serve
```

## Database Management

```bash
# Run migrations
cd backend && diesel migration run

# Create new migration
diesel migration generate add_new_feature

# Revert last migration
diesel migration revert

# Reset database (destroys all data)
./scripts/dev.sh stop
docker-compose -f docker-compose.test.yml down -v
./scripts/dev.sh start
```

## Troubleshooting

### Port 3000 already in use
```bash
# Find what's using it
lsof -i :3000

# Kill the process or use a different port
cargo run -p backend -- --dev-mode --port 3001
```

### Database connection failed
```bash
# Check if PostgreSQL is running
docker ps | grep claude-portal-test-db

# Restart the database
./scripts/dev.sh stop
./scripts/dev.sh start
```

### Frontend build failed
```bash
# Ensure WASM target is installed
rustup target add wasm32-unknown-unknown

# Clean and rebuild
cd frontend
trunk clean
trunk build
```

### "diesel CLI not found"
```bash
# Update Rust and reinstall
rustup update stable
cargo install diesel_cli --no-default-features --features postgres
```

### Migrations failed
```bash
# Check migration status
cd backend
diesel migration list

# If stuck, reset the database
cd ..
./scripts/dev.sh stop
docker-compose -f docker-compose.test.yml down -v
./scripts/dev.sh start
```

## Environment Variables

The dev script sets these automatically, but you can override them:

| Variable | Default | Description |
|----------|---------|-------------|
| `DATABASE_URL` | PostgreSQL connection string | Database connection |
| `HOST` | `0.0.0.0` | Backend bind address |
| `PORT` | `3000` | Backend port |

## Next Steps

- [Development Guide](DEVELOPING.md) - Full development workflow, testing, contributing
- [Docker Guide](DOCKER.md) - Docker-based deployment
- [Deployment Guide](DEPLOYING.md) - Production deployment
