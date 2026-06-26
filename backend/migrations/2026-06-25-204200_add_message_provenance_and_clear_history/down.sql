-- This drops the provenance columns but cannot restore the temporary message
-- history deleted by up.sql.
ALTER TABLE messages
    DROP COLUMN provenance_agent_type,
    DROP COLUMN provenance_session_id,
    DROP COLUMN provenance_kind;
