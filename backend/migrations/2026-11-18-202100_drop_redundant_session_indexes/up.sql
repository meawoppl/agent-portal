-- Existing composite indexes cover these leading-column lookups.
-- Keep idx_messages_session_id for frequently scanned whole-session reads.
DROP INDEX IF EXISTS idx_messages_user_id;
DROP INDEX IF EXISTS idx_pending_inputs_session_id;
DROP INDEX IF EXISTS idx_pending_inputs_session_seq;
DROP INDEX IF EXISTS idx_session_members_session;
