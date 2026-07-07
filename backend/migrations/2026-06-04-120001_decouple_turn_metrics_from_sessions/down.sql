-- Reverse: re-couple turn_metrics to sessions (cascade delete) and drop the
-- owner column. Rows orphaned from a deleted session can't satisfy the restored
-- NOT NULL, so drop them first (their session is already gone).
DROP INDEX IF EXISTS idx_turn_metrics_user;

DELETE FROM turn_metrics WHERE session_id IS NULL;
ALTER TABLE turn_metrics DROP CONSTRAINT turn_metrics_session_id_fkey;
ALTER TABLE turn_metrics ALTER COLUMN session_id SET NOT NULL;
ALTER TABLE turn_metrics
    ADD CONSTRAINT turn_metrics_session_id_fkey
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE;

ALTER TABLE turn_metrics DROP COLUMN user_id;
