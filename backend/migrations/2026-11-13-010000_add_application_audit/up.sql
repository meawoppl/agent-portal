-- No foreign keys: deleting an account or secret must not delete its history.
CREATE TABLE audit_events (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    occurred_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    category text NOT NULL CHECK (category IN ('identity', 'computers', 'secrets')),
    event_type text NOT NULL,
    actor_kind text NOT NULL DEFAULT 'user' CHECK (actor_kind IN ('user', 'agent', 'system', 'unknown')),
    actor_user_id uuid,
    owner_user_id uuid,
    session_id uuid,
    launcher_id uuid,
    target_kind text NOT NULL,
    target_id uuid,
    operation_id uuid,
    outcome text NOT NULL,
    details jsonb NOT NULL DEFAULT '{}'::jsonb,
    schema_version integer NOT NULL DEFAULT 1
);
CREATE INDEX audit_events_time ON audit_events (occurred_at DESC, id DESC);
CREATE INDEX audit_events_category_time ON audit_events (category, occurred_at DESC, id DESC);
CREATE INDEX audit_events_actor_time ON audit_events (actor_user_id, occurred_at DESC, id DESC);
CREATE INDEX audit_events_operation ON audit_events (operation_id) WHERE operation_id IS NOT NULL;
INSERT INTO audit_events (id, occurred_at, category, event_type, actor_user_id, target_kind, target_id, outcome)
SELECT id, created_at, 'secrets', 'secret.' || action, actor_user_id, 'secret', secret_id, outcome
FROM secret_audit_events;

-- State changes also occur in sweeps and cascade revocations. These small,
-- explicit triggers record only identifiers and state, never row JSON/payloads.
CREATE FUNCTION audit_secret_transition() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_TABLE_NAME = 'secret_bindings' THEN
        IF OLD.revoked_at IS NULL AND NEW.revoked_at IS NOT NULL THEN
            INSERT INTO audit_events(category,event_type,actor_kind,owner_user_id,session_id,launcher_id,target_kind,target_id,operation_id,outcome)
            VALUES ('secrets','secret.binding.invalidated','system',NEW.created_by,NEW.session_id,NEW.launcher_id,'secret',NEW.secret_id,NEW.id,'revoked');
        END IF;
    ELSIF TG_TABLE_NAME = 'secret_expunge_jobs' THEN
        IF OLD.state IS DISTINCT FROM NEW.state THEN
            INSERT INTO audit_events(category,event_type,actor_kind,owner_user_id,target_kind,target_id,operation_id,outcome)
            VALUES ('secrets','secret.cleanup.transition','system',NEW.requested_by,'secret',NEW.secret_id,NEW.id,NEW.state);
        END IF;
    ELSE
        IF OLD.state IS DISTINCT FROM NEW.state AND NEW.state IN ('expired','invalidated') THEN
            INSERT INTO audit_events(category,event_type,actor_kind,owner_user_id,session_id,launcher_id,target_kind,target_id,operation_id,outcome)
            VALUES ('secrets',CASE WHEN TG_TABLE_NAME = 'secret_deposit_proposals' THEN 'secret.set.' ELSE 'secret.access.' END || NEW.state,'system',NEW.owner_user_id,NEW.session_id,NEW.launcher_id,'request',NEW.id,NEW.id,NEW.state);
        END IF;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER audit_binding_transition AFTER UPDATE ON secret_bindings FOR EACH ROW EXECUTE FUNCTION audit_secret_transition();
CREATE TRIGGER audit_access_transition AFTER UPDATE ON secret_access_requests FOR EACH ROW EXECUTE FUNCTION audit_secret_transition();
CREATE TRIGGER audit_deposit_transition AFTER UPDATE ON secret_deposit_proposals FOR EACH ROW EXECUTE FUNCTION audit_secret_transition();
CREATE TRIGGER audit_cleanup_transition AFTER UPDATE ON secret_expunge_jobs FOR EACH ROW EXECUTE FUNCTION audit_secret_transition();

-- Identity mutations are atomic with their evidence, including unlink cascades.
CREATE FUNCTION audit_identity_transition() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        INSERT INTO audit_events(category,event_type,actor_kind,owner_user_id,target_kind,target_id,outcome,details)
        VALUES ('identity','identity.linked','system',NEW.user_id,'identity',NEW.id,'success',jsonb_build_object('provider',NEW.provider));
        RETURN NEW;
    END IF;
    INSERT INTO audit_events(category,event_type,actor_kind,owner_user_id,target_kind,target_id,outcome,details)
    VALUES ('identity','identity.unlinked','system',OLD.user_id,'identity',OLD.id,'success',jsonb_build_object('provider',OLD.provider));
    RETURN OLD;
END $$;
CREATE TRIGGER audit_identity_transition AFTER INSERT OR DELETE ON user_identities FOR EACH ROW EXECUTE FUNCTION audit_identity_transition();

CREATE FUNCTION audit_token_transition() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE action text;
BEGIN
    IF TG_OP = 'INSERT' THEN action := 'credential.issued';
    ELSIF NOT OLD.revoked AND NEW.revoked THEN action := 'credential.revoked';
    ELSIF OLD.token_hash IS DISTINCT FROM NEW.token_hash THEN action := 'credential.renewed';
    ELSE RETURN NEW;
    END IF;
    INSERT INTO audit_events(category,event_type,actor_kind,owner_user_id,target_kind,target_id,outcome)
    VALUES ('identity',action,'system',NEW.user_id,'credential',NEW.id,'success');
    RETURN NEW;
END $$;
CREATE TRIGGER audit_token_transition AFTER INSERT OR UPDATE ON proxy_auth_tokens FOR EACH ROW EXECUTE FUNCTION audit_token_transition();
CREATE INDEX secret_audit_events_time ON secret_audit_events (created_at);

-- Grant history already records the actual author and recipient. Project only
-- permission bits, not secret metadata or values, into the common audit trail.
CREATE FUNCTION audit_secret_sharing() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO audit_events(category,event_type,actor_user_id,target_kind,target_id,outcome,details)
    VALUES ('secrets','secret.sharing.' || NEW.action,NEW.actor_user_id,'secret',NEW.secret_id,'success',
        jsonb_build_object('grantee_user_id',NEW.grantee_user_id,'can_read',NEW.can_read,'can_use',NEW.can_use,'can_write',NEW.can_write,'can_manage',NEW.can_manage));
    RETURN NEW;
END $$;
CREATE TRIGGER audit_secret_sharing AFTER INSERT ON secret_acl_history FOR EACH ROW EXECUTE FUNCTION audit_secret_sharing();
