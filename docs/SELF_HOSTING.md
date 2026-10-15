# Self-hosting Agent Portal

Run your own portal on one machine with Docker: Postgres, the portal, and
Caddy terminating HTTPS with an automatic Let's Encrypt certificate. Nothing is
compiled; you pull a released image.

Everything lives in [`deploy/`](../deploy):

| File | Purpose |
|---|---|
| `compose.yaml` | The stack. Only Caddy publishes ports (80, 443); Postgres has no published port and is isolated from the edge network. |
| `Caddyfile` | HTTPS + reverse proxy (WebSockets included) to the portal. |
| `setup.sh` | Writes `deploy/.env`: generates secrets, asks for domain and login provider. |
| `backup.sh` / `restore.sh` | Database + session-archive backup and restore. |

## Before you start

You need all of these; the stack cannot work around any of them.

1. **A Linux host** with Docker Engine and the Compose plugin (`docker compose version`).
2. **A domain name** (e.g. `portal.example.com`) whose DNS `A`/`AAAA` record
   points at the host, and **ports 80 and 443 reachable** from the internet —
   Let's Encrypt validates over port 80.
3. **An OAuth app** with Google or GitHub. You'll register the callback URL
   that `setup.sh` prints:
   - GitHub: *Settings → Developer settings → OAuth Apps → New*. Callback:
     `https://<domain>/api/auth/github/callback`.
   - Google: *Cloud Console → APIs & Services → Credentials → OAuth client ID
     (Web application)*. Redirect URI:
     `https://<domain>/api/auth/google/callback`.

   One provider is enough. To offer both, add the second provider's three
   `*_CLIENT_ID`/`*_CLIENT_SECRET`/`*_REDIRECT_URI` lines to `.env` (all three
   or none).

## Install

```bash
git clone https://github.com/meawoppl/agent-portal.git
cd agent-portal/deploy
./setup.sh            # answers go into deploy/.env (mode 600)
docker compose up -d
```

Open `https://<domain>` and sign in. The first boot creates the schema; the
certificate is issued within a minute of DNS resolving. Then, on each machine
that should run agents:

```bash
curl -fsSL "https://<domain>/api/download/install.sh" | bash
agent-portal login              # approve the device code in your browser
agent-portal service install    # keep it running as a systemd / launchd service
```

`setup.sh` suggests the version matching your checkout. Images are published
for every commit on `main` as `ghcr.io/meawoppl/agent-portal:<version>`, where
the version is the one the portal reports in its footer and at `/api/health`.

**Restricting sign-in.** `setup.sh` asks for `ALLOWED_EMAILS` and/or
`ALLOWED_EMAIL_DOMAIN`. Leave both blank only if anyone with an account at your
provider should be able to use your portal and its agents.

**Secrets.** `deploy/.env` holds the database password and `SESSION_SECRET`,
which signs every cookie and launcher token. Back it up somewhere other than
the host. Losing it signs everyone out and disconnects every launcher;
changing `POSTGRES_PASSWORD` after first boot locks the portal out of its own
database.

## Optional features

Everything below is off unless configured in `.env`; the core chat works
without any of it. See the environment table in the main README for details.

- **Speech-to-text** — `PORTAL_STT_BACKEND` and friends.
- **Push notifications** — `PORTAL_VAPID_PRIVATE_KEY` / `PORTAL_VAPID_PUBLIC_KEY`
  (generate with `scripts/generate-vapid-keys.sh`).
- **Port forwarding** — `PORTAL_FORWARD_DOMAIN` needs a wildcard DNS record and
  a wildcard certificate, which Caddy can only obtain via a DNS-01 plugin. See
  [PORT_FORWARDING.md](PORT_FORWARDING.md).

The session archive lives on the stack's `archive` volume, and `backup.sh` /
`restore.sh` cover it. An S3 archive needs a Compose override and its own
bucket backups; this stack and its scripts support the local archive only.

## Backups

```bash
cd deploy
./backup.sh           # → deploy/backups/<timestamp>-<id>/
```

Runs while the portal is up. Each backup holds the database dump, the archive,
the portal version it was taken from and checksums; it is private to your
user and only appears once complete. Copy `deploy/backups/` and `deploy/.env`
off the host on a schedule (e.g. a nightly cron running `backup.sh` then
`rsync`).

## Restore

```bash
cd deploy
./restore.sh backups/<name>
```

Verifies the backup's checksums, dump and archive, and refuses unless `.env`
selects the **same `PORTAL_VERSION`** the backup was taken from. It then
restores the dump into a scratch database and stages the archive; only when
both succeed does it stop the portal, swap the new data in, and start it again.
A bad backup leaves the running portal untouched. Restore also needs the
**same secrets** in `.env` the backup was taken with.

Test a restore on a scratch host before you need one: install as above with
the backed-up `.env`, `docker compose up -d`, copy the backup in, run
`restore.sh`.

## Upgrading

```bash
cd deploy
./backup.sh
sed -i "s/^PORTAL_VERSION=.*/PORTAL_VERSION='<new version>'/" .env
docker compose pull portal && docker compose up -d
```

The portal applies any new database migrations when it starts. Launchers
update themselves from the latest GitHub release independently of your portal
version, so keep the portal reasonably current.

## Rolling back

Migrations only move forward, so an older image may not run against a schema a
newer one has migrated. To roll back, select the old image first, then
restore the backup you took before upgrading:

```bash
cd deploy
sed -i "s/^PORTAL_VERSION=.*/PORTAL_VERSION='<old version>'/" .env
docker compose pull portal
./restore.sh backups/<pre-upgrade backup>
```

`restore.sh` refuses a backup whose version doesn't match `.env`, so the newer
image never boots against the restored schema. Sessions and messages created
since that backup are lost.

## Troubleshooting

- **Certificate never issues** — `docker compose logs caddy`. Usually DNS not
  yet pointing at the host, or port 80 blocked.
- **"redirect_uri mismatch" at sign-in** — the callback registered with the
  provider must match `*_REDIRECT_URI` in `.env` exactly.
- **Portal restarting** — `docker compose logs portal`; it refuses to boot with
  a half-configured provider (some but not all of its three variables set).

See also [TROUBLESHOOTING.md](TROUBLESHOOTING.md).
