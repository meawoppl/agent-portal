ALTER TABLE scheduled_tasks
    DROP COLUMN session_name,
    DROP COLUMN worktree_branch,
    DROP COLUMN worktree_mode;
