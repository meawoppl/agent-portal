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

-- After the join-backfill, every row must have an agent_type. If the NOT NULL
-- alter fails, you have orphan messages whose session was deleted without
-- cascading — investigate before re-running. We deliberately do NOT set a
-- column DEFAULT here: every callsite must specify agent_type explicitly so
-- future schema changes can't silently inherit a claude default for codex
-- sessions.
ALTER TABLE messages ALTER COLUMN agent_type SET NOT NULL;
