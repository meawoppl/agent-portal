-- Distinguish why a session continuation was created. Existing rows were all
-- usage-limit resets (#231/#1260), so they default to 'limit'. Transient-529
-- overload auto-retries use 'overloaded' (auto-retry killed turns).
ALTER TABLE session_continuations
    ADD COLUMN reason VARCHAR(32) NOT NULL DEFAULT 'limit';
