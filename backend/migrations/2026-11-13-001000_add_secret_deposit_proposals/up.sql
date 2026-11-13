CREATE TABLE secret_deposit_proposals (
 id uuid PRIMARY KEY,
 owner_user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 session_id uuid NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
 launcher_id uuid NOT NULL,
 proxy_token_id uuid NOT NULL,
 organization_id uuid NOT NULL,
 state text NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','approved','denied','expired')),
 metadata jsonb NOT NULL,
 envelopes jsonb,
 created_at timestamptz NOT NULL DEFAULT now(),
 expires_at timestamptz NOT NULL DEFAULT (now() + interval '10 minutes'),
 CHECK ((state = 'pending') = (envelopes IS NOT NULL))
);
CREATE INDEX secret_deposit_pending_expiry ON secret_deposit_proposals (expires_at) WHERE state = 'pending';
CREATE INDEX secret_deposit_owner_created ON secret_deposit_proposals (owner_user_id, created_at);
