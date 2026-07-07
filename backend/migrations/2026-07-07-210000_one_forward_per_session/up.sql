-- One forward per session, with a stable session-scoped subdomain
-- (docs/PORT_FORWARDING.md). The public subdomain now identifies the SESSION,
-- not the port, so an agent fronts multiple services behind its own reverse
-- proxy on the single forwarded port.

-- Forwarding has not shipped to production, so clearing these session-scoped
-- ephemeral rows is safe — live agents re-register on next `agent-portal
-- forward`. (Avoids backfilling a subdomain for pre-existing rows.)
DELETE FROM session_forwards;

-- At most one forward per session (was UNIQUE(session_id, port)).
ALTER TABLE session_forwards DROP CONSTRAINT session_forwards_session_id_port_key;
ALTER TABLE session_forwards ADD CONSTRAINT session_forwards_session_id_key UNIQUE (session_id);

-- Subdomain label lookup table. A short 8-hex label is allocated per session
-- on first forward — derived from sha256(session_id), re-derived with a counter
-- on the (rare) collision so short labels stay collision-free — and stored here
-- so it maps a Host-header label back to its session and stays stable across
-- close/reopen. Cascade-deleted with the session.
CREATE TABLE forward_subdomains (
    label TEXT PRIMARY KEY,
    session_id UUID NOT NULL UNIQUE REFERENCES sessions(id) ON DELETE CASCADE,
    created_at TIMESTAMP NOT NULL DEFAULT now()
);
