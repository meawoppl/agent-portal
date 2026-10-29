#!/usr/bin/env python3
"""Fail closed on undocumented or expired cargo-audit exposure exceptions."""
from datetime import date, datetime, timezone
from pathlib import Path
import tomllib

ROOT = Path(__file__).resolve().parent.parent


def validate(config, registry, today):
    ignored = config.get("advisories", {}).get("ignore", [])
    entries = registry.get("exceptions", [])
    ids = [entry["id"] for entry in entries]
    if len(ignored) != len(set(ignored)) or len(ids) != len(set(ids)):
        raise ValueError("duplicate audit exception")
    if set(ignored) != set(ids):
        raise ValueError("audit ignores must exactly match documented exceptions")
    for entry in entries:
        name = entry["id"]
        if not entry.get("owner", "").strip() or not entry.get("reason", "").strip():
            raise ValueError(f"{name}: owner and exposure reason are required")
        reviewed, deadline = entry.get("reviewed_on"), entry.get("review_by")
        if type(reviewed) is not date or type(deadline) is not date:
            raise ValueError(f"{name}: review dates must be TOML dates")
        if not reviewed <= today <= deadline:
            raise ValueError(f"{name}: review is expired or not yet valid (review by {deadline})")
        if (deadline - reviewed).days > 31:
            raise ValueError(f"{name}: review window exceeds 31 days")


def main():
    config = tomllib.loads((ROOT / ".cargo/audit.toml").read_text())
    registry = tomllib.loads((ROOT / ".cargo/audit-exceptions.toml").read_text())
    validate(config, registry, datetime.now(timezone.utc).date())
    print("Audit exceptions are documented and within their review windows.")


if __name__ == "__main__":
    main()
