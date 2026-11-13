#!/usr/bin/env python3
"""Summarize downloaded native-benchmark-* artifacts without hiding failed lanes."""
import argparse
import json
from pathlib import Path

p = argparse.ArgumentParser(description=__doc__)
p.add_argument("artifacts", type=Path)
a = p.parse_args()
print(
    "| Variant | Restored cache | Initial s | Unchanged s | Backend edit s | Dist edit s | Binary MiB | Retained MiB |"
)
print("|---|---|---:|---:|---:|---:|---:|---:|")
for path in sorted(a.artifacts.glob("native-benchmark-*/results.json")):
    data = json.loads(path.read_text())
    scenarios = {x["scenario"]: x for x in data["scenarios"]}

    def elapsed(name):
        return (
            f"{scenarios[name]['wall_seconds']:.2f}"
            if name in scenarios
            else "INCOMPLETE"
        )

    initial = scenarios.get("initial", {})
    cache = path.parent / "cache-size.txt"
    retained = data.get("local_cache", {}).get("retained_bytes")
    retained = (
        retained / 2**20
        if retained is not None
        else (int(cache.read_text().split()[0]) / 2**20 if cache.exists() else None)
    )
    print(
        f"| {path.parent.name.removeprefix('native-benchmark-')} | {bool(data['cache_hit'])} | {elapsed('initial')} | {elapsed('unchanged')} | {elapsed('backend-edit')} | {elapsed('frontend-edit')} | {initial.get('binary_bytes',0)/2**20:.2f} | {retained if retained is not None else 'INCOMPLETE'} |"
    )
