# shellcheck shell=bash
# Shared values for the scripts in this directory.
# Source this file; do not execute it:
#   SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
#   source "$SCRIPT_DIR/lib.sh"

# Development database URL. Must match the credentials in
# docker-compose.test.yml. Documented in scripts/README.md.
DEV_DATABASE_URL="postgresql://claude_portal:dev_password_change_in_production@localhost:5432/claude_portal"
