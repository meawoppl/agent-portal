CREATE TABLE session_continuations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    launcher_id UUID NOT NULL,
    reset_at TIMESTAMPTZ NOT NULL,
    prompt TEXT NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'pending',
    source_message TEXT,
    last_error TEXT,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    scheduled_at TIMESTAMP,
    fired_at TIMESTAMP,
    dropped_at TIMESTAMP,
    cancelled_at TIMESTAMP
);

CREATE INDEX idx_session_continuations_launcher_status_reset
    ON session_continuations (launcher_id, status, reset_at);

CREATE INDEX idx_session_continuations_session
    ON session_continuations (session_id);
