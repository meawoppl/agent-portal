-- This portal's stable identity for federation discovery. Exactly one row:
-- the boolean primary key can only ever be TRUE.
CREATE TABLE portal_instance (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    instance_id UUID NOT NULL DEFAULT gen_random_uuid(),
    created_at TIMESTAMP NOT NULL DEFAULT NOW()
);

INSERT INTO portal_instance DEFAULT VALUES;
