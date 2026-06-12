#!/bin/bash
# Install all required dependencies for claude-portal development

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log() { echo -e "${BLUE}[claude-portal]${NC} $1"; }
success() { echo -e "${GREEN}✓${NC} $1"; }
error() { echo -e "${RED}✗${NC} $1"; }
warn() { echo -e "${YELLOW}⚠${NC} $1"; }

log "Installing claude-portal dependencies..."
echo ""

# Check for Rust
if ! command -v cargo &> /dev/null; then
    error "Rust not found. Install from: https://rustup.rs"
    exit 1
fi

RUST_VERSION=$(rustc --version | awk '{print $2}')
log "Current Rust version: $RUST_VERSION"

# Check if Rust is recent enough for diesel
REQUIRED_VERSION="1.78.0"
log "Checking Rust version (diesel requires >= $REQUIRED_VERSION)..."

# Try to update Rust
log "Updating Rust to latest stable..."
if rustup update stable 2>&1 | grep -q "updated"; then
    success "Rust updated to $(rustc --version | awk '{print $2}')"
else
    warn "Rust is already up to date or rustup not available"
fi

rustup default stable 2>/dev/null || true
success "Using Rust: $(rustc --version)"

# Install diesel CLI
log "Checking diesel CLI..."
if command -v diesel &> /dev/null; then
    success "diesel CLI already installed: $(diesel --version)"
else
    warn "diesel CLI not found. Installing..."
    log "This may take 5-10 minutes on first install..."
    echo ""

    # Try installation
    if cargo install diesel_cli --no-default-features --features postgres 2>&1; then
        success "diesel CLI installed"
    else
        error "Failed to install diesel CLI"
        echo ""
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo "Troubleshooting diesel CLI installation:"
        echo ""
        echo "1. Make sure PostgreSQL dev libraries are installed:"
        echo ""
        echo "   # Ubuntu/Debian:"
        echo "   sudo apt-get install libpq-dev"
        echo ""
        echo "   # macOS:"
        echo "   brew install postgresql"
        echo ""
        echo "   # Fedora:"
        echo "   sudo dnf install libpq-devel"
        echo ""
        echo "2. Update Rust if version is too old:"
        echo "   rustup update stable"
        echo "   rustup default stable"
        echo ""
        echo "3. Try manual installation:"
        echo "   cargo install diesel_cli --no-default-features --features postgres"
        echo ""
        echo "4. Alternative: Use pre-built binary (macOS):"
        echo "   brew install diesel"
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo ""
        exit 1
    fi
fi

# Install trunk
log "Checking trunk..."
if command -v trunk &> /dev/null; then
    success "trunk already installed: $(trunk --version)"
else
    warn "trunk not found. Installing..."
    if cargo install --locked trunk; then
        success "trunk installed"
    else
        error "Failed to install trunk"
        echo ""
        echo "Try installing manually:"
        echo "  cargo install --locked trunk"
        echo ""
        echo "Or download from: https://trunkrs.dev/#install"
        exit 1
    fi
fi

# Check Docker
log "Checking Docker..."
if command -v docker &> /dev/null; then
    success "Docker installed: $(docker --version)"
else
    warn "Docker not found. Install from: https://docs.docker.com/get-docker/"
fi

# Check Docker Compose
log "Checking Docker Compose..."
if docker compose version &> /dev/null 2>&1; then
    success "Docker Compose installed: $(docker compose version)"
else
    warn "Docker Compose not found (or using old version)"
    echo "Install Docker Compose v2: https://docs.docker.com/compose/install/"
fi

echo ""
echo "╔══════════════════════════════════════════════════════╗"
echo "║              ✅ Dependencies Check Complete          ║"
echo "╚══════════════════════════════════════════════════════╝"
echo ""
echo "Ready to run:"
echo "  ./scripts/dev.sh start"
echo ""
