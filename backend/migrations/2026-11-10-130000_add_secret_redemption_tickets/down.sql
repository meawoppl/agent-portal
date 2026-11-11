ALTER TABLE secret_mutations DROP COLUMN binding_id;
DROP TABLE secret_redemption_tickets;
ALTER TABLE secret_bindings DROP CONSTRAINT secret_bindings_session_id_fkey;
ALTER TABLE secret_bindings ADD CONSTRAINT secret_bindings_session_id_fkey FOREIGN KEY(session_id) REFERENCES sessions(id);
ALTER TABLE secret_bindings DROP CONSTRAINT secret_binding_version;
ALTER TABLE secret_bindings DROP COLUMN env_name, DROP COLUMN version_id;
