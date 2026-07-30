-- The model's context window in tokens, as reported by the agent (Codex sends
-- it on the wire; Claude does not, so claude rows stay NULL and the window is
-- derived from the model id at read time). Powers the context-usage gauge.
ALTER TABLE turn_metrics ADD COLUMN model_context_window BIGINT;
