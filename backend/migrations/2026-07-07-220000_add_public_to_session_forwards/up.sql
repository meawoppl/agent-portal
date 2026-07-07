-- Per-forward public flag (docs/PORT_FORWARDING.md). When true, the
-- forward-origin skips the token-handoff auth and serves the subdomain to
-- anyone with the URL — an opt-in ngrok-style public share, owner-toggled from
-- the Settings ▸ Forwarding tab. Default private.
ALTER TABLE session_forwards ADD COLUMN public BOOLEAN NOT NULL DEFAULT false;
