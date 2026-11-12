-- A cached handle is display metadata, never an account identifier. Old rows
-- lack a current provider verification assertion and must not authorize email
-- selectors until the person signs in through that provider again.
ALTER TABLE user_identities
    ADD COLUMN handle VARCHAR(255),
    ADD COLUMN email_verified BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN last_seen_at TIMESTAMP;
CREATE INDEX user_identities_verified_email_lookup
    ON user_identities (lower(email)) WHERE email_verified;
CREATE INDEX user_identities_provider_handle_lookup
    ON user_identities (provider, lower(handle)) WHERE handle IS NOT NULL;
