-- A short-lived lease reconcile claims atomically before launching a session,
-- so concurrent reconciles / a heartbeat racing a launch can't double-spawn a
-- proxy. NULL or past = claimable; reconcile sets it to now()+TTL when it claims,
-- registration clears it. The TTL means a crashed-mid-launch claim self-expires
-- rather than wedging the session out of reconcile (the old in-memory
-- `pending_launch_sessions` had no TTL and could wedge permanently).
ALTER TABLE sessions ADD COLUMN launch_lease_until TIMESTAMP;
