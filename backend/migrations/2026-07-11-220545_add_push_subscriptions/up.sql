-- Push notification subscriptions (mobile-apps plan §8.3).
-- One row per (user, endpoint/token). `platform` is 'webpush' | 'apns' | 'fcm';
-- webpush rows carry `p256dh`/`auth`, native rows leave them NULL. Dead
-- endpoints are marked with `disabled_at` (pruned on 404/410) rather than
-- deleted, so re-registration can revive them by clearing the timestamp.
CREATE TABLE push_subscriptions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    platform VARCHAR NOT NULL,
    endpoint_or_token TEXT NOT NULL,
    p256dh TEXT,
    auth TEXT,
    device_label VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_success_at TIMESTAMPTZ,
    disabled_at TIMESTAMPTZ,
    UNIQUE (user_id, endpoint_or_token)
);

CREATE INDEX idx_push_subscriptions_user_id ON push_subscriptions (user_id);
