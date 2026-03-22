#!/bin/bash
# Full OAuth testing script
# Requires Google OAuth credentials in .env

set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log() { echo -e "${BLUE}[claude-portal]${NC} $1"; }
success() { echo -e "${GREEN}✓${NC} $1"; }
error() { echo -e "${RED}✗${NC} $1"; }
warn() { echo -e "${YELLOW}⚠${NC} $1"; }

cleanup() {
    log "Cleaning up..."
    docker-compose -f docker-compose.test.yml down -v 2>/dev/null || true
    pkill -f "cargo run -p backend" 2>/dev/null || true
    pkill -f "cargo run -p proxy" 2>/dev/null || true
}

trap cleanup EXIT INT TERM

# Check for .env file
if [ ! -f .env ]; then
    error ".env file not found"
    echo ""
    echo "Please create .env file with Google OAuth credentials:"
    echo "  cp .env.example .env"
    echo "  # Edit .env with your credentials"
    echo ""
    exit 1
fi

# Source .env
set -a
source .env
set +a

# Validate OAuth credentials
if [ -z "$GOOGLE_CLIENT_ID" ] || [ "$GOOGLE_CLIENT_ID" = "your_client_id.apps.googleusercontent.com" ]; then
    error "GOOGLE_CLIENT_ID not set in .env"
    echo "Get credentials from: https://console.cloud.google.com/apis/credentials"
    exit 1
fi

log "🧹 Cleaning up..."
cleanup
sleep 2

log "🗄️  Starting PostgreSQL..."
docker-compose -f docker-compose.test.yml up -d db

log "⏳ Waiting for database..."
for i in {1..30}; do
    if docker-compose -f docker-compose.test.yml exec -T db pg_isready -U claude_portal > /dev/null 2>&1; then
        success "Database is ready"
        break
    fi
    [ $i -eq 30 ] && { error "Database timeout"; exit 1; }
    sleep 1
done

export DATABASE_URL="postgresql://claude_portal:dev_password_change_in_production@localhost:5432/claude_portal"

log "🔄 Running migrations..."

# Check if diesel CLI is installed
if ! command -v diesel &> /dev/null; then
    warn "diesel CLI not installed. Installing now..."
    echo "This may take a few minutes..."
    if cargo install diesel_cli --no-default-features --features postgres; then
        success "diesel CLI installed"
    else
        error "Failed to install diesel CLI. Install manually:"
        echo "  cargo install diesel_cli --no-default-features --features postgres"
        exit 1
    fi
fi

cd backend && diesel migration run && cd ..
success "Migrations complete"

log "🎨 Building frontend..."
cd frontend && trunk build --release && cd ..
success "Frontend built"

log "🚀 Starting backend with OAuth..."
cargo run -p backend > /tmp/claude-portal-backend.log 2>&1 &
BACKEND_PID=$!

log "⏳ Waiting for backend..."
for i in {1..30}; do
    if curl -sf http://localhost:3000/ > /dev/null 2>&1; then
        success "Backend is ready"
        break
    fi
    [ $i -eq 30 ] && { error "Backend timeout"; exit 1; }
    sleep 1
done

log "🔌 Starting proxy (will prompt for OAuth)..."
echo ""

cargo run -p proxy -- \
    --backend-url ws://localhost:3000 \
    --session-name "oauth-test-session" &
PROXY_PID=$!

echo ""
echo "╔══════════════════════════════════════════════════════╗"
echo "║     ✅ Agent Portal Test Environment (OAuth Mode)     ║"
echo "╚══════════════════════════════════════════════════════╝"
echo ""
echo "  🌐 Web Interface:  http://localhost:3000/app/"
echo "  📊 Backend API:    http://localhost:3000/"
echo ""
echo "  📝 Logs:"
echo "     Backend: tail -f /tmp/claude-portal-backend.log"
echo ""
echo "  🔐 OAuth Flow:"
echo "     1. Follow the link displayed by the proxy"
echo "     2. Enter the verification code"
echo "     3. Sign in with Google"
echo ""
echo "Press Ctrl+C to stop all services"
echo ""

wait $BACKEND_PID $PROXY_PID
