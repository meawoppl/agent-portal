#!/usr/bin/env python3
"""Summarize downloaded native-benchmark-* artifacts without hiding failed lanes."""
import argparse
import json
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("artifacts", type=Path)
args = parser.parse_args()
paths = sorted(args.artifacts.glob("native-benchmark-*/results.json"))
if not paths and (args.artifacts / "results.json").is_file():
    paths = [args.artifacts / "results.json"]
rows = [
    (p.parent.name.removeprefix("native-benchmark-"), json.loads(p.read_text()))
    for p in paths
]


def number(value, divisor=1):
    return f"{value/divisor:.2f}" if value is not None else "INCOMPLETE"


print("| Variant | Cold s | Unchanged s | Backend edit s | Dist edit s | Binary MiB |")
print("|---|---:|---:|---:|---:|---:|")
for name, data in rows:
    scenarios = {s["scenario"]: s for s in data["scenarios"]}
    elapsed = lambda key: number(scenarios.get(key, {}).get("wall_seconds"))
    size = number(scenarios.get("initial", {}).get("binary_bytes"), 2**20)
    print(
        f"| {name} | {elapsed('initial')} | {elapsed('unchanged')} | {elapsed('backend-edit')} | {elapsed('frontend-edit')} | {size} |"
    )

print(
    "\n| Variant | Retained MiB | Archive MiB | Pack s | Unpack s | New HEAD build s |"
)
print("|---|---:|---:|---:|---:|---:|")
for name, data in rows:
    cache = data.get("local_cache", {})
    rebuilt = next(
        (
            s["wall_seconds"]
            for s in data["scenarios"]
            if s["scenario"] == "restored-new-commit"
        ),
        None,
    )
    print(
        f"| {name} | {number(cache.get('retained_bytes'),2**20)} | {number(cache.get('compressed_bytes'),2**20)} | {number(cache.get('compression_seconds'))} | {number(cache.get('restoration_seconds'))} | {number(rebuilt)} |"
    )

print(
    "\nLocal archive costs exclude network transfer. Fixed-HEAD edits and new-HEAD restore are separate experiments."
)
for name, data in rows:
    cpu = next(
        (
            line.split(":", 1)[1].strip()
            for line in data["cpu"].splitlines()
            if line.startswith("Model name:")
        ),
        "unknown",
    )
    print(f"- {name}: {cpu}; image {data['runner_image']}; source {data['commit']}")
