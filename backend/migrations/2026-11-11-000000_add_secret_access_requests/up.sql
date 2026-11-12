CREATE TABLE secret_access_requests (
 id uuid PRIMARY KEY,
 owner_user_id uuid NOT NULL REFERENCES users(id),
 session_id uuid NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
 launcher_id uuid NOT NULL,
 proxy_token_id uuid NOT NULL REFERENCES proxy_auth_tokens(id) ON DELETE CASCADE,
 secret_id uuid NOT NULL REFERENCES secrets(id),
 version_id uuid NOT NULL,
 generation bigint NOT NULL,
 reference text NOT NULL,
 requested_version integer,
 env_name text NOT NULL,
 lifetime_seconds integer NOT NULL CHECK(lifetime_seconds BETWEEN 1 AND 3600),
 state text NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','approved','denied','expired','invalidated')),
 binding_id uuid REFERENCES secret_bindings(id) ON DELETE SET NULL,
 notice_message_id uuid REFERENCES messages(id) ON DELETE SET NULL,
 created_at timestamptz NOT NULL DEFAULT now(),
 expires_at timestamptz NOT NULL,
 FOREIGN KEY(secret_id,version_id) REFERENCES secret_versions(secret_id,id)
);
CREATE INDEX secret_access_requests_owner_created ON secret_access_requests(owner_user_id,created_at);
CREATE INDEX secret_access_requests_process_pending ON secret_access_requests(proxy_token_id,state,expires_at);
