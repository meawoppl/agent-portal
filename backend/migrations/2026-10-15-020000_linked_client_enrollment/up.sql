CREATE TABLE linked_client_enrollments (
    device_code_hash VARCHAR(64) PRIMARY KEY,
    user_code VARCHAR(32) NOT NULL UNIQUE,
    client_origin VARCHAR(255) NOT NULL,
    requested_scope VARCHAR(16) NOT NULL CHECK (requested_scope IN ('read', 'write')),
    device_label VARCHAR(128),
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMP NOT NULL,
    status VARCHAR(16) NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','approved','denied','consumed')),
    user_id UUID REFERENCES users(id) ON DELETE CASCADE,
    denial_reason VARCHAR(32)
);
CREATE INDEX linked_enrollment_expiry ON linked_client_enrollments(expires_at);
