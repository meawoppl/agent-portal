-- This file should undo anything in `up.sql`
DROP INDEX IF EXISTS idx_sessions_last_messaged_at;
ALTER TABLE sessions DROP COLUMN last_messaged_at;
