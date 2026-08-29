#!/usr/bin/env bash
set -uo pipefail
R=${DEMO_ROOT:-/tmp/readme-demo}/home/acme-api
docker exec claude-portal-test-db psql -U claude_portal -d readme_demo -c "TRUNCATE sessions CASCADE;" >/dev/null 2>&1
rm -rf "$R/.worktrees"
git -C "$R" worktree prune >/dev/null 2>&1
for b in $(git -C "$R" for-each-ref --format='%(refname:short)' refs/heads | grep -v '^main$'); do
  git -C "$R" branch -D "$b" >/dev/null 2>&1
done
git -C "$R" checkout -q main 2>/dev/null
git -C "$R" reset -q --hard HEAD
git -C "$R" clean -qfd
echo "reset: branches=$(git -C "$R" for-each-ref --format='%(refname:short)' refs/heads | tr '\n' ' ')"
