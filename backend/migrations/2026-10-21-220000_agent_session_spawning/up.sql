ALTER TABLE sessions
    ADD COLUMN spawned_by UUID REFERENCES sessions(id) ON DELETE SET NULL,
    ADD COLUMN spawn_depth INTEGER NOT NULL DEFAULT 1 CHECK (spawn_depth BETWEEN 1 AND 8),
    ADD COLUMN spawn_idle_timeout_seconds INTEGER CHECK (spawn_idle_timeout_seconds BETWEEN 60 AND 604800),
    ADD COLUMN spawn_stop_on_parent_end BOOLEAN NOT NULL DEFAULT FALSE;
CREATE INDEX sessions_spawned_by_idx ON sessions(spawned_by) WHERE spawned_by IS NOT NULL;

CREATE TABLE session_spawn_policies (
    session_id UUID PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    approval_mode TEXT NOT NULL DEFAULT 'first_spawn' CHECK (approval_mode IN ('first_spawn', 'auto', 'off')),
    max_children INTEGER NOT NULL DEFAULT 4 CHECK (max_children BETWEEN 1 AND 32),
    max_depth INTEGER NOT NULL DEFAULT 2 CHECK (max_depth BETWEEN 1 AND 8),
    idle_timeout_seconds INTEGER NOT NULL DEFAULT 1800 CHECK (idle_timeout_seconds BETWEEN 60 AND 604800),
    stop_on_parent_end BOOLEAN NOT NULL DEFAULT FALSE,
    first_approved_at TIMESTAMP
);

CREATE TABLE session_spawn_requests (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- Audit survives deletion of the parent session.
    parent_session_id UUID NOT NULL,
    launcher_id UUID NOT NULL,
    request JSONB NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'launching', 'launched', 'denied', 'failed')),
    -- Reserved before the child exists; retained for failed-launch audit.
    child_session_id UUID,
    approved_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP NOT NULL DEFAULT NOW(),
    error_code TEXT
);
CREATE INDEX session_spawn_requests_parent_idx ON session_spawn_requests(parent_session_id, created_at);
CREATE INDEX session_spawn_requests_user_rate_idx ON session_spawn_requests(user_id, created_at);
