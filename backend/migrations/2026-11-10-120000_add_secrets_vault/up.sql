CREATE TABLE secrets_generation (
 id smallint PRIMARY KEY DEFAULT 1 CHECK(id=1), organization_id uuid NOT NULL UNIQUE,
 generation bigint NOT NULL DEFAULT 0
);
INSERT INTO secrets_generation(id,organization_id) VALUES(1,gen_random_uuid());
CREATE TABLE secrets (
 id uuid PRIMARY KEY, organization_id uuid NOT NULL, owner_user_id uuid NOT NULL REFERENCES users(id),
 namespace text NOT NULL CHECK(namespace IN ('personal','organization')), name text NOT NULL,
 description text NOT NULL DEFAULT '', current_version_id uuid,
 system_tags text[] NOT NULL DEFAULT '{}', deleted_at timestamptz, deleted_by uuid REFERENCES users(id),
 acl_generation bigint NOT NULL DEFAULT 1, created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now(),
 CHECK ((deleted_at IS NOT NULL) = ('deleted'=ANY(system_tags)))
);
CREATE UNIQUE INDEX secrets_personal_name ON secrets(owner_user_id,name) WHERE namespace='personal';
CREATE UNIQUE INDEX secrets_organization_name ON secrets(organization_id,name) WHERE namespace='organization';
CREATE TABLE secret_versions (
 id uuid PRIMARY KEY, secret_id uuid NOT NULL REFERENCES secrets(id), ordinal integer NOT NULL CHECK(ordinal>0),
 encrypted_value bytea NOT NULL, nonce bytea NOT NULL CHECK(octet_length(nonce)=12), auth_tag bytea NOT NULL CHECK(octet_length(auth_tag)=16),
 wrapped_dek bytea NOT NULL, wrapping_key_id text NOT NULL, crypto_format integer NOT NULL CHECK(crypto_format=1),
 created_by uuid NOT NULL REFERENCES users(id), created_at timestamptz NOT NULL DEFAULT now(),
 UNIQUE(secret_id,ordinal), UNIQUE(secret_id,id)
);
ALTER TABLE secrets ADD CONSTRAINT secrets_current_version FOREIGN KEY(id,current_version_id) REFERENCES secret_versions(secret_id,id) DEFERRABLE INITIALLY DEFERRED;
-- Vault history is immutable even if application code accidentally attempts a replacement.
CREATE FUNCTION secret_version_immutable() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'vault versions are immutable'; END $$;
CREATE TRIGGER secret_versions_immutable BEFORE UPDATE OR DELETE ON secret_versions FOR EACH ROW EXECUTE FUNCTION secret_version_immutable();
CREATE TABLE secret_grants (
 secret_id uuid NOT NULL REFERENCES secrets(id), grantee_user_id uuid NOT NULL REFERENCES users(id),
 can_read boolean NOT NULL DEFAULT false, can_use boolean NOT NULL DEFAULT false, can_write boolean NOT NULL DEFAULT false, can_manage boolean NOT NULL DEFAULT false,
 expires_at timestamptz, generation bigint NOT NULL, PRIMARY KEY(secret_id,grantee_user_id)
);
CREATE TABLE secret_credentials (
 id uuid PRIMARY KEY, user_id uuid NOT NULL REFERENCES users(id), token_hash text NOT NULL UNIQUE, label text NOT NULL,
 metadata_scope boolean NOT NULL, mutation_scope boolean NOT NULL, read_scope boolean NOT NULL,
 expires_at timestamptz NOT NULL, revoked_at timestamptz, created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE secret_enrollments (
 id uuid PRIMARY KEY, user_id uuid NOT NULL REFERENCES users(id), device_hash text NOT NULL UNIQUE, user_code text NOT NULL UNIQUE,
 label text NOT NULL, state text NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','approved','denied','consumed')),
 expires_at timestamptz NOT NULL, created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE secret_audit_events (
 id uuid PRIMARY KEY, actor_user_id uuid REFERENCES users(id), secret_id uuid REFERENCES secrets(id),
 action text NOT NULL, outcome text NOT NULL, created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE secret_bindings (
 id uuid PRIMARY KEY, secret_id uuid NOT NULL REFERENCES secrets(id), session_id uuid NOT NULL REFERENCES sessions(id),
 launcher_id uuid NOT NULL, created_by uuid NOT NULL REFERENCES users(id), operation text NOT NULL,
 expires_at timestamptz NOT NULL, generation bigint NOT NULL, revoked_at timestamptz, created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE secret_outbox (
 id uuid PRIMARY KEY, kind text NOT NULL, target_id uuid NOT NULL, generation bigint NOT NULL,
 payload jsonb NOT NULL DEFAULT '{}', processed_at timestamptz, created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE secret_expunge_jobs (
 id uuid PRIMARY KEY, organization_id uuid NOT NULL, secret_id uuid NOT NULL REFERENCES secrets(id), requested_by uuid NOT NULL REFERENCES users(id),
 state text NOT NULL DEFAULT 'queued', generation bigint NOT NULL, checkpoint jsonb NOT NULL DEFAULT '{}', receipt jsonb NOT NULL DEFAULT '{}',
 created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE secret_occurrences (
 rule_version_id uuid NOT NULL REFERENCES secret_versions(id), record_kind text NOT NULL, record_id text NOT NULL,
 archive_generation bigint NOT NULL DEFAULT 0, PRIMARY KEY(rule_version_id,record_kind,record_id)
);
CREATE TABLE secret_coverage (
 object_id text PRIMARY KEY, rule_generation bigint NOT NULL, object_generation text NOT NULL
);

CREATE TABLE secret_mutations (
 actor_user_id uuid NOT NULL REFERENCES users(id), request_id uuid NOT NULL, path text NOT NULL, secret_id uuid NOT NULL REFERENCES secrets(id),
 created_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY(actor_user_id,request_id)
);
