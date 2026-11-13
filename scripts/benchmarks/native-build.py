#!/usr/bin/env python3
"""Measure native build profiles in an explicitly disposable checkout.

Artifacts include Cargo timing HTML/JSON, runner/toolchain identity, binary size,
cache size, a small HTTP runtime smoke sample, and output-identity assertions.
Does not change repository profiles or make a production optimization decision.
"""
import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[2]


def command(*args, **kwargs):
    return subprocess.check_output(args, cwd=ROOT, text=True, **kwargs).strip()


def bytes_under(path):
    return sum(p.stat().st_size for p in path.rglob("*") if p.is_file())


def get_config():
    with urllib.request.urlopen("http://127.0.0.1:3107/api/config", timeout=10) as r:
        return json.load(r)


def smoke(binary, output, scenario, expected_title):
    env = dict(os.environ, PORT="3107", HOST="127.0.0.1")
    with (output / f"{scenario}-server.log").open("w") as log:
        process = subprocess.Popen(
            [str(binary), "--dev-mode"],
            cwd=ROOT,
            env=env,
            stdout=log,
            stderr=subprocess.STDOUT,
        )
        try:
            deadline = time.monotonic() + 60
            while True:
                try:
                    config = get_config()
                    break
                except Exception:
                    if process.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError(f"Backend did not start; see {log.name}")
                    time.sleep(0.1)
            expected_hash = command("git", "rev-parse", "--short", "HEAD")
            version = command("git", "rev-list", "--count", "HEAD")
            assert config["git_hash"] == expected_hash, config
            assert config["server_version"].endswith("." + version), config
            assert config["app_title"].endswith(expected_title), config
            if scenario == "frontend-edit":
                with urllib.request.urlopen(
                    "http://127.0.0.1:3107/native-benchmark-marker.txt", timeout=10
                ) as response:
                    assert (
                        response.read() == b"native frontend-only change\n"
                    ), "changed frontend asset was not embedded"
            # Same request shape and concurrency for every variant. This is a
            # smoke comparison, NOT a substitute for representative load tests.
            start = time.monotonic()
            with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
                results = list(pool.map(lambda _: get_config(), range(1000)))
            assert all(r["git_hash"] == expected_hash for r in results)
            return {
                "identity": config,
                "http_1000_requests_seconds": time.monotonic() - start,
            }
        finally:
            process.terminate()
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--disposable-checkout", action="store_true", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cache-hit", default="false")
    args = parser.parse_args()
    os.chdir(ROOT)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    target = Path(os.environ["CARGO_TARGET_DIR"]).resolve()
    target.mkdir(parents=True, exist_ok=True)
    backend = ROOT / "backend/src/handlers/config.rs"
    frontend = ROOT / "frontend/dist/native-benchmark-marker.txt"
    original = backend.read_bytes()
    assert not frontend.exists(), "benchmark marker must not overwrite an asset"
    assert "app_title: app_state.app_title.clone()," in original.decode()
    env = dict(os.environ, CARGO_BUILD_JOBS=os.environ.get("CARGO_BUILD_JOBS", "2"))
    metadata = {
        "commit": command("git", "rev-parse", "HEAD"),
        "runner": platform.platform(),
        "cpu_count": os.cpu_count(),
        "cpu": command("sh", "-c", "lscpu | head -n 24"),
        "rustc": command("rustc", "-Vv"),
        "cargo": command("cargo", "-V"),
        "runner_image": os.environ.get("ImageVersion"),
        "cache_hit": args.cache_hit,
        "settings": {
            k: v
            for k, v in env.items()
            if k.startswith("CARGO_PROFILE_")
            or k in ("CARGO_INCREMENTAL", "RUSTFLAGS", "CARGO_BUILD_JOBS")
        },
        "initial_target_bytes": bytes_under(target),
        "scenarios": [],
    }
    (output / "results.json").write_text(json.dumps(metadata, indent=2))

    def build(scenario, title="⚠️ INSECURE DEV MODE ⚠️"):
        started = time.monotonic()
        with (output / f"{scenario}-cargo.jsonl").open("w") as stdout, (
            output / f"{scenario}-cargo.log"
        ).open("w") as stderr:
            subprocess.run(
                [
                    "cargo",
                    "build",
                    "--locked",
                    "--release",
                    "-p",
                    "backend",
                    "--timings",
                    "--message-format=json",
                ],
                cwd=ROOT,
                env=env,
                stdout=stdout,
                stderr=stderr,
                check=True,
            )
        seconds = time.monotonic() - started
        binary = target / "release/backend"
        records = []
        for line in (output / f"{scenario}-cargo.jsonl").read_text().splitlines():
            try:
                row = json.loads(line)
            except ValueError:
                continue
            if row.get("reason") == "compiler-artifact":
                records.append(
                    {
                        "package": row["package_id"],
                        "target": row["target"]["name"],
                        "fresh": row["fresh"],
                    }
                )
        item = {
            "scenario": scenario,
            "wall_seconds": seconds,
            "binary_bytes": binary.stat().st_size,
            "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
            "target_bytes": bytes_under(target),
            "artifacts": records,
        }
        item.update(smoke(binary, output, scenario, title))
        metadata["scenarios"].append(item)
        timing_dir = target / "cargo-timings"
        if timing_dir.exists():
            shutil.copytree(
                timing_dir, output / f"{scenario}-timings", dirs_exist_ok=True
            )
        (output / "results.json").write_text(json.dumps(metadata, indent=2))
        print(
            f'{scenario}: {seconds:.2f}s, {item["binary_bytes"]} binary bytes, {item["target_bytes"]} target bytes',
            flush=True,
        )

    try:
        # First run is cold; rerunning the workflow measures restored cache.
        build("initial")
        build("unchanged")
        backend.write_text(
            original.decode().replace(
                "app_title: app_state.app_title.clone(),",
                'app_title: format!("{} native-benchmark", app_state.app_title),',
            )
        )
        build("backend-edit", "native-benchmark")
        backend.write_bytes(original)
        # Restoring the backend is measured separately, not charged to the
        # frontend-only edit. Dist is the actual optimized WASM distribution.
        build("restore-backend")
        frontend.write_text("native frontend-only change\n")
        build("frontend-edit")
        frontend.unlink()
        build("restore-frontend")
    finally:
        backend.write_bytes(original)
        frontend.unlink(missing_ok=True)
    metadata["final_target_bytes"] = bytes_under(target)
    (output / "results.json").write_text(json.dumps(metadata, indent=2))


if __name__ == "__main__":
    main()
