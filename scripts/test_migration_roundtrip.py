"""Safety contracts for the migration CI runner (no database required)."""
import contextlib
import importlib.util
import io
import os
from pathlib import Path
import tempfile
import sys
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True

spec = importlib.util.spec_from_file_location("roundtrip", Path(__file__).with_name("check-migration-roundtrip.py"))
roundtrip = importlib.util.module_from_spec(spec)
spec.loader.exec_module(roundtrip)


class DisposableDatabaseTests(unittest.TestCase):
    def execute(self, fail=None, error=RuntimeError, container=""):
        calls = []
        def run(label, argv, env, capture=False):
            calls.append((label, argv, env.copy()))
            if label == fail:
                raise error("injected failure")
            return "schema\n" if "Generate schema" in label else "objects\n"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "backend/src").mkdir(parents=True)
            (root / "backend/src/schema.rs").write_text("schema\n")
            with patch.object(roundtrip, "ROOT", root), patch.object(roundtrip, "run", run), patch.dict(os.environ, {"DATABASE_URL": "postgresql://user:secret@localhost:5432/existing?sslmode=disable", "POSTGRES_TOOLS_CONTAINER": container}), contextlib.redirect_stdout(io.StringIO()):
                if fail:
                    with self.assertRaisesRegex(error, "injected failure"):
                        roundtrip.main()
                else:
                    roundtrip.main()
        return calls

    def test_all_migrations_and_snapshots_use_owned_database(self):
        calls = self.execute()
        create = calls[0][1]
        name = create[-1]
        self.assertRegex(name, r"^ap_migration_check_[a-f0-9]{32}$")
        for _, argv, env in calls:
            self.assertIn("/" + name + "?sslmode=disable", env["DATABASE_URL"])
            if argv[0] in ("createdb", "dropdb"):
                self.assertEqual(argv[-1], name)
                self.assertIn("/existing?", argv[2])
            else:
                self.assertNotIn("/existing", " ".join(argv))
        self.assertEqual(calls[-1][1][0], "dropdb")

    def test_container_tools_keep_owned_database_and_literal_arguments(self):
        calls = self.execute(container="postgres-service")
        name = calls[0][1][-1]
        for label, argv, env in calls:
            if argv[0] == "docker":
                self.assertEqual(argv[:3], ["docker", "exec", "postgres-service"])
                self.assertIn(argv[3], ["createdb", "dropdb", "pg_dump"])
                if argv[3] in ["createdb", "dropdb"]:
                    self.assertEqual(argv[-1], name)
                    self.assertIn("/existing?sslmode=disable", argv[-2])
                else:
                    self.assertEqual(argv[-1], env["DATABASE_URL"])
            else:
                self.assertEqual(argv[0], "diesel")
        self.assertEqual(calls[-1][1][3], "dropdb")
        url = "postgresql://user:p%24%28word%29@localhost/db?sslmode=disable"
        self.assertEqual(
            roundtrip.postgres_tool("pg_dump", ["--dbname", url], {"POSTGRES_TOOLS_CONTAINER": "abc123"}),
            ["docker", "exec", "abc123", "pg_dump", "--dbname", url],
        )

    def test_container_mode_failure_still_drops_only_owned_database(self):
        for label in ["Snapshot database objects", "Revert all migrations"]:
            calls = self.execute(label, container="postgres-service")
            self.assertEqual(calls[-1][1][:4], ["docker", "exec", "postgres-service", "dropdb"])
            self.assertEqual(calls[-1][1][-1], calls[0][1][-1])

    def test_container_option_injection_is_rejected(self):
        for container in ["--privileged", "name; command", "name with spaces"]:
            with self.subTest(container=container), self.assertRaises(ValueError):
                roundtrip.postgres_tool("pg_dump", [], {"POSTGRES_TOOLS_CONTAINER": container})

    def test_failed_creation_never_drops_any_database(self):
        calls = self.execute("Create isolated migration database")
        self.assertEqual(len(calls), 1)

    def test_migration_failures_drop_only_owned_database(self):
        for label in ["Apply all migrations", "Revert all migrations", "Reapply all migrations"]:
            with self.subTest(label=label):
                calls = self.execute(label)
                self.assertEqual(calls[-1][1][0], "dropdb")
                self.assertEqual(calls[-1][1][-1], calls[0][1][-1])

    def test_malformed_url_does_not_create_database(self):
        with patch.dict(os.environ, {"DATABASE_URL": "file:///not-postgres"}), patch.object(roundtrip, "run") as run:
            with self.assertRaises(ValueError):
                roundtrip.main()
            run.assert_not_called()

    def test_dbname_query_override_is_rejected_before_create(self):
        for key in ["dbname", "DBNAME", "db%6eame"]:
            with self.subTest(key=key), patch.dict(os.environ, {"DATABASE_URL": f"postgresql://localhost/maintenance?{key}=existing"}), patch.object(roundtrip, "run") as run:
                with self.assertRaisesRegex(ValueError, "dbname query override"):
                    roundtrip.main()
                run.assert_not_called()

    def test_interrupt_and_termination_unwind_cleanup(self):
        for error in [KeyboardInterrupt, SystemExit]:
            with self.subTest(error=error):
                calls = self.execute("Revert all migrations", error=error)
                self.assertEqual(calls[-1][1][0], "dropdb")
        with self.assertRaises(SystemExit) as stopped:
            roundtrip.terminate(15, None)
        self.assertEqual(stopped.exception.code, 143)

    def test_only_internal_attribute_versions_are_normalized(self):
        old = 'ALTER TABLE ONLY public.users ALTER COLUMN name SET (pg_attver=123);'
        new = old.replace('123', '456')
        self.assertEqual(roundtrip.normalize_dump(old), roundtrip.normalize_dump(new))
        for changed in [old.replace('name', 'email'), old.replace('pg_attver', 'user_option'), old.replace(');', ', n_distinct=1);')]:
            self.assertNotEqual(roundtrip.normalize_dump(old), roundtrip.normalize_dump(changed))
        sql = "SELECT 'pg_attver=123)';"
        self.assertEqual(roundtrip.normalize_dump(sql), sql)

    def test_schema_drift_is_a_failure(self):
        with contextlib.redirect_stdout(io.StringIO()), self.assertRaisesRegex(RuntimeError, "schema drift"):
            roundtrip.compare("before\n", "after\n", "schema")


if __name__ == "__main__":
    unittest.main()
