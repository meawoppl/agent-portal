CREATE TABLE session_attention (
    session_id UUID PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    reply_id UUID NOT NULL,
    reply_at TIMESTAMP NOT NULL,
    generation BIGINT NOT NULL DEFAULT 1,
    preview TEXT NOT NULL DEFAULT ''
);
CREATE TABLE session_attention_receipts (
    session_id UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    seen_generation BIGINT NOT NULL DEFAULT 0,
    muted BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (session_id, user_id)
);
CREATE TABLE session_attention_views (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    client_id UUID NOT NULL,
    session_id UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    expires_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (user_id, client_id)
);
CREATE INDEX session_attention_views_session ON session_attention_views (session_id, user_id, expires_at);
