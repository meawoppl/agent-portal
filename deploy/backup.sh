#!/usr/bin/env bash
# Back up the database and the local session archive into
# deploy/backups/<timestamp>-<id>/ (private to the current user).
#
# Safe while the portal is up: pg_dump takes a consistent snapshot. The backup
# is assembled in a temporary directory and only published once every part
# has been written and verified, so a directory under backups/ is always
# complete. deploy/.env is NOT copied here — store it separately; every
# cookie, token and the database password depend on it.
set -euo pipefail
umask 077

cd "$(dirname "$0")"
# shellcheck disable=SC1091
version=$(. ./.env && echo "${PORTAL_VERSION:?PORTAL_VERSION missing from .env}")

mkdir -p backups
work=$(mktemp -d backups/.partial-XXXXXX)
trap 'rm -rf "$work"' EXIT

docker compose exec -T db pg_dump -U agent_portal -d agent_portal --format=custom \
  > "$work/db.dump"
docker compose exec -T db pg_restore --list < "$work/db.dump" > /dev/null

docker compose exec -T portal tar -C /data -czf - --exclude='archive/.restore-*' archive \
  > "$work/archive.tar.gz"
tar -tzf "$work/archive.tar.gz" > /dev/null

{
  echo "PORTAL_VERSION=$version"
  echo "CREATED=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} > "$work/backup.meta"
(cd "$work" && sha256sum db.dump archive.tar.gz backup.meta > SHA256SUMS)

dest="backups/$(date -u +%Y%m%dT%H%M%SZ)-${work##*-}"
mv "$work" "$dest"
trap - EXIT
echo "Backup of portal $version written to deploy/$dest"
