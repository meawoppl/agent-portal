-- Long-term session archive bookkeeping (#1258 phase 1). NULL = never
-- archived; the periodic sweep re-archives when last_activity advances past
-- this (a session that reactivated after archival gets a fresh archive).
ALTER TABLE sessions ADD COLUMN archived_at TIMESTAMP;
