-- Make hostname required (tasks must be pinned to a launcher)
-- Set any existing NULL hostnames to empty string before adding constraint
UPDATE scheduled_tasks SET hostname = '' WHERE hostname IS NULL;
ALTER TABLE scheduled_tasks ALTER COLUMN hostname SET NOT NULL;

-- Drop next_run_at (computed on-demand from cron_expression + timezone)
ALTER TABLE scheduled_tasks DROP COLUMN next_run_at;
