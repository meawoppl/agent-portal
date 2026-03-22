#!/bin/bash
# Development testing script - runs everything locally in dev mode
# No OAuth required

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

log() {
    echo -e "${BLUE}[claude-portal]${NC} $1"
}

success() {
    echo -e "${GREEN}✓${NC} $1"
}

error() {
    echo -e "${RED}✗${NC} $1"
}

warn() {
    echo -e "${YELLOW}⚠${NC} $1"
}

# Cleanup function
cleanup() {
    log "Cleaning up..."
    docker compose -f docker-compose.test.yml down -v 2>/dev/null || true
    pkill -f "cargo run -p backend" 2>/dev/null || true
    pkill -f "cargo run -p proxy" 2>/dev/null || true
    pkill -f "trunk serve" 2>/dev/null || true
}

# Trap Ctrl+C and cleanup
trap cleanup EXIT INT TERM

log "🧹 Cleaning up any existing instances..."
cleanup
sleep 2

log "🗄️  Starting PostgreSQL..."
docker compose -f docker-compose.test.yml up -d db

log "⏳ Waiting for database to be ready..."
for i in {1..30}; do
    if docker compose -f docker-compose.test.yml exec -T db pg_isready -U claude_portal > /dev/null 2>&1; then
        success "Database is ready"
        break
    fi
    if [ $i -eq 30 ]; then
        error "Database failed to start"
        exit 1
    fi
    sleep 1
done

# Set database URL
export DATABASE_URL="postgresql://claude_portal:dev_password_change_in_production@localhost:5432/claude_portal"

log "🔄 Running database migrations..."

# Check if diesel CLI is installed
if ! command -v diesel &> /dev/null; then
    warn "diesel CLI not installed. Checking Rust version..."

    # Check Rust version
    RUST_VERSION=$(rustc --version | awk '{print $2}')
    REQUIRED_VERSION="1.78.0"

    echo "Current Rust version: $RUST_VERSION"
    echo "Required version: >= $REQUIRED_VERSION"
    echo ""

    # Try to update Rust first
    warn "Updating Rust to latest version..."
    if rustup update stable; then
        success "Rust updated to $(rustc --version | awk '{print $2}')"
    else
        warn "Failed to update Rust"
    fi

    echo ""
    log "Installing diesel CLI..."
    echo "This may take 5-10 minutes on first install..."
    echo ""

    if cargo install diesel_cli --no-default-features --features postgres; then
        success "diesel CLI installed"
    else
        error "Failed to install diesel CLI"
        echo ""
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo "Troubleshooting:"
        echo ""
        echo "1. Update Rust to the latest version:"
        echo "   rustup update stable"
        echo "   rustup default stable"
        echo ""
        echo "2. Try installing diesel CLI manually:"
        echo "   cargo install diesel_cli --no-default-features --features postgres"
        echo ""
        echo "3. Or use a pre-built binary:"
        echo "   # On macOS with Homebrew:"
        echo "   brew install diesel"
        echo ""
        echo "4. Skip migrations and run manually later:"
        echo "   export SKIP_MIGRATIONS=1"
        echo "   ./scripts/test-dev.sh"
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo ""
        exit 1
    fi
fi

cd backend

# Check if we should skip migrations
if [ -n "$SKIP_MIGRATIONS" ]; then
    warn "Skipping migrations (SKIP_MIGRATIONS is set)"
else
    if ! diesel migration run; then
        error "Failed to run migrations"
        echo ""
        echo "Try running migrations manually:"
        echo "  cd backend"
        echo "  export DATABASE_URL='postgresql://claude_portal:dev_password_change_in_production@localhost:5432/claude_portal'"
        echo "  diesel migration run"
        echo ""
        exit 1
    fi
    success "Migrations complete"
fi

cd ..

log "🎨 Building frontend..."

# Check if trunk is installed
if ! command -v trunk &> /dev/null; then
    warn "trunk not installed. Installing now..."
    echo "This may take a few minutes..."
    echo ""

    if cargo install --locked trunk; then
        success "trunk installed"
    else
        error "Failed to install trunk"
        echo ""
        echo "Install manually with:"
        echo "  cargo install --locked trunk"
        echo ""
        echo "Or download from: https://trunkrs.dev/#install"
        exit 1
    fi
fi

cd frontend
if ! trunk build; then
    error "Frontend build failed"
    echo ""
    echo "Try building manually:"
    echo "  cd frontend"
    echo "  trunk build"
    echo ""
    exit 1
fi
cd ..
success "Frontend built"

log "🚀 Starting backend in dev mode..."
export DEV_MODE=true
cargo run -p backend -- --dev-mode > /tmp/claude-portal-backend.log 2>&1 &
BACKEND_PID=$!

log "⏳ Waiting for backend to start..."
for i in {1..30}; do
    if curl -sf http://localhost:3000/ > /dev/null 2>&1; then
        success "Backend is ready"
        break
    fi
    if [ $i -eq 30 ]; then
        error "Backend failed to start. Check logs:"
        echo "  tail -f /tmp/claude-portal-backend.log"
        exit 1
    fi
    sleep 1
done

echo ""
echo "╔══════════════════════════════════════════════════════╗"
echo "║       ✅ Agent Portal Test Environment Running        ║"
echo "╚══════════════════════════════════════════════════════╝"
echo ""
echo "  🌐 Web Interface:  http://localhost:3000/"
echo "  📊 Backend API:    http://localhost:3000/api/"
echo "  🗄️  Database:       postgresql://claude_portal:***@localhost:5432/claude_portal"
echo ""
echo "  📝 Logs:"
echo "     Backend: tail -f /tmp/claude-portal-backend.log"
echo ""
echo "  🧪 Test Account:"
echo "     Email: testing@testing.local"
echo ""
echo "  🔌 To start a proxy session:"
echo "     1. Open http://localhost:3000/ and generate a setup token"
echo "     2. Run the setup command shown in the UI"
echo ""
echo "  ⚠️  DEV MODE ENABLED - OAuth bypassed"
echo ""
echo "Press Ctrl+C to stop all services"
echo ""

# Wait for user interrupt
wait $BACKEND_PID
