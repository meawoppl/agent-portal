-- Admin-assigned custom subdomain labels (docs/PORT_FORWARDING.md). An admin
-- can give a session's forward a human-readable alias (e.g. `myapp`) that
-- routes alongside the auto 8-hex `forward_subdomains` label — both resolve to
-- the same session. One custom label per session; the `label` PK plus the
-- deconfliction in the handler (valid DNS label, not 8-hex, not reserved,
-- unique) keep the namespace collision-free. Cascade-deleted with the session.
CREATE TABLE custom_subdomains (
    label TEXT PRIMARY KEY,
    session_id UUID NOT NULL UNIQUE REFERENCES sessions(id) ON DELETE CASCADE,
    created_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMP NOT NULL DEFAULT now()
);
