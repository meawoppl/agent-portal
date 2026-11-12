ALTER TABLE secret_bindings ADD COLUMN version_id uuid;
UPDATE secret_bindings b SET version_id=s.current_version_id FROM secrets s WHERE s.id=b.secret_id;
ALTER TABLE secret_bindings ALTER COLUMN version_id SET NOT NULL;
ALTER TABLE secret_bindings ADD CONSTRAINT secret_binding_version FOREIGN KEY(secret_id,version_id) REFERENCES secret_versions(secret_id,id);
ALTER TABLE secret_bindings ADD COLUMN env_name text NOT NULL DEFAULT 'PORTAL_SECRET';
ALTER TABLE secret_bindings DROP CONSTRAINT secret_bindings_session_id_fkey;
ALTER TABLE secret_bindings ADD CONSTRAINT secret_bindings_session_id_fkey FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE;
CREATE TABLE secret_redemption_tickets (
 id uuid PRIMARY KEY, binding_id uuid NOT NULL REFERENCES secret_bindings(id) ON DELETE CASCADE,
 proxy_token_id uuid NOT NULL REFERENCES proxy_auth_tokens(id) ON DELETE CASCADE,
 token_hash text NOT NULL UNIQUE, expires_at timestamptz NOT NULL, consumed_at timestamptz,
 created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX secret_redemption_tickets_expiry ON secret_redemption_tickets(expires_at);

ALTER TABLE secret_mutations ADD COLUMN binding_id uuid REFERENCES secret_bindings(id) ON DELETE CASCADE;
