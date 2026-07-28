-- #1489: tie a session-bound proxy auth token's lifetime to its session.
--
-- `proxy_auth_tokens.session_id` had no foreign key, so a token whose session
-- was deleted still verified successfully (verification looks the row up by
-- `token_hash` alone). The only thing preventing a live credential pointing at
-- a deleted session was every deletion path remembering to call
-- `revoke_tokens_for_session` — a convention, not a constraint.

-- Clean up existing orphans first: the FK below rejects rows whose session no
-- longer exists. NULL session_id (user-scoped / unbound launch tokens) is left
-- untouched — those are legitimately session-less.
DELETE FROM proxy_auth_tokens
WHERE session_id IS NOT NULL
  AND session_id NOT IN (SELECT id FROM sessions);

-- ON DELETE CASCADE (not SET NULL): a session-scoped credential must die with
-- its session, never silently widen into a user-scoped one.
ALTER TABLE proxy_auth_tokens
    ADD CONSTRAINT proxy_auth_tokens_session_id_fkey
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE;
