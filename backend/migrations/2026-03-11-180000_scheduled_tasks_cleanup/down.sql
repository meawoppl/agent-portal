ALTER TABLE scheduled_tasks ADD COLUMN next_run_at TIMESTAMP;
ALTER TABLE scheduled_tasks ALTER COLUMN hostname DROP NOT NULL;
