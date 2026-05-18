-- Records which agent's wire format the message's `content` JSON came from,
-- so readers can pick the right typed deserializer instead of guessing
-- (claude → `shared::ClaudeOutput`, codex → codex-wrapped envelope).
ALTER TABLE messages ADD COLUMN agent_type VARCHAR(16);

-- Backfill from the parent session — every existing message came from a
-- session whose agent_type is the canonical answer.
UPDATE messages
SET agent_type = sessions.agent_type
FROM sessions
WHERE messages.session_id = sessions.id;

-- Defensive: any orphans (shouldn't exist — messages.session_id has ON DELETE
-- CASCADE) default to 'claude'.
UPDATE messages SET agent_type = 'claude' WHERE agent_type IS NULL;

ALTER TABLE messages ALTER COLUMN agent_type SET NOT NULL;
ALTER TABLE messages ALTER COLUMN agent_type SET DEFAULT 'claude';
