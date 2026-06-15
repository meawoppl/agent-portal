-- One-time cleanup of leaked launcher-spawned tokens (see #1045).
--
-- `launcher-spawned` tokens are minted per launch and only ever bound to a
-- session (and old ones revoked) on a *successful* proxy registration. Every
-- launch whose proxy failed to register left behind a never-expiring,
-- never-bound, never-revoked token. These rows only have a legitimate reason
-- to exist transiently between mint and registration, so any still unbound an
-- hour after creation belong to a failed launch and are safe to delete.
DELETE FROM proxy_auth_tokens
WHERE name = 'launcher-spawned'
  AND session_id IS NULL
  AND created_at < (now() AT TIME ZONE 'UTC') - INTERVAL '1 hour';
