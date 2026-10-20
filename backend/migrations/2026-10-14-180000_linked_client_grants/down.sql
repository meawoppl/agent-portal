-- Never turn scoped credentials into unrestricted credentials on rollback.
DELETE FROM proxy_auth_tokens WHERE linked_grant_id IS NOT NULL;
ALTER TABLE proxy_auth_tokens DROP CONSTRAINT linked_token_owner, DROP CONSTRAINT linked_token_scope,
    DROP COLUMN linked_grant_id, DROP COLUMN scope;
DROP TABLE linked_client_grants;
DROP TABLE federation_policy;
