#!/bin/bash
# Open a PostgreSQL shell to the test database

if ! docker ps | grep -q claude-portal-test-db; then
    echo "Database not running. Start with:"
    echo "  docker compose -f docker-compose.test.yml up -d db"
    exit 1
fi

docker compose -f docker-compose.test.yml exec db psql -U claude_portal -d claude_portal
