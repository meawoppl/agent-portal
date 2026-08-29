#!/usr/bin/env bash
# Wipe the demo sessions, then re-mint the CLI's auth token.
#
# Order matters: `TRUNCATE sessions CASCADE` also truncates `proxy_auth_tokens`
# (it carries a session_id FK), so any token minted before the wipe is gone and
# `agent-portal forward` fails with 401. Mint after.
set -euo pipefail
ROOT="${DEMO_ROOT:-/tmp/readme-demo}"
URL="${DEMO_URL:-http://localhost:3100}"
DB_CONTAINER="${DEMO_DB_CONTAINER:-claude-portal-test-db}"

docker exec "$DB_CONTAINER" psql -U claude_portal -d readme_demo \
  -c "TRUNCATE sessions CASCADE;" >/dev/null

TOKEN=$(curl -s -X POST "$URL/api/proxy-tokens" -H 'Content-Type: application/json' \
          -d '{"name":"demo-workstation","expires_in_days":1}' \
        | python3 -c "import sys, json; print(json.load(sys.stdin)['token'])")

CONFIG="$ROOT/home/.config/agent-portal/launcher.json"
python3 - "$CONFIG" "$TOKEN" <<'PY'
import json, sys
path, token = sys.argv[1], sys.argv[2]
cfg = json.load(open(path))
cfg['auth_token'] = token
json.dump(cfg, open(path, 'w'), indent=1)
PY
echo "reset: sessions wiped, token re-minted into $CONFIG"
