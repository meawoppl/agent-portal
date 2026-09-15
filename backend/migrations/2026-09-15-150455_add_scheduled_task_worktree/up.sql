ALTER TABLE scheduled_tasks
    ADD COLUMN worktree_mode VARCHAR(16) NOT NULL DEFAULT 'none',
    ADD COLUMN worktree_branch VARCHAR(255),
    ADD COLUMN session_name VARCHAR(255);
