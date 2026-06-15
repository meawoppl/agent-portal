-- Track launch failures separately from user-initiated pause so a transient
-- launch failure no longer permanently wedges a session in `paused`. See #1045.
ALTER TABLE sessions ADD COLUMN launch_failure_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN last_launch_attempt_at TIMESTAMP;
