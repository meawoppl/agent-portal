-- Per-session and launcher proxy tokens no longer carry a fixed TTL. Their
-- lifetime is tracked by revocation (revoke-on-terminate) plus the always-live
-- DB checks in verify_and_get_user, not by `expires_at`. See #932.
--
-- expires_at becomes nullable: NULL means "never expires". User-created
-- dashboard CLI tokens still set an explicit expiry.
ALTER TABLE proxy_auth_tokens ALTER COLUMN expires_at DROP NOT NULL;

-- session_id links a launch token to the session whose proxy holds it, so the
-- token can be revoked when that session terminates.
ALTER TABLE proxy_auth_tokens ADD COLUMN session_id UUID;
