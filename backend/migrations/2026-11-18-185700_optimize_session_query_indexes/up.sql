-- Match the latest-per-session query's equality filter and DISTINCT ON order.
CREATE INDEX idx_turn_metrics_user_session_started
    ON turn_metrics (user_id, session_id, started_at DESC);

-- The leading columns of the remaining composite indexes cover these lookups.
DROP INDEX IF EXISTS idx_messages_session_id;
DROP INDEX IF EXISTS idx_messages_user_id;
DROP INDEX IF EXISTS idx_pending_inputs_session_id;
DROP INDEX IF EXISTS idx_pending_inputs_session_seq;
DROP INDEX IF EXISTS idx_session_members_session;
