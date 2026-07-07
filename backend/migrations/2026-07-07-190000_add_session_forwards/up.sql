-- Port-forward allowlist (docs/PORT_FORWARDING.md): one row per (session, port)
-- an agent has declared via `agent-portal forward <port>`. The backend only
-- tunnels ports with a live row here; rows die with the session (cascade +
-- session reaper), so forward lifetime is strictly session lifetime.
CREATE TABLE session_forwards (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    port INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
    created_at TIMESTAMP NOT NULL DEFAULT now(),
    UNIQUE (session_id, port)
);

CREATE INDEX idx_session_forwards_session_id ON session_forwards(session_id);
