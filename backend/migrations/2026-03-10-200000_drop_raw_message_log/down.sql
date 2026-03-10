CREATE TABLE raw_message_log (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id UUID REFERENCES sessions(id) ON DELETE SET NULL,
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    message_content JSONB NOT NULL,
    message_source VARCHAR(50) NOT NULL,
    render_reason VARCHAR(255),
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    content_hash VARCHAR(64) NOT NULL
);

CREATE INDEX idx_raw_message_log_created_at ON raw_message_log(created_at DESC);
CREATE INDEX idx_raw_message_log_session_id ON raw_message_log(session_id);
CREATE UNIQUE INDEX idx_raw_message_log_dedup
ON raw_message_log (session_id, content_hash)
WHERE session_id IS NOT NULL;
