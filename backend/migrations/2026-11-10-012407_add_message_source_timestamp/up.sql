ALTER TABLE messages ADD COLUMN source_created_at TIMESTAMP;
COMMENT ON COLUMN messages.source_created_at IS
    'Optional original event clock for display; created_at remains the server replay and retention clock';
