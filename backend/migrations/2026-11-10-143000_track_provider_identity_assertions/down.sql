DROP INDEX user_identities_provider_handle_lookup;
DROP INDEX user_identities_verified_email_lookup;
ALTER TABLE user_identities DROP COLUMN last_seen_at, DROP COLUMN email_verified, DROP COLUMN handle;
