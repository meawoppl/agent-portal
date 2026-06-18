-- All open PRs in the session's repo, as a JSON array of {number, url, branch}.
-- Backs the session pill's PR list. Defaults to an empty array so existing rows
-- and inserts that don't set it are valid.
ALTER TABLE sessions ADD COLUMN open_prs JSONB NOT NULL DEFAULT '[]'::jsonb;
