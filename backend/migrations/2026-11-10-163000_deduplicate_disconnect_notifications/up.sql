-- A reconnect does not create new work. Keep the notification watermark
-- durable so a crash loop or backend restart cannot re-alert for the same input.
CREATE TABLE session_disconnect_notifications (
    session_id UUID PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    input_seq BIGINT NOT NULL
);
