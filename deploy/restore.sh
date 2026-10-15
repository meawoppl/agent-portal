#!/usr/bin/env bash
# Restore a backup made by backup.sh: ./restore.sh backups/<name>
#
# Replaces the current database and local archive. Everything is validated
# and staged while the portal keeps running:
#   1. checksums, the dump's table of contents and the archive tarball verify;
#   2. .env must select the portal version the backup was taken with, so the
#      restored schema is never migrated forward by a newer image;
#   3. the dump is restored into a scratch database in one transaction and the
#      archive is extracted into a staging directory on the archive volume.
# Only then is the portal stopped and the data swapped in by renames. The
# previous database and archive are kept until the restarted portal reports
# healthy; if anything fails after the stop, the portal stays stopped and the
# script prints how to put the previous data back.
set -Eeuo pipefail

cd "$(dirname "$0")"
src=${1:?usage: ./restore.sh backups/<name>}
src=${src%/}

exec 9> .restore.lock
flock -n 9 || { echo "Another restore is running." >&2; exit 1; }

# `docker compose exec -T` forwards stdin, so keep the caller's stdin for the
# confirmation prompt and give every other command /dev/null.
exec 3<&0 < /dev/null

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

db() { docker compose exec -T db "$@"; }
psql_admin() { db psql -U agent_portal -d postgres -v ON_ERROR_STOP=1 -tAqc "$1"; }
# A one-off container on the archive volume. Never `up`/`exec` the portal
# service before the swap: after a rollback edit to .env, Compose would
# recreate it with the older image against the not-yet-restored database.
archive_sh() { docker compose run --rm --no-deps -T --entrypoint sh portal -c "$1"; }

docker compose up -d db
until db pg_isready -U agent_portal -d agent_portal > /dev/null; do sleep 1; done

leftovers=$(psql_admin "SELECT string_agg(datname, ' ') FROM pg_database
  WHERE datname IN ('agent_portal_restore', 'agent_portal_previous')")
if [[ -n $leftovers ]] || archive_sh 'test -e /data/archive/.restore-old'; then
  echo "Artifacts from an earlier restore remain (${leftovers:-archive .restore-old})." >&2
  echo "Finish or undo that restore first; see docs/SELF_HOSTING.md. Nothing was changed." >&2
  exit 1
fi
archive_sh 'rm -rf /data/archive/.restore-new'

db pg_restore --list < "$src/db.dump" > /dev/null 2>&1 || {
  echo "$src/db.dump is unreadable; nothing was changed." >&2
  exit 1
}

read -r -u 3 -p "Replace the current database and archive with $src? Type 'restore': " confirm
[[ $confirm == restore ]] || { echo "Aborted; nothing was changed."; exit 1; }

discard_staging() {
  db dropdb -U agent_portal --if-exists agent_portal_restore || true
  archive_sh 'rm -rf /data/archive/.restore-new' || true
}

db createdb -U agent_portal agent_portal_restore
if ! db pg_restore -U agent_portal -d agent_portal_restore \
  --no-owner --exit-on-error --single-transaction < "$src/db.dump"; then
  discard_staging
  echo "The dump did not restore cleanly; the live data was not touched." >&2
  exit 1
fi
if ! archive_sh 'set -e; mkdir /data/archive/.restore-new
     tar -xzf - -C /data/archive/.restore-new --strip-components=1' < "$src/archive.tar.gz"; then
  discard_staging
  echo "The archive did not extract; the live data was not touched." >&2
  exit 1
fi

# --- Point of no return: everything below runs with the portal stopped. ---
docker compose stop portal

# Print the exact steps that put the previous data back, for the state the
# swap actually reached. Nothing here runs them: the operator decides.
recovery() {
  trap - ERR
  set +e
  docker compose stop portal > /dev/null 2>&1
  local dbs
  dbs=$(psql_admin "SELECT string_agg(datname, ' ') FROM pg_database
    WHERE datname IN ('agent_portal_restore', 'agent_portal_previous')")
  echo >&2
  echo "The restore stopped part-way. The portal is stopped; the previous data is kept." >&2
  echo "To put it back, run these from deploy/:" >&2
  if [[ $dbs == *agent_portal_previous* ]]; then
    echo "  docker compose exec -T db psql -U agent_portal -d postgres \\" >&2
    echo "    -c 'DROP DATABASE agent_portal' \\" >&2
    echo "    -c 'ALTER DATABASE agent_portal_previous RENAME TO agent_portal'" >&2
  fi
  if [[ $dbs == *agent_portal_restore* ]]; then
    echo "  docker compose exec -T db dropdb -U agent_portal agent_portal_restore" >&2
  fi
  # The marker is written only once every original entry is inside
  # .restore-old; until then the root still holds untouched originals, which
  # must be kept, not deleted.
  if archive_sh 'test -e /data/archive/.restore-old-complete'; then
    cat >&2 <<'EOF'
  docker compose run --rm --no-deps -T --entrypoint sh portal -c 'set -e
    cd /data/archive
    find . -mindepth 1 -maxdepth 1 ! -name .restore-old -exec rm -rf {} +
    find .restore-old -mindepth 1 -maxdepth 1 -exec mv -t . -- {} +
    rmdir .restore-old'
EOF
  elif archive_sh 'test -d /data/archive/.restore-old'; then
    cat >&2 <<'EOF'
  docker compose run --rm --no-deps -T --entrypoint sh portal -c 'set -e
    cd /data/archive
    find .restore-old -mindepth 1 -maxdepth 1 -exec mv -t . -- {} +
    rmdir .restore-old
    rm -rf .restore-new'
EOF
  else
    echo "  docker compose run --rm --no-deps -T --entrypoint sh portal -c 'rm -rf /data/archive/.restore-new'" >&2
  fi
  echo "  docker compose up -d" >&2
  exit 1
}
trap recovery ERR

psql_admin "ALTER DATABASE agent_portal RENAME TO agent_portal_previous"
psql_admin "ALTER DATABASE agent_portal_restore RENAME TO agent_portal"
# Batched `-exec … +` so a failed move fails find, and with it `set -e`;
# `-exec … \;` would report success after any individual mv failed.
archive_sh 'set -e; cd /data/archive; mkdir .restore-old
  find . -mindepth 1 -maxdepth 1 ! -name ".restore-*" -exec mv -t .restore-old -- {} +
  touch .restore-old-complete
  find .restore-new -mindepth 1 -maxdepth 1 -exec mv -t . -- {} +
  rmdir .restore-new'

docker compose up -d
healthy() {
  local id
  id=$(docker compose ps -q portal) && [[ -n $id ]] &&
    [[ $(docker inspect -f '{{.State.Health.Status}}' "$id") == healthy ]]
}
for _ in $(seq 1 60); do
  healthy && break
  sleep 3
done
healthy
trap - ERR

psql_admin "DROP DATABASE agent_portal_previous"
archive_sh 'rm -rf /data/archive/.restore-old /data/archive/.restore-old-complete'
echo "Restored portal $backup_version data from deploy/$src"
