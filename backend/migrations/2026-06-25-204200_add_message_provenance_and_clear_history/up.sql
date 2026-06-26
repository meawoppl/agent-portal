ALTER TABLE messages
    ADD COLUMN provenance_kind VARCHAR(32),
    ADD COLUMN provenance_session_id UUID,
    ADD COLUMN provenance_agent_type VARCHAR(16);

-- Message rows are temporary portal display history. Clear existing transcript
-- rows so rendering can rely on record-level provenance for all future rows
-- instead of preserving legacy content-envelope detection. Agent CLI context is
-- stored outside this table and is unaffected. turn_metrics.user_message_id is
-- ON DELETE SET NULL, so durable metrics rows remain.
DELETE FROM messages;
