CREATE INDEX idx_messages_session_id ON messages (session_id);
CREATE INDEX idx_messages_user_id ON messages (user_id);
CREATE INDEX idx_pending_inputs_session_id ON pending_inputs (session_id);
CREATE INDEX idx_pending_inputs_session_seq ON pending_inputs (session_id, seq_num);
CREATE INDEX idx_session_members_session ON session_members (session_id);

DROP INDEX IF EXISTS idx_turn_metrics_user_session_started;
