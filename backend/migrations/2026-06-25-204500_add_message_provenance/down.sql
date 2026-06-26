-- NOTE: the deleted message rows cannot be restored (history was disposable by
-- design). This only reverses the schema change.
ALTER TABLE messages DROP COLUMN provenance_agent_type;
ALTER TABLE messages DROP COLUMN provenance_session_id;
ALTER TABLE messages DROP COLUMN provenance_kind;
