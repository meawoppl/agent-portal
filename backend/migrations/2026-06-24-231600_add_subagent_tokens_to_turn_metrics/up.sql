-- Subagent (Task/sidechain) token rollup, mirroring the Claude binary's
-- distinct `subagent_tokens` line in its result `<usage>` envelope. Defaults
-- to 0; the Claude stream-json protocol does not yet surface this (see the
-- upstream claude-codes SDK gap), so claude turns persist 0 until it does.
ALTER TABLE turn_metrics ADD COLUMN subagent_tokens BIGINT NOT NULL DEFAULT 0;
