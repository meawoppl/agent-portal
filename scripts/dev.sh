#!/bin/bash
# Development environment management script
# Usage: ./scripts/dev.sh [start|stop|status|logs|restart]

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Network config. Overridable so parallel worktrees don't collide on one port.
# HOST defaults to dual-stack (::) so IPv6 `localhost` and port-forwards reach
# the backend — a plain 0.0.0.0 (IPv4-only) bind is missed by forwarders that
# resolve localhost to ::1.
PORT_EXPLICIT=1
if [ -z "${PORT+x}" ]; then
    PORT_EXPLICIT=0
fi
PORT="${PORT:-3000}"
HOST="${HOST:-::}"

# PID file + log locations, keyed by PORT so several instances (one per
# worktree/branch, on different ports) can run at once without clobbering each
# other's status/stop tracking. `PORT=3400 ./dev.sh start` runs a second one;
# `PORT=3400 ./dev.sh stop|status|logs` targets exactly that instance.
PID_DIR="/tmp/claude-portal-dev"
BACKEND_PID_FILE="$PID_DIR/backend-$PORT.pid"
BACKEND_LOG="/tmp/claude-portal-backend-$PORT.log"

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

port_in_use() {
    local port="$1"
    if command -v lsof >/dev/null 2>&1; then
        lsof -nP -iTCP:"$port" -sTCP:LISTEN >/dev/null 2>&1
    elif command -v ss >/dev/null 2>&1; then
        ss -ltn "( sport = :$port )" | tail -n +2 | grep -q .
    else
        return 1
    fi
}

describe_port_owner() {
    local port="$1"
    if command -v lsof >/dev/null 2>&1; then
        lsof -nP -iTCP:"$port" -sTCP:LISTEN || true
    elif command -v ss >/dev/null 2>&1; then
        ss -ltnp "( sport = :$port )" || true
    else
        echo "No lsof/ss available to identify the listener."
    fi
}

choose_available_port() {
    if ! port_in_use "$PORT"; then
        return 0
    fi

    if [ "$PORT_EXPLICIT" -eq 1 ]; then
        error "PORT=$PORT is already in use."
        describe_port_owner "$PORT"
        return 1
    fi

    local candidate
    for candidate in $(seq 3001 3099); do
        if ! port_in_use "$candidate"; then
            warn "Port $PORT is already in use; using $candidate instead. Set PORT explicitly to require a specific port."
            PORT="$candidate"
            BACKEND_PID_FILE="$PID_DIR/backend-$PORT.pid"
            BACKEND_LOG="/tmp/claude-portal-backend-$PORT.log"
            return 0
        fi
    done

    error "No free dev backend port found in 3000-3099."
    describe_port_owner 3000
    return 1
}

# Ensure PID directory exists
mkdir -p "$PID_DIR"

# Get the script's directory and project root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

source "$SCRIPT_DIR/lib.sh"

cd "$PROJECT_ROOT"

# Check if a process is running
is_running() {
    local pid_file="$1"
    if [ -f "$pid_file" ]; then
        local pid=$(cat "$pid_file")
        if kill -0 "$pid" 2>/dev/null; then
            return 0
        fi
    fi
    return 1
}

# Check if database container is running and accepting connections
is_db_running() {
    # First check if any db container is running
    if ! docker ps --format '{{.Names}}' 2>/dev/null | grep -q "db"; then
        return 1
    fi
    # Then check if it's actually accepting connections
    docker compose -f docker-compose.test.yml exec -T db pg_isready -U claude_portal > /dev/null 2>&1
}

# Start the database
start_db() {
    if is_db_running; then
        success "Database already running"
        return 0
    fi

    log "Starting PostgreSQL..."
    docker compose -f docker-compose.test.yml up -d db

    log "Waiting for database to be ready..."
    for i in {1..30}; do
        if docker compose -f docker-compose.test.yml exec -T db pg_isready -U claude_portal > /dev/null 2>&1; then
            success "Database is ready"
            return 0
        fi
        sleep 1
    done

    error "Database failed to start"
    return 1
}

# Run migrations
run_migrations() {
    export DATABASE_URL="$DEV_DATABASE_URL"

    if ! command -v diesel &> /dev/null; then
        warn "diesel CLI not found — skipping explicit migrations (the backend runs pending migrations itself at startup)."
        return 0
    fi

    log "Running database migrations..."
    cd backend
    if diesel migration run; then
        success "Migrations complete"
    else
        error "Migrations failed"
        cd ..
        return 1
    fi
    cd ..
}

# Build frontend
build_frontend() {
    if ! command -v trunk &> /dev/null; then
        error "trunk not installed. Run: ./scripts/install-deps.sh"
        return 1
    fi

    log "Building frontend..."
    cd frontend
    if trunk build; then
        success "Frontend built"
    else
        error "Frontend build failed"
        cd ..
        return 1
    fi
    cd ..
}

# Start the backend
start_backend() {
    if is_running "$BACKEND_PID_FILE"; then
        success "Backend already running (PID: $(cat $BACKEND_PID_FILE))"
        return 0
    fi

    export DATABASE_URL="$DEV_DATABASE_URL"
    export DEV_MODE=true
    export HOST PORT

    log "Starting backend in dev mode on ${HOST}:${PORT}..."
    # Run in its own session/process group (setsid) so `stop` can kill exactly
    # this backend's group without touching other worktrees' backends.
    setsid cargo run -p backend -- --dev-mode > "$BACKEND_LOG" 2>&1 &
    local pid=$!
    echo $pid > "$BACKEND_PID_FILE"

    log "Waiting for backend to start..."
    for i in {1..30}; do
        if curl -sf "http://localhost:$PORT/api/health" > /dev/null 2>&1; then
            success "Backend is ready (PID: $pid)"
            return 0
        fi
        if ! kill -0 "$pid" 2>/dev/null; then
            error "Backend process died. Check logs: tail -f $BACKEND_LOG"
            rm -f "$BACKEND_PID_FILE"
            return 1
        fi
        sleep 1
    done

    error "Backend failed to start. Check logs: tail -f $BACKEND_LOG"
    return 1
}

# Stop everything
stop_all() {
    log "Stopping services..."

    # Stop backend. The backend was started with setsid, so its PID is also its
    # process-group id; kill the whole group (cargo + the backend child) with a
    # negative PID. We deliberately DO NOT `pkill -f target/debug/backend` — that
    # matches every worktree's backend and would reap other developers'/agents'
    # instances running on this host.
    if [ -f "$BACKEND_PID_FILE" ]; then
        local pid=$(cat "$BACKEND_PID_FILE")
        if kill -0 "$pid" 2>/dev/null; then
            kill -- "-$pid" 2>/dev/null || kill "$pid" 2>/dev/null || true
            success "Backend stopped (PID: $pid)"
        fi
        rm -f "$BACKEND_PID_FILE"
    fi

    # Stop the (shared) database only when no other instances are still using
    # it — the DB is shared across ports, so tearing it down while another
    # instance runs would break that instance.
    local other_live=0
    for f in "$PID_DIR"/backend-*.pid; do
        [ -e "$f" ] || continue
        if kill -0 "$(cat "$f")" 2>/dev/null; then
            other_live=$((other_live + 1))
        fi
    done
    if [ "$other_live" -gt 0 ]; then
        warn "Leaving database up — $other_live other backend instance(s) still running."
    elif is_db_running; then
        docker compose -f docker-compose.test.yml down
        success "Database stopped"
    fi

    success "Services stopped (port $PORT)"
}

# Show status
show_status() {
    echo ""
    echo "Agent Portal Development Environment Status"
    echo "=================================================="
    echo ""

    # Database status
    if is_db_running; then
        echo -e "  Database:  ${GREEN}running${NC}"
    else
        echo -e "  Database:  ${RED}stopped${NC}"
    fi

    # Backend status
    if is_running "$BACKEND_PID_FILE"; then
        local pid=$(cat "$BACKEND_PID_FILE")
        echo -e "  Backend:   ${GREEN}running${NC} (PID: $pid)"

        # Check if it's actually responding
        if curl -sf http://localhost:$PORT/api/health > /dev/null 2>&1; then
            echo -e "  API:       ${GREEN}healthy${NC}"
        else
            echo -e "  API:       ${YELLOW}not responding${NC}"
        fi
    else
        echo -e "  Backend:   ${RED}stopped${NC}"
    fi

    echo ""
    echo "URLs:"
    echo "  Web Interface:  http://localhost:$PORT/"
    echo "  Backend API:    http://localhost:$PORT/api/"
    echo ""
    echo "Logs:"
    echo "  Backend: tail -f $BACKEND_LOG"
    echo ""
}

# Show logs
show_logs() {
    local service="${1:-backend}"
    case "$service" in
        backend)
            if [ -f "$BACKEND_LOG" ]; then
                tail -f "$BACKEND_LOG"
            else
                error "No backend log file found"
            fi
            ;;
        db|database)
            docker compose -f docker-compose.test.yml logs -f db
            ;;
        *)
            error "Unknown service: $service"
            echo "Usage: $0 logs [backend|db]"
            ;;
    esac
}

# Full start sequence
do_start() {
    echo ""
    echo "╔═══════════════════════════════════════════════════════════╗"
    echo "║       Starting Agent Portal Development Environment       ║"
    echo "╚═══════════════════════════════════════════════════════════╝"
    echo ""

    choose_available_port || exit 1
    start_db || exit 1
    run_migrations || exit 1
    build_frontend || exit 1
    start_backend || exit 1

    echo ""
    echo "╔═══════════════════════════════════════════════════════════╗"
    echo "║          ✅ Agent Portal Dev Environment Ready            ║"
    echo "╚═══════════════════════════════════════════════════════════╝"
    echo ""
    echo "  🌐 Web Interface:  http://localhost:$PORT/"
    echo "  📊 Backend API:    http://localhost:$PORT/api/"
    echo ""
    echo "  🧪 Test Account:   testing@testing.local"
    echo "  ⚠️  DEV MODE:       OAuth bypassed"
    echo ""
    echo "  🔌 To start a portal session:"
    echo "     1. Open http://localhost:$PORT/ and generate a setup token"
    echo "     2. Run the setup command shown in the UI"
    echo ""
    echo "Commands:"
    echo "  ./scripts/dev.sh status  - Show status"
    echo "  ./scripts/dev.sh logs    - Tail backend logs"
    echo "  ./scripts/dev.sh stop    - Stop all services"
    echo "  ./scripts/dev.sh restart - Restart all services"
    echo ""
}

# Nuke the database (delete all data and start fresh)
nuke_db() {
    warn "This will DELETE ALL DATA in the development database!"
    echo ""
    read -p "Are you sure? Type 'yes' to confirm: " confirm
    if [ "$confirm" != "yes" ]; then
        echo "Aborted."
        exit 1
    fi

    log "Stopping services..."
    stop_all

    log "Removing database volume..."
    docker volume rm agent-portal_test_postgres_data 2>/dev/null || true

    success "Database nuked. Run './scripts/dev.sh start' to recreate."
}

# Print usage
usage() {
    echo "Usage: $0 {start|stop|status|logs|restart|build|nuke-db}"
    echo ""
    echo "Commands:"
    echo "  start   - Start all services (db, backend)"
    echo "  stop    - Stop all services"
    echo "  status  - Show status of all services"
    echo "  logs    - Tail backend logs (or: logs db)"
    echo "  restart - Stop and start all services"
    echo "  build   - Rebuild frontend only"
    echo "  nuke-db - Delete all database data and start fresh"
    echo ""
}

# Main command handler
case "${1:-}" in
    start)
        do_start
        ;;
    stop)
        stop_all
        ;;
    status)
        show_status
        ;;
    logs)
        show_logs "${2:-backend}"
        ;;
    restart)
        stop_all
        sleep 2
        do_start
        ;;
    build)
        build_frontend
        ;;
    nuke-db)
        nuke_db
        ;;
    "")
        # Default to start if no argument
        do_start
        ;;
    *)
        error "Unknown command: $1"
        usage
        exit 1
        ;;
esac
