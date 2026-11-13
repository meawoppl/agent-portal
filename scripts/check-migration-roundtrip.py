#!/usr/bin/env python3
"""Check structural migrations in an owned disposable database, never the app DB.

DATABASE_URL is only the maintenance connection used to CREATE/DROP a randomly
named database. Requires CREATEDB, PostgreSQL client tools and diesel_cli 2.3.11.
Set POSTGRES_TOOLS_CONTAINER to use that Docker service's matching client tools.
DATABASE_URL must address the same server from the host (Diesel) and container
(client tools); CI uses localhost:5432 in both namespaces. No URL is rewritten.
Data-only irreversible migrations are not expected to restore deleted rows.
"""
import difflib
import json
import os
import re
import signal
from pathlib import Path
import subprocess
import tempfile
from urllib.parse import parse_qsl, urlsplit, urlunsplit
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


def postgres_tool(tool, args, env):
    container = env.get("POSTGRES_TOOLS_CONTAINER")
    if not container:
        return [tool, *args]
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,127}", container):
        raise ValueError("POSTGRES_TOOLS_CONTAINER must be a Docker container ID or name")
    # Literal argv, no shell or URL transformation. Every tool reaches the same
    # maintenance/owned database as before, using the server image's binaries.
    return ["docker", "exec", container, tool, *args]


def dump_schema(env):
    dump = run("Snapshot database objects", postgres_tool("pg_dump", ["--schema-only", "--no-owner", "--no-privileges", "--dbname", env["DATABASE_URL"]], env), env, capture=True)
    return normalize_dump(dump)


def normalize_dump(dump):
    # Older pg_dump clients expose the server's internal pg_attver WAL LSN.
    # Recreating a column necessarily changes it; it is not user-settable DDL.
    # Keep the statement/column and every other option so real drift still fails.
    # https://github.com/postgres/postgres/blob/REL_16_STABLE/src/backend/catalog/README.attver.md
    lines = []
    for line in dump.splitlines():
        if line.startswith(("\\restrict ", "\\unrestrict ")):
            continue
        if line.startswith("ALTER TABLE ") and " ALTER COLUMN " in line:
            line = re.sub(r"(?<=pg_attver=)[0-9]+(?=[,)])", "0", line)
        lines.append(line)
    return "\n".join(lines)


def main():
    maintenance = os.environ["DATABASE_URL"]
    url = urlsplit(maintenance)
    if url.scheme not in ("postgres", "postgresql") or not url.netloc:
        raise ValueError("DATABASE_URL must be a PostgreSQL URL with a host")
    if any(key.lower() == "dbname" for key, _ in parse_qsl(url.query)):
        raise ValueError("DATABASE_URL must not contain a dbname query override")
    name = "ap_migration_check_" + uuid.uuid4().hex
    database_url = urlunsplit(url._replace(path="/" + name))
    env = dict(os.environ, DATABASE_URL=database_url)
    # createdb/dropdb receive only the maintenance connection; Diesel receives
    # only our new database. No migration or registry operation hits the former.
    run("Create isolated migration database", postgres_tool("createdb", ["--maintenance-db", maintenance, name], env), env)
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
        run("Drop isolated migration database", postgres_tool("dropdb", ["--maintenance-db", maintenance, name], env), env)


def terminate(signum, _frame):
    # Turn normal job cancellation into stack unwinding so finally drops the DB.
    # SIGKILL cannot be handled; CI's disposable service remains the backstop.
    raise SystemExit(128 + signum)


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, terminate)
    main()
