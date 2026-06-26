-- Inter-agent (and future) message provenance as typed record metadata, so
-- rendering branches on metadata instead of parsing the message body. See the
-- MessageOrigin design: `provenance_kind` is the discriminant (NULL = derive
-- from `role`), and the two attribution columns are the inter-agent payload.
ALTER TABLE messages ADD COLUMN provenance_kind VARCHAR;
ALTER TABLE messages ADD COLUMN provenance_session_id UUID;
ALTER TABLE messages ADD COLUMN provenance_agent_type VARCHAR;

-- Message history is intentionally disposable. Rather than risk a brittle,
-- irreversible in-place content rewrite to backfill provenance onto old rows
-- (two legacy formats: the pre-typed `[message from ...]` text envelope and the
-- transitional PortalContent::AgentMessage JSON), we drop existing rows. New
-- rows carry provenance natively, so the frontend can render inter-agent
-- messages purely from MessageOrigin and delete all legacy content parsing.
--
-- IMPACT: every session's portal web transcript backscroll is cleared. The
-- agent CLI processes are unaffected (they keep their own transcripts + full
-- context); only the portal's replayable display resets.
--
-- `turn_metrics.user_message_id` is `ON DELETE SET NULL`, so the durable
-- per-user metrics archive survives this delete with its message links nulled.
DELETE FROM messages;
