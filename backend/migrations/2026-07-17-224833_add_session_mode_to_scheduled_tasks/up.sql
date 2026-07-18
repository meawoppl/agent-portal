-- Per-task session mode: 'fresh' (default, a brand-new session each run) or
-- 'continue' (each firing resumes the same conversation, accumulating context).
ALTER TABLE scheduled_tasks
    ADD COLUMN session_mode VARCHAR(16) NOT NULL DEFAULT 'fresh';
