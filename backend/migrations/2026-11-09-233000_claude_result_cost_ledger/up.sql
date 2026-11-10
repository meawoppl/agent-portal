-- Keep result identities for the session lifetime, even after transcript
-- retention, so replay cannot increment spending twice after backend restart.
CREATE TABLE claude_result_cost_ledger (
    session_id UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    result_uuid TEXT NOT NULL,
    PRIMARY KEY (session_id, result_uuid)
);

-- Existing totals have already seen hot results. Mark them accounted without
-- changing historical costs; replay across deployment must not charge them again.
-- Parse row-by-row so a malformed historical payload cannot prevent startup.
DO $$
DECLARE
    item RECORD;
    payload JSONB;
BEGIN
    FOR item IN SELECT session_id, content FROM messages
        WHERE role = 'result' AND agent_type = 'claude'
    LOOP
        BEGIN
            payload := item.content::jsonb;
        EXCEPTION WHEN invalid_text_representation THEN
            CONTINUE;
        END;
        IF jsonb_typeof(payload->'uuid') = 'string'
            AND btrim(payload->>'uuid') <> '' THEN
            INSERT INTO claude_result_cost_ledger (session_id, result_uuid)
            VALUES (item.session_id, payload->>'uuid')
            ON CONFLICT DO NOTHING;
        END IF;
    END LOOP;
END $$;
