DROP TABLE secret_mutations;
DROP TABLE secret_coverage, secret_occurrences, secret_expunge_jobs, secret_outbox, secret_bindings, secret_audit_events, secret_enrollments, secret_credentials, secret_grants;
ALTER TABLE secrets DROP CONSTRAINT secrets_current_version;
DROP TABLE secret_versions, secrets, secrets_generation;
DROP FUNCTION secret_version_immutable();
