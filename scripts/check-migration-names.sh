#!/usr/bin/env bash
#
# Validates that all Diesel migration directories follow the naming convention:
#   - 00000000000000_<description>  (special initial migration)
#   - YYYY-MM-DD-HHMMSS_<description>  (standard timestamp format)
#
# Where <description> is lowercase snake_case (e.g., add_users_table)
#
# Usage: ./scripts/check-migration-names.sh
# Exit code: 0 if all valid, 1 if any invalid

set -euo pipefail

MIGRATIONS_DIR="backend/migrations"
ERRORS=0

# Pattern for valid migration names:
# - Initial migration: 00000000000000_<snake_case>
# - Timestamped: YYYY-MM-DD-HHMMSS_<snake_case>
INITIAL_PATTERN='^00000000000000_[a-z][a-z0-9_]*$'
TIMESTAMP_PATTERN='^[0-9]{4}-[0-9]{2}-[0-9]{2}-[0-9]{6}_[a-z][a-z0-9_]*$'

echo "Checking migration naming convention..."
echo "Expected format: YYYY-MM-DD-HHMMSS_snake_case_description"
echo "---"

for dir in "$MIGRATIONS_DIR"/*/; do
    # Skip if not a directory
    [[ -d "$dir" ]] || continue

    name=$(basename "$dir")

    # Skip hidden files/dirs
    [[ "$name" == .* ]] && continue

    if [[ "$name" =~ $INITIAL_PATTERN ]] || [[ "$name" =~ $TIMESTAMP_PATTERN ]]; then
        echo "  OK: $name"
    else
        echo "  ERROR: $name"
        echo "         Expected format: YYYY-MM-DD-HHMMSS_snake_case_description"
        echo "         Example: 2026-01-15-143022_add_users_table"
        ERRORS=$((ERRORS + 1))
    fi
done

echo "---"

if [[ $ERRORS -gt 0 ]]; then
    echo "FAILED: $ERRORS migration(s) have invalid names"
    echo ""
    echo "To fix, rename the migration directory to match the format:"
    echo "  YYYY-MM-DD-HHMMSS_snake_case_description"
    echo ""
    echo "If renaming an existing migration, update the database with:"
    echo "  See scripts/fix_migration_names.sql for an example"
    exit 1
else
    echo "PASSED: All migrations follow naming convention"
    exit 0
fi
