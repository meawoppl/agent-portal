DROP INDEX IF EXISTS idx_sessions_paused;

ALTER TABLE sessions
    DROP COLUMN claude_args,
    DROP COLUMN paused;
