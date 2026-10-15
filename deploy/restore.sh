#!/usr/bin/env bash
# Restore a backup made by backup.sh: ./restore.sh backups/<name>
#
# Replaces the current database and local archive. Everything is validated
# and staged before live data is touched:
#   1. checksums, the dump's table of contents and the archive tarball verify;
#   2. .env must select the portal version the backup was taken with, so the
#      restored schema is never migrated forward by a newer image;
#   3. the dump is restored into a scratch database in one transaction;
#   4. only then is the portal stopped, the databases swapped by rename and
#      the archive swapped by directory rename, and the portal started.
# A failure before step 4 leaves the running portal and its data untouched.
set -euo pipefail

cd "$(dirname "$0")"
src=${1:?usage: ./restore.sh backups/<name>}
src=${src%/}

for f in db.dump archive.tar.gz backup.meta SHA256SUMS; do
  [[ -f $src/$f ]] || { echo "$src is not a complete backup (missing $f)" >&2; exit 1; }
done
(cd "$src" && sha256sum --quiet -c SHA256SUMS) || {
  echo "$src failed its checksum verification; nothing was changed." >&2
  exit 1
}
tar -tzf "$src/archive.tar.gz" > /dev/null 2>&1 || {
  echo "$src/archive.tar.gz is unreadable; nothing was changed." >&2
  exit 1
}

# shellcheck disable=SC1091
backup_version=$(. "$src/backup.meta" && echo "$PORTAL_VERSION")
# shellcheck disable=SC1091
current_version=$(. ./.env && echo "$PORTAL_VERSION")
if [[ $backup_version != "$current_version" ]]; then
  cat >&2 <<EOF
This backup was taken from portal $backup_version but .env selects $current_version.
A different image would migrate (or fail on) the restored schema. Set
PORTAL_VERSION=$backup_version in deploy/.env, run 'docker compose pull portal',
then run this script again. Nothing was changed.
EOF
  exit 1
fi

read -r -p "Replace the current database and archive with $src? Type 'restore': " confirm
[[ $confirm == restore ]] || { echo "Aborted; nothing was changed."; exit 1; }

psql_admin() { docker compose exec -T db psql -U agent_portal -d postgres -v ON_ERROR_STOP=1 -qc "$1"; }

docker compose up -d db
until docker compose exec -T db pg_isready -U agent_portal -d agent_portal > /dev/null; do sleep 1; done
docker compose exec -T db pg_restore --list < "$src/db.dump" > /dev/null 2>&1 || {
  echo "$src/db.dump is unreadable; nothing was changed." >&2
  exit 1
}

docker compose exec -T db dropdb -U agent_portal --if-exists agent_portal_restore
docker compose exec -T db createdb -U agent_portal agent_portal_restore
if ! docker compose exec -T db pg_restore -U agent_portal -d agent_portal_restore \
  --no-owner --exit-on-error --single-transaction < "$src/db.dump"; then
  docker compose exec -T db dropdb -U agent_portal --if-exists agent_portal_restore
  echo "The dump did not restore cleanly; the live database was not touched." >&2
  exit 1
fi

docker compose stop portal

# Stage the archive inside the same volume so the final swap is a rename.
archive_sh() { docker compose run --rm --no-deps -T --entrypoint sh portal -c "$1"; }
if ! archive_sh 'set -e; rm -rf /data/archive/.restore-new; mkdir /data/archive/.restore-new
     tar -xzf - -C /data/archive/.restore-new --strip-components=1' < "$src/archive.tar.gz"; then
  archive_sh 'rm -rf /data/archive/.restore-new' || true
  docker compose exec -T db dropdb -U agent_portal --if-exists agent_portal_restore
  docker compose up -d
  echo "The archive did not extract; the live data was not touched." >&2
  exit 1
fi

psql_admin "ALTER DATABASE agent_portal RENAME TO agent_portal_previous"
psql_admin "ALTER DATABASE agent_portal_restore RENAME TO agent_portal"
archive_sh 'set -e; cd /data/archive; mkdir .restore-old
  find . -mindepth 1 -maxdepth 1 ! -name .restore-new ! -name .restore-old -exec mv {} .restore-old/ \;
  find .restore-new -mindepth 1 -maxdepth 1 -exec mv {} . \;
  rm -rf .restore-new .restore-old'
psql_admin "DROP DATABASE agent_portal_previous"

docker compose up -d
echo "Restored portal $backup_version data from deploy/$src"
