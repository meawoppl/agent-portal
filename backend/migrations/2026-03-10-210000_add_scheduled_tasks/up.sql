CREATE TABLE scheduled_tasks (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id         UUID NOT NULL REFERENCES users(id),
    name            VARCHAR(255) NOT NULL,
    cron_expression VARCHAR(128) NOT NULL,
    timezone        VARCHAR(64) NOT NULL DEFAULT 'UTC',
    hostname        VARCHAR(255),
    working_directory TEXT NOT NULL,
    prompt          TEXT NOT NULL,
    claude_args     JSONB NOT NULL DEFAULT '[]',
    agent_type      VARCHAR(16) NOT NULL DEFAULT 'claude',
    enabled         BOOLEAN NOT NULL DEFAULT true,
    max_runtime_minutes INTEGER NOT NULL DEFAULT 30,
    last_session_id UUID REFERENCES sessions(id) ON DELETE SET NULL,
    last_run_at     TIMESTAMP,
    next_run_at     TIMESTAMP,
    created_at      TIMESTAMP NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_scheduled_tasks_user_id ON scheduled_tasks(user_id);

ALTER TABLE sessions ADD COLUMN scheduled_task_id UUID REFERENCES scheduled_tasks(id) ON DELETE SET NULL;
