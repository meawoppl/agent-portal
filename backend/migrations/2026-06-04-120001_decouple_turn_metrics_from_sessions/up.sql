-- Make `turn_metrics` a durable, per-user archive that outlives the session
-- that produced it. Previously `session_id` was `NOT NULL ... ON DELETE
-- CASCADE`, so the hourly session-age cleanup (and any explicit/launcher
-- session deletion) cascade-deleted the metrics — contradicting the table's
-- documented intent ("stick around indefinitely for long-horizon trend
-- analysis"). The Performance page then only ever showed as many days as the
-- user's sessions happened to survive.
--
-- NOTE on the -120001 version: this migration originally shipped as
-- 2026-06-04-120000, colliding with add_send_mode_to_pending_inputs. Diesel
-- keys migrations by version, so with the duplicate only one of the two ever
-- applied to a given database — a fresh database silently skipped this one
-- (no turn_metrics.user_id), while a database that migrated incrementally
-- before the collision landed has it. The rename gives it a unique version;
-- every statement below is idempotent so re-applying on an already-decoupled
-- database is a no-op, and a database that skipped it gets it for real.

-- 1. Owner column, backfilled from the session. Rows created while the old
--    cascade was in force all have a live session, so the backfill is total
--    and the NOT NULL tightening is safe.
ALTER TABLE turn_metrics ADD COLUMN IF NOT EXISTS user_id UUID REFERENCES users(id) ON DELETE CASCADE;
UPDATE turn_metrics tm SET user_id = s.user_id FROM sessions s WHERE s.id = tm.session_id AND tm.user_id IS NULL;
ALTER TABLE turn_metrics ALTER COLUMN user_id SET NOT NULL;

-- 2. Stop deleting metrics when their session is pruned: keep the row with a
--    NULL session_id instead of cascading the delete.
ALTER TABLE turn_metrics ALTER COLUMN session_id DROP NOT NULL;
ALTER TABLE turn_metrics DROP CONSTRAINT IF EXISTS turn_metrics_session_id_fkey;
ALTER TABLE turn_metrics
    ADD CONSTRAINT turn_metrics_session_id_fkey
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE SET NULL;

-- 3. Covering index for the per-user, time-ordered aggregation/recent queries.
CREATE INDEX IF NOT EXISTS idx_turn_metrics_user ON turn_metrics (user_id, started_at DESC);
