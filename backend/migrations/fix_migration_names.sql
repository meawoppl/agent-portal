-- Migration Name Standardization Script
--
-- This script updates the __diesel_schema_migrations table to reflect
-- the renamed migration directories. Run this ONCE on any existing database
-- that was created before the naming convention was standardized.
--
-- Background: Diesel stores migration versions as the timestamp prefix
-- (with dashes stripped). When we renamed migrations to remove the `-0000`
-- suffix, we changed the version identifier.
--
-- Usage: Run this via docker exec or psql:
--   docker exec <container> psql -U claude_portal -f /path/to/fix_migration_names.sql
--   -- or --
--   psql $DATABASE_URL -f backend/migrations/fix_migration_names.sql
--
-- Safe to run multiple times (uses idempotent UPDATE statements).

BEGIN;

-- The compact format 20260109000000 produces the same version as
-- 2026-01-09-000000 (dashes stripped), so no update needed for that one.

-- Remove -0000 suffix: 2026-01-10-211719-0000 → 2026-01-10-211719
-- Version changes: 202601102117190000 → 20260110211719
UPDATE __diesel_schema_migrations
SET version = '20260110211719'
WHERE version = '202601102117190000';

-- Remove -0000 suffix: 2026-01-14-185028-0000 → 2026-01-14-185028
-- Version changes: 202601141850280000 → 20260114185028
UPDATE __diesel_schema_migrations
SET version = '20260114185028'
WHERE version = '202601141850280000';

COMMIT;

-- Verify the changes
SELECT version, run_on FROM __diesel_schema_migrations ORDER BY version;
