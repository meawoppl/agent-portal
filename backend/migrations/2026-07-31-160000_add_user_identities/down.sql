DROP INDEX IF EXISTS idx_users_email_lower;

-- Restore the Google-only identity column from the identities table. Users
-- whose only identity is non-Google cannot be represented, so they are dropped
-- (they could not have existed before this migration).
ALTER TABLE users ADD COLUMN google_id VARCHAR(255);

UPDATE users u
SET google_id = i.subject
FROM user_identities i
WHERE i.user_id = u.id AND i.provider = 'google';

DELETE FROM users WHERE google_id IS NULL;

ALTER TABLE users ALTER COLUMN google_id SET NOT NULL;
ALTER TABLE users ADD CONSTRAINT users_google_id_key UNIQUE (google_id);

DROP TABLE user_identities;
