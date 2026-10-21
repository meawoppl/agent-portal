DROP TABLE session_spawn_requests;
DROP TABLE session_spawn_policies;
DROP INDEX sessions_spawned_by_idx;
ALTER TABLE sessions
    DROP COLUMN spawn_stop_on_parent_end,
    DROP COLUMN spawn_idle_timeout_seconds,
    DROP COLUMN spawn_depth,
    DROP COLUMN spawned_by;
