ALTER TABLE secret_bindings ADD COLUMN proxy_token_id uuid REFERENCES proxy_auth_tokens(id) ON DELETE CASCADE;
-- An older session-only approval must never silently become process approval.
UPDATE secret_bindings SET revoked_at=COALESCE(revoked_at,now()) WHERE proxy_token_id IS NULL;

ALTER TABLE secret_bindings ADD CONSTRAINT secret_binding_process_required CHECK(proxy_token_id IS NOT NULL OR revoked_at IS NOT NULL);
