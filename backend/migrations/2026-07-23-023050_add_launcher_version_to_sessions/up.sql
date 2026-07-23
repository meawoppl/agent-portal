-- Persist the launching launcher's reported version onto the session row so
-- it survives into long-term archive manifests (#1258 provenance). The live
-- version lives only in the in-memory launcher registry, which is gone by the
-- time the archival sweep runs; capturing it at session-create time is the
-- only durable source. NULL for non-launcher (proxy-direct) sessions.
ALTER TABLE sessions ADD COLUMN launcher_version VARCHAR(32);
