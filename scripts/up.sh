#!/bin/bash
# One-command portal startup: ensures Docker is running, then starts dev environment
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Check if Docker daemon is running
if ! docker info >/dev/null 2>&1; then
    case "$OSTYPE" in
        darwin*)
            echo "Starting Docker Desktop..."
            open -a Docker
            ;;
        linux*)
            echo "Starting Docker daemon via systemctl..."
            if ! systemctl start docker 2>/dev/null && ! sudo systemctl start docker; then
                echo "Error: failed to start the Docker daemon."
                echo "Start it manually (e.g. 'sudo systemctl start docker') and re-run."
                exit 1
            fi
            ;;
        *)
            echo "Error: Docker daemon is not running and \$OSTYPE '$OSTYPE' is not recognized."
            echo "Start Docker manually and re-run."
            exit 1
            ;;
    esac

    echo -n "Waiting for Docker"
    for i in {1..60}; do
        if docker info >/dev/null 2>&1; then
            echo " ready!"
            break
        fi
        echo -n "."
        sleep 2
    done

    if ! docker info >/dev/null 2>&1; then
        echo ""
        echo "Error: Docker failed to start after 2 minutes"
        exit 1
    fi
fi

exec "$SCRIPT_DIR/dev.sh" start
