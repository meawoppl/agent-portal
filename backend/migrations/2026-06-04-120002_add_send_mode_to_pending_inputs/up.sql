-- NOTE on the -120002 version: this migration originally shipped as
-- 2026-06-04-120000, colliding with decouple_turn_metrics_from_sessions (see
-- that migration's header for the full story). Idempotent so it no-ops on a
-- database that already applied it under the old version.
ALTER TABLE pending_inputs
ADD COLUMN IF NOT EXISTS send_mode VARCHAR(32);
