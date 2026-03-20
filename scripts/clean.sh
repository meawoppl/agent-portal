#!/bin/bash
# Clean up all test artifacts and processes

# Colors
GREEN='\033[0;32m'
BLUE='\033[0;34m'
NC='\033[0m'

log() { echo -e "${BLUE}[claude-portal]${NC} $1"; }
success() { echo -e "${GREEN}✓${NC} $1"; }

log "Stopping all processes..."
pkill -f "cargo run -p backend" 2>/dev/null || true
pkill -f "cargo run -p proxy" 2>/dev/null || true
pkill -f "trunk serve" 2>/dev/null || true
success "Processes stopped"

log "Stopping Docker containers..."
docker-compose -f docker-compose.test.yml down -v 2>/dev/null || true
docker-compose down -v 2>/dev/null || true
success "Containers stopped"

log "Cleaning build artifacts..."
cargo clean 2>/dev/null || true
cd frontend && trunk clean 2>/dev/null && cd .. || true
success "Build artifacts cleaned"

log "Removing log files..."
rm -f /tmp/claude-portal-*.log
success "Logs removed"

log "Removing test config (optional)..."
read -p "Remove ~/.config/agent-portal/config.json? (y/N) " -n 1 -r
echo
if [[ $REPLY =~ ^[Yy]$ ]]; then
    rm -rf ~/.config/agent-portal/
    success "Config removed"
fi

echo ""
echo "Cleanup complete!"
