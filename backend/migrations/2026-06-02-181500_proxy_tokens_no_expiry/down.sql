ALTER TABLE proxy_auth_tokens DROP COLUMN session_id;

-- Backfill any NULL expiries before restoring the NOT NULL constraint.
UPDATE proxy_auth_tokens SET expires_at = now() WHERE expires_at IS NULL;
ALTER TABLE proxy_auth_tokens ALTER COLUMN expires_at SET NOT NULL;
