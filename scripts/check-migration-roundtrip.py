#!/usr/bin/env python3
"""Check structural migrations in an owned disposable database, never the app DB.

DATABASE_URL is only the maintenance connection used to CREATE/DROP a randomly
named database. Requires CREATEDB, PostgreSQL client tools and diesel_cli 2.3.11.
Data-only irreversible migrations are not expected to restore deleted rows.
"""
import difflib
import json
import os
from pathlib import Path
import subprocess
import tempfile
from urllib.parse import urlsplit, urlunsplit
import uuid

ROOT = Path(__file__).resolve().parent.parent


def run(label, argv, env, capture=False):
    print(label, flush=True)
    result = subprocess.run(argv, env=env, text=True, stdout=subprocess.PIPE if capture else None)
    if result.returncode:
        # Do not include argv: the maintenance URL may contain credentials.
        raise RuntimeError(f"{label} failed (exit {result.returncode})")
    return result.stdout


def compare(expected, actual, label):
    if expected != actual:
        print("".join(difflib.unified_diff(expected.splitlines(True), actual.splitlines(True), fromfile="expected", tofile=label)))
        raise RuntimeError(f"{label}: schema drift")


def dump_schema(env):
    dump = run("Snapshot database objects", ["pg_dump", "--schema-only", "--no-owner", "--no-privileges", "--dbname", env["DATABASE_URL"]], env, capture=True)
    # Newer pg_dump versions generate a random psql guard token per dump.
    return "\n".join(line for line in dump.splitlines() if not line.startswith(("\\restrict ", "\\unrestrict ")))


def main():
    maintenance = os.environ["DATABASE_URL"]
    url = urlsplit(maintenance)
    if url.scheme not in ("postgres", "postgresql") or not url.netloc:
        raise ValueError("DATABASE_URL must be a PostgreSQL URL with a host")
    name = "ap_migration_check_" + uuid.uuid4().hex
    database_url = urlunsplit(url._replace(path="/" + name))
    env = dict(os.environ, DATABASE_URL=database_url)
    # createdb/dropdb receive only the maintenance connection; Diesel receives
    # only our new database. No migration or registry operation hits the former.
    run("Create isolated migration database", ["createdb", "--maintenance-db", maintenance, name], env)
    try:
        with tempfile.TemporaryDirectory(prefix="portal-migration-") as temp:
            config = Path(temp) / "diesel.toml"
            config.write_text('[print_schema]\ncustom_type_derives = ["diesel::query_builder::QueryId"]\n[migrations_directory]\ndir = ' + json.dumps(str(ROOT / "backend/migrations")) + '\n')
            diesel = ["diesel", "--config-file", str(config)]
            run("Apply all migrations", diesel + ["migration", "run"], env)
            before = run("Generate schema after apply", diesel + ["print-schema"], env, capture=True)
            compare((ROOT / "backend/src/schema.rs").read_text(), before, "schema after apply")
            objects_before = dump_schema(env)
            run("Revert all migrations", diesel + ["migration", "revert", "--all"], env)
            run("Reapply all migrations", diesel + ["migration", "run"], env)
            after = run("Generate schema after reapply", diesel + ["print-schema"], env, capture=True)
            compare(before, after, "schema after reapply")
            compare(objects_before, dump_schema(env), "database objects after reapply")
            print("Migration structural roundtrip and generated schema match.")
    finally:
        run("Drop isolated migration database", ["dropdb", "--maintenance-db", maintenance, name], env)


if __name__ == "__main__":
    main()
