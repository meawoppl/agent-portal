"""Safety contracts for the migration CI runner (no database required)."""
import contextlib
import importlib.util
import io
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("roundtrip", Path(__file__).with_name("check-migration-roundtrip.py"))
roundtrip = importlib.util.module_from_spec(spec)
spec.loader.exec_module(roundtrip)


class DisposableDatabaseTests(unittest.TestCase):
    def execute(self, fail=None):
        calls = []
        def run(label, argv, env, capture=False):
            calls.append((label, argv, env.copy()))
            if label == fail:
                raise RuntimeError("injected failure")
            return "schema\n" if "Generate schema" in label else "objects\n"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "backend/src").mkdir(parents=True)
            (root / "backend/src/schema.rs").write_text("schema\n")
            with patch.object(roundtrip, "ROOT", root), patch.object(roundtrip, "run", run), patch.dict(os.environ, {"DATABASE_URL": "postgresql://user:secret@localhost:5432/existing?sslmode=disable"}), contextlib.redirect_stdout(io.StringIO()):
                if fail:
                    with self.assertRaisesRegex(RuntimeError, "injected failure"):
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

    def test_schema_drift_is_a_failure(self):
        with contextlib.redirect_stdout(io.StringIO()), self.assertRaisesRegex(RuntimeError, "schema drift"):
            roundtrip.compare("before\n", "after\n", "schema")


if __name__ == "__main__":
    unittest.main()
