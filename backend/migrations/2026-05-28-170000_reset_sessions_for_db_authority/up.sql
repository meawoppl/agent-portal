-- Reset legacy split-brain session state.
--
-- After this migration the backend database is the authority for which
-- launcher-backed sessions should run. Existing sessions were created under
-- the old launcher-local expected-session model, so keep users/credentials/
-- scheduled task definitions but clear session rows and their dependent data.
DELETE FROM sessions;
