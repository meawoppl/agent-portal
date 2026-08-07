-- This file should undo anything in `up.sql`
DROP INDEX IF EXISTS sessions_forked_from_session_id_idx;
ALTER TABLE sessions
    DROP COLUMN IF EXISTS fork_create_worktree,
    DROP COLUMN IF EXISTS fork_launch_pending,
    DROP COLUMN IF EXISTS fork_point_turn_id,
    DROP COLUMN IF EXISTS forked_from_session_id;
