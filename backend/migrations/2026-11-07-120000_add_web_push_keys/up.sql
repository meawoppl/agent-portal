-- One durable application-server identity shared by all backend instances.
-- Only the public key derived from this secret may be exposed to clients.
CREATE TABLE web_push_keys (
    id BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
    private_key TEXT NOT NULL
);
