-- A home-portal user's links to accounts on other portals (federated views).
-- Metadata only: the credential for each connection never leaves the device
-- that enrolled it. remote_user_id is the linked portal's own user id, kept as
-- a key, never matched against local users.
CREATE TABLE portal_connections (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    portal_origin VARCHAR(255) NOT NULL,
    instance_id UUID NOT NULL,
    remote_user_id UUID NOT NULL,
    remote_email VARCHAR(255) NOT NULL,
    label VARCHAR(64) NOT NULL,
    scope VARCHAR(16) NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP NOT NULL DEFAULT NOW(),
    UNIQUE (user_id, portal_origin, remote_user_id)
);
