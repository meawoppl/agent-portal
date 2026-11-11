CREATE TABLE secret_acl_history (
 id uuid PRIMARY KEY, secret_id uuid NOT NULL REFERENCES secrets(id),
 actor_user_id uuid NOT NULL REFERENCES users(id), grantee_user_id uuid NOT NULL REFERENCES users(id),
 action text NOT NULL CHECK(action IN ('grant','revoke')), generation bigint NOT NULL,
 can_read boolean NOT NULL, can_use boolean NOT NULL, can_write boolean NOT NULL, can_manage boolean NOT NULL,
 created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX secret_acl_history_target ON secret_acl_history(secret_id,created_at);
CREATE FUNCTION secret_acl_history_immutable() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'secret ACL history is immutable'; END $$;
CREATE TRIGGER secret_acl_history_immutable BEFORE UPDATE OR DELETE ON secret_acl_history FOR EACH ROW EXECUTE FUNCTION secret_acl_history_immutable();
