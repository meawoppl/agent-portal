-- UUID/timestamps are not insertion cursors. Serialize each session's inserts
-- with this row lock, held until commit, so a reader cannot skip a late commit.
CREATE TABLE session_follow_heads (
    session_id UUID PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    sequence BIGINT NOT NULL DEFAULT 0
);
CREATE TABLE session_follow_messages (
    session_id UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    sequence BIGINT NOT NULL,
    message_id UUID NOT NULL UNIQUE,
    PRIMARY KEY (session_id, sequence)
);
-- Prevent inserts between the backfill snapshot and trigger installation.
LOCK TABLE messages IN SHARE ROW EXCLUSIVE MODE;
-- Keep only identifiers after transcript retention, so a missing body is a
-- detectable gap and an archived history download can use the same cursor.
INSERT INTO session_follow_messages
SELECT session_id, row_number() OVER (PARTITION BY session_id ORDER BY created_at, id), id FROM messages;
INSERT INTO session_follow_heads SELECT session_id, max(sequence) FROM session_follow_messages GROUP BY session_id;
CREATE FUNCTION assign_follow_cursor() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE next_sequence BIGINT;
BEGIN
    INSERT INTO session_follow_heads(session_id) VALUES (NEW.session_id) ON CONFLICT DO NOTHING;
    PERFORM 1 FROM session_follow_heads WHERE session_id = NEW.session_id FOR UPDATE;
    IF EXISTS (SELECT 1 FROM session_follow_messages WHERE message_id = NEW.id) THEN RETURN NEW; END IF;
    UPDATE session_follow_heads SET sequence = sequence + 1 WHERE session_id = NEW.session_id RETURNING sequence INTO next_sequence;
    INSERT INTO session_follow_messages VALUES (NEW.session_id, next_sequence, NEW.id);
    RETURN NEW;
END $$;
CREATE TRIGGER messages_follow_cursor AFTER INSERT ON messages FOR EACH ROW EXECUTE FUNCTION assign_follow_cursor();
