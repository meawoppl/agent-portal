CREATE TABLE federation_policy (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    policy JSONB NOT NULL DEFAULT '{"mode":"off"}'::jsonb
);
INSERT INTO federation_policy DEFAULT VALUES;
CREATE TABLE linked_client_grants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    client_origin VARCHAR(255) NOT NULL,
    scope VARCHAR(16) NOT NULL CHECK (scope IN ('read','write')),
    device_label VARCHAR(128),
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMP NOT NULL,
    revoked_at TIMESTAMP,
    UNIQUE (id, user_id)
);
ALTER TABLE proxy_auth_tokens
    ADD COLUMN scope VARCHAR(16),
    ADD COLUMN linked_grant_id UUID,
    ADD CONSTRAINT linked_token_scope CHECK (
        (scope IS NULL AND linked_grant_id IS NULL) OR
        (scope IS NOT NULL AND scope IN ('read','write') AND linked_grant_id IS NOT NULL AND expires_at IS NOT NULL)
    ),
    ADD CONSTRAINT linked_token_owner FOREIGN KEY (linked_grant_id, user_id)
        REFERENCES linked_client_grants(id, user_id) ON DELETE CASCADE;
CREATE INDEX linked_tokens_grant ON proxy_auth_tokens(linked_grant_id) WHERE linked_grant_id IS NOT NULL;
