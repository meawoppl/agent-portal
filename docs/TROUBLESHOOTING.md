# Troubleshooting Guide

Common issues and solutions for claude-code-portal development.

## diesel CLI Installation Issues

### Error: "diesel_cli 2.2.12 supports rustc 1.78.0"

**Problem:** Your Rust version is too old for the latest diesel CLI.

**Solution 1: Update Rust (Recommended)**
```bash
# Update Rust to latest stable
./scripts/update-rust.sh

# Or manually:
rustup update stable
rustup default stable

# Verify version
rustc --version  # Should be >= 1.78.0

# Now install diesel
cargo install diesel_cli --no-default-features --features postgres
```

**Solution 2: Install older diesel CLI**
```bash
cargo install diesel_cli --version 2.1.0 --no-default-features --features postgres
```

**Solution 3: Use pre-built binary (macOS only)**
```bash
brew install diesel
```

### Error: "linking with `cc` failed" or "cannot find -lpq"

**Problem:** PostgreSQL development libraries not installed.

**Solution:**
```bash
# Ubuntu/Debian
sudo apt-get update
sudo apt-get install libpq-dev build-essential

# macOS
brew install postgresql

# Fedora/RHEL
sudo dnf install libpq-devel

# Arch Linux
sudo pacman -S postgresql-libs
```

Then try installing diesel again:
```bash
cargo install diesel_cli --no-default-features --features postgres
```

## trunk Installation Issues

### Error: trunk installation fails

**Solution:**
```bash
# Use --locked flag
cargo install --locked trunk

# Or download pre-built binary
# Linux/macOS:
wget -qO- https://github.com/trunk-rs/trunk/releases/download/v0.18.3/trunk-x86_64-unknown-linux-gnu.tar.gz | tar -xzf-
sudo mv trunk /usr/local/bin/

# Or use your package manager
# macOS:
brew install trunk
```

## Database Issues

### Error: "connection to server on socket failed"

**Problem:** PostgreSQL not running or connection refused.

**Solution:**
```bash
# Check if container is running
docker ps | grep claude-code-portal

# Start database
docker-compose -f docker-compose.test.yml up -d db

# Check logs
docker-compose -f docker-compose.test.yml logs db

# Test connection
psql postgresql://claude_portal:dev_password_change_in_production@localhost:5432/claude_portal -c "SELECT 1"
```

### Error: "Port 5432 already in use"

**Problem:** Another PostgreSQL instance is using the port.

**Solution 1: Stop other PostgreSQL**
```bash
# Find what's using the port
sudo lsof -i :5432

# Stop system PostgreSQL (varies by OS)
sudo systemctl stop postgresql
# or
brew services stop postgresql
```

**Solution 2: Use different port**
Edit `docker-compose.test.yml`:
```yaml
ports:
  - "5433:5432"  # Use 5433 instead
```

Then update DATABASE_URL:
```bash
export DATABASE_URL="postgresql://claude_portal:dev_password_change_in_production@localhost:5433/claude_portal"
```

## Frontend Build Issues

### Error: "wasm-opt: command not found"

**Solution:**
```bash
# Install binaryen (contains wasm-opt)
# Ubuntu/Debian:
sudo apt-get install binaryen

# macOS:
brew install binaryen

# Or let trunk download it:
trunk build --release  # Will auto-download wasm-opt
```

### Error: "rustup target add wasm32-unknown-unknown"

**Solution:**
```bash
rustup target add wasm32-unknown-unknown
cd frontend && trunk build --release
```

## Backend Issues

### Error: "GOOGLE_CLIENT_ID must be set"

**Problem:** Running without dev mode and OAuth not configured.

**Solution 1: Use dev mode**
```bash
./scripts/test-dev.sh  # Uses dev mode automatically
```

**Solution 2: Configure OAuth**
```bash
cp .env.example .env
# Edit .env with your Google OAuth credentials
./scripts/test-oauth.sh
```

### Error: "Port 3000 already in use"

**Solution:**
```bash
# Find process
lsof -i :3000

# Kill it
kill -9 <PID>

# Or use a different port
export PORT=8080
cargo run -p backend -- --dev-mode
```

## Proxy Issues

### Error: "Failed to connect to backend"

**Solution:**
```bash
# Check if backend is running
curl http://localhost:3000/

# Check backend logs
tail -f /tmp/claude-code-portal-backend.log

# Verify WebSocket endpoint
wscat -c ws://localhost:3000/ws/session
```

### Error: "claude: command not found"

**Problem:** Claude CLI not in PATH.

**Solution:**
```bash
# Check if claude is installed
which claude

# If not installed, install it:
# (varies by platform - see Claude CLI docs)

# Or specify full path
cargo run -p proxy -- \
  --backend-url ws://localhost:3000 \
  /full/path/to/claude
```

## Docker Issues

### Error: "docker: command not found"

**Solution:**
```bash
# Install Docker: https://docs.docker.com/get-docker/

# Verify installation
docker --version
docker compose version
```

### Error: "permission denied while trying to connect to Docker daemon"

**Solution:**
```bash
# Add user to docker group (Linux)
sudo usermod -aG docker $USER
newgrp docker

# Or run with sudo (not recommended)
sudo docker-compose up
```

## Script Issues

### Error: "Permission denied" when running scripts

**Solution:**
```bash
chmod +x scripts/*.sh
./scripts/test-dev.sh
```

## General Tips

### Clean slate

If everything is broken, try a full cleanup:
```bash
./scripts/clean.sh
rm -rf ~/.config/claude-code-portal/  # Remove cached auth
cargo clean
cd frontend && trunk clean && cd ..
./scripts/install-deps.sh
./scripts/test-dev.sh
```

### Check system requirements

```bash
# Rust version
rustc --version  # Should be >= 1.78.0

# Cargo
cargo --version

# Docker
docker --version
docker compose version

# PostgreSQL libraries (Linux)
dpkg -l | grep libpq-dev

# PostgreSQL libraries (macOS)
brew list | grep postgresql
```

### Enable debug logging

```bash
# Backend
RUST_LOG=debug cargo run -p backend -- --dev-mode

# Proxy
RUST_LOG=debug cargo run -p proxy

# View all logs
tail -f /tmp/claude-code-portal-*.log
```

### Test database connection manually

```bash
export DATABASE_URL="postgresql://claude_portal:dev_password_change_in_production@localhost:5432/claude_portal"

# Using psql
psql $DATABASE_URL -c "SELECT version()"

# Using diesel
cd backend
diesel migration run
diesel migration revert
diesel migration run
```

## Getting Help

If you're still stuck:

1. Check the logs:
   ```bash
   tail -f /tmp/claude-code-portal-backend.log
   tail -f /tmp/claude-code-portal-proxy.log
   ```

2. Enable verbose output:
   ```bash
   RUST_LOG=debug ./scripts/test-dev.sh
   ```

3. Try running components individually:
   ```bash
   # Start just the database
   docker-compose -f docker-compose.test.yml up db

   # In separate terminals:
   cargo run -p backend -- --dev-mode
   cargo run -p proxy
   ```

4. Check system resources:
   ```bash
   df -h     # Disk space
   free -h   # Memory (Linux)
   top       # CPU usage
   ```

5. File an issue with:
   - Your OS and version
   - Rust version (`rustc --version`)
   - Full error message
   - Output of `./scripts/install-deps.sh`
