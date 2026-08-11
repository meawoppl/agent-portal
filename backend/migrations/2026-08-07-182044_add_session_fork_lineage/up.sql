-- Your SQL goes here
ALTER TABLE sessions
    ADD COLUMN forked_from_session_id UUID REFERENCES sessions(id) ON DELETE SET NULL,
    ADD COLUMN fork_point_turn_id TEXT,
    ADD COLUMN fork_launch_pending BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN fork_create_worktree BOOLEAN NOT NULL DEFAULT FALSE;

CREATE INDEX sessions_forked_from_session_id_idx
    ON sessions (forked_from_session_id);
