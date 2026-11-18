-- Match the latest-per-session query's equality filter and DISTINCT ON order.
CREATE INDEX idx_turn_metrics_user_session_started
    ON turn_metrics (user_id, session_id, started_at DESC);
