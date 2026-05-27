-- Per-turn performance metrics for both Claude and Codex sessions.
--
-- One row per user-input → terminator (`ClaudeOutput::Result` for Claude,
-- `CodexEvent::TurnCompleted` / `TurnFailed` for Codex). The proxy ships a
-- typed `TurnMetricsReport` envelope when a turn finishes; the backend
-- writes it here verbatim and broadcasts the saved row to connected web
-- clients.
--
-- Retention note: this table is intentionally NOT wired into the
-- `MESSAGE_RETENTION_DAYS` cleanup job. Per-turn latency / cost rows are
-- small, useful for long-horizon trend analysis, and we want them retained
-- indefinitely independent of the chat-history retention sweep that
-- prunes `messages` rows.
CREATE TABLE turn_metrics (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    -- Nullable because `messages` rows can be retention-purged independently
    -- (per the chat-history sweep) while we keep the metric row forever.
    user_message_id UUID REFERENCES messages(id) ON DELETE SET NULL,

    agent_type TEXT NOT NULL,
    model TEXT,
    service_tier TEXT,

    started_at TIMESTAMPTZ NOT NULL,
    first_token_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,

    ttft_ms BIGINT,
    total_duration_ms BIGINT,
    generation_duration_ms BIGINT,
    max_inter_token_gap_ms BIGINT,

    input_tokens BIGINT NOT NULL DEFAULT 0,
    output_tokens BIGINT NOT NULL DEFAULT 0,
    cache_creation_tokens BIGINT NOT NULL DEFAULT 0,
    cache_read_tokens BIGINT NOT NULL DEFAULT 0,
    thinking_tokens BIGINT NOT NULL DEFAULT 0,

    stop_reason TEXT,
    is_error BOOLEAN NOT NULL DEFAULT FALSE,
    tool_call_count INTEGER NOT NULL DEFAULT 0,
    stream_restarts INTEGER NOT NULL DEFAULT 0,

    -- Nullable: codex does not surface per-turn cost on its wire today.
    total_cost_usd DOUBLE PRECISION,

    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_turn_metrics_session ON turn_metrics(session_id);
CREATE INDEX idx_turn_metrics_started_at ON turn_metrics(started_at DESC);
CREATE INDEX idx_turn_metrics_model ON turn_metrics(agent_type, model, service_tier, started_at DESC);
