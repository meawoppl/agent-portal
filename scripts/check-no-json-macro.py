#!/usr/bin/env python3
"""Reject serde_json::json!/json! in production Rust code.

Test fixtures may still use json! while production protocol paths are migrated
to typed structs. This intentionally allows code inside #[cfg(test)] modules.
"""

from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]
SKIP_DIRS = {".git", "target", "dist"}
JSON_MACRO = re.compile(r"serde_json::json!\s*\(|(?<![A-Za-z0-9_])json!\s*\(")
CFG_TEST = re.compile(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]")
MOD_WITH_OPEN_BRACE = re.compile(r"\bmod\s+\w+\s*\{")


def rust_files():
    for path in ROOT.rglob("*.rs"):
        if any(part in SKIP_DIRS for part in path.parts):
            continue
        yield path


def code_before_comment(line: str) -> str:
    return line.split("//", 1)[0]


def brace_delta(line: str) -> int:
    # Good enough for this lint: it only needs to keep us inside ordinary
    # #[cfg(test)] modules, not parse arbitrary Rust.
    code = code_before_comment(line)
    return code.count("{") - code.count("}")


def violations_in(path: Path):
    violations = []
    cfg_test_pending = False
    test_depth = None
    depth = 0

    for lineno, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        code = code_before_comment(line)
        stripped = code.strip()
        in_test_module = test_depth is not None

        if not in_test_module and JSON_MACRO.search(code):
            violations.append((lineno, line.strip()))

        starts_test_module = False
        if CFG_TEST.search(stripped):
            cfg_test_pending = True
        elif cfg_test_pending and MOD_WITH_OPEN_BRACE.search(stripped):
            starts_test_module = True
        elif stripped and not stripped.startswith("#"):
            cfg_test_pending = False

        depth += brace_delta(line)

        if starts_test_module:
            test_depth = depth
            cfg_test_pending = False
        elif test_depth is not None and depth < test_depth:
            test_depth = None

    return violations


def main() -> int:
    failures = []
    for path in rust_files():
        for lineno, line in violations_in(path):
            failures.append((path.relative_to(ROOT), lineno, line))

    if failures:
        print("serde_json::json!/json! is not allowed in production Rust code.")
        print("Use typed structs plus serde_json::to_value instead.\n")
        for path, lineno, line in failures:
            print(f"{path}:{lineno}: {line}")
        return 1

    print("PASSED: no production Rust serde_json::json!/json! usages found")
    return 0


if __name__ == "__main__":
    sys.exit(main())
