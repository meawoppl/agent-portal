-- Make `turn_metrics` a durable, per-user archive that outlives the session
-- that produced it. Previously `session_id` was `NOT NULL ... ON DELETE
-- CASCADE`, so the hourly session-age cleanup (and any explicit/launcher
-- session deletion) cascade-deleted the metrics — contradicting the table's
-- documented intent ("stick around indefinitely for long-horizon trend
-- analysis"). The Performance page then only ever showed as many days as the
-- user's sessions happened to survive.

-- 1. Owner column, backfilled from the session. Every existing row still has a
--    live session (the old cascade guaranteed it), so the backfill is total and
--    the NOT NULL tightening is safe.
ALTER TABLE turn_metrics ADD COLUMN user_id UUID REFERENCES users(id) ON DELETE CASCADE;
UPDATE turn_metrics tm SET user_id = s.user_id FROM sessions s WHERE s.id = tm.session_id;
ALTER TABLE turn_metrics ALTER COLUMN user_id SET NOT NULL;

-- 2. Stop deleting metrics when their session is pruned: keep the row with a
--    NULL session_id instead of cascading the delete.
ALTER TABLE turn_metrics ALTER COLUMN session_id DROP NOT NULL;
ALTER TABLE turn_metrics DROP CONSTRAINT turn_metrics_session_id_fkey;
ALTER TABLE turn_metrics
    ADD CONSTRAINT turn_metrics_session_id_fkey
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE SET NULL;

-- 3. Covering index for the per-user, time-ordered aggregation/recent queries.
CREATE INDEX idx_turn_metrics_user ON turn_metrics (user_id, started_at DESC);
