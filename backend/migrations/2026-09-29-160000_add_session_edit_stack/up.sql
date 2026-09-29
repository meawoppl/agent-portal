CREATE TABLE session_edit_stack_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    created_by UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    body TEXT NOT NULL DEFAULT '',
    source JSONB,
    context JSONB,
    image_data_url TEXT,
    status VARCHAR(32) NOT NULL DEFAULT 'pending',
    sent_client_msg_id UUID,
    sent_at TIMESTAMP,
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_session_edit_stack_session_created
    ON session_edit_stack_items(session_id, created_at);

CREATE INDEX idx_session_edit_stack_session_status
    ON session_edit_stack_items(session_id, status);
