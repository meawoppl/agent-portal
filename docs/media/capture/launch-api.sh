#!/usr/bin/env bash
# launch-api.sh <name> [worktree:true|false] [model]
set -euo pipefail
NAME="${1:-rate-cache}"; WT="${2:-true}"; MODEL="${3:-claude-haiku-4-5}"
LID=$(curl -s ${DEMO_URL:-http://localhost:3100}/api/launchers | python3 -c "import sys,json;print(json.load(sys.stdin)[0]['launcher_id'])")
curl -s -X POST ${DEMO_URL:-http://localhost:3100}/api/launch -H 'Content-Type: application/json' \
  -d "{\"working_directory\":\"${DEMO_ROOT:-/tmp/readme-demo}/home/acme-api\",\"launcher_id\":\"$LID\",\"claude_args\":[\"--model\",\"$MODEL\"],\"agent_type\":\"claude\",\"name\":\"$NAME\",\"create_worktree\":$WT}"
echo
