CREATE TABLE linked_ws_tickets (
    ticket_hash VARCHAR(64) PRIMARY KEY,
    token_id UUID NOT NULL REFERENCES proxy_auth_tokens(id) ON DELETE CASCADE,
    grant_id UUID NOT NULL REFERENCES linked_client_grants(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    client_origin VARCHAR(255) NOT NULL,
    scope VARCHAR(16) NOT NULL CHECK (scope IN ('read', 'write')),
    expires_at TIMESTAMP NOT NULL
);
CREATE INDEX linked_ws_ticket_expiry ON linked_ws_tickets(expires_at);
