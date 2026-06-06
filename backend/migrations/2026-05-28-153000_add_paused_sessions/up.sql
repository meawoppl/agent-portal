ALTER TABLE sessions
    ADD COLUMN paused BOOLEAN NOT NULL DEFAULT TRUE,
    ADD COLUMN claude_args JSONB NOT NULL DEFAULT '[]'::jsonb;

CREATE INDEX idx_sessions_paused ON sessions(paused);
