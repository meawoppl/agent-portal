ALTER TABLE sessions
ADD COLUMN last_messaged_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP;

-- Existing sessions predate this field, so creation is their truthful
-- messaging-recency baseline.
UPDATE sessions SET last_messaged_at = created_at;

CREATE INDEX idx_sessions_last_messaged_at
ON sessions(last_messaged_at DESC);
