#!/usr/bin/env python3
"""Publish a ready platform without waiting for unrelated matrix lanes.

Only this script advances release-ready refs. Non-force Git pushes enforce
source ancestry even if an older workflow finishes or is retried later.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import tomllib

PLATFORMS = {
    "linux-x86_64",
    "linux-aarch64",
    "darwin-aarch64",
    "darwin-x86_64",
    "windows-x86_64",
}


def run(*args, input=None):
    return subprocess.check_output(args, input=input, text=True).strip()


def api(path, payload=None):
    args = ["gh", "api", path]
    if payload is not None:
        args += ["--method", "POST", "--input", "-"]
    return json.loads(
        run(*args, input=json.dumps(payload) if payload is not None else None)
    )


def version():
    base = tomllib.loads(Path("Cargo.toml").read_text())["workspace"]["package"][
        "version"
    ]
    return ".".join(base.split(".")[:2] + [run("git", "rev-list", "--count", "HEAD")])


def artifact_names(platform):
    ext = ".exe" if platform.startswith("windows-") else ""
    return [f"{prefix}-{platform}{ext}" for prefix in ("agent-portal", "claude-portal")]


def manifest_for(directory, platform, sha, build_version):
    return {
        "schema": 1,
        "commit": sha,
        "version": build_version,
        "platform": platform,
        "artifacts": {
            name: {
                "sha256": hashlib.sha256((directory / name).read_bytes()).hexdigest(),
                "size": (directory / name).stat().st_size,
            }
            for name in artifact_names(platform)
        },
    }


def validate_manifest(manifest, platform, sha, assets):
    assert manifest["schema"] == 1 and manifest["commit"] == sha
    assert manifest["platform"] == platform
    assert set(manifest["artifacts"]) == set(artifact_names(platform))
    for name, expected in manifest["artifacts"].items():
        asset = next(a for a in assets if a["name"] == name)
        assert asset["state"] == "uploaded"
        assert asset["size"] == expected["size"] > 0
        assert asset["digest"] == "sha256:" + expected["sha256"]


def channel_advance(repo, channel, sha):
    ref = f"refs/heads/release-ready/{channel}"
    for _ in range(3):
        remote = run("git", "ls-remote", "origin", ref).split()
        if remote:
            old = remote[0]
            run("git", "fetch", "--no-tags", "origin", ref)
            if old == sha:
                return True
            if (
                subprocess.run(
                    ["git", "merge-base", "--is-ancestor", sha, old]
                ).returncode
                == 0
            ):
                print(
                    f"{channel}: newer source commit already published; keeping {old}"
                )
                return False
            if (
                subprocess.run(
                    ["git", "merge-base", "--is-ancestor", old, sha]
                ).returncode
                != 0
            ):
                raise RuntimeError("Refusing divergent release source history")
        # Git's fast-forward check is atomic; a racing newer publisher wins.
        result = subprocess.run(["git", "push", "origin", f"{sha}:{ref}"])
        if result.returncode == 0:
            return True
    raise RuntimeError(f"Could not advance {repo} {channel}; retry the workflow")


def prepare(repo, sha):
    tag = f"cli-{sha}"
    found = subprocess.run(
        [
            "gh",
            "release",
            "view",
            tag,
            "--repo",
            repo,
            "--json",
            "databaseId,targetCommitish",
        ],
        capture_output=True,
        text=True,
    )
    if found.returncode == 0:
        release = json.loads(found.stdout)
        assert release["targetCommitish"] == sha
        return
    api(
        f"repos/{repo}/releases",
        {
            "tag_name": tag,
            "target_commitish": sha,
            "name": f"CLI {version()} ({sha[:8]})",
            "draft": True,
            "make_latest": "false",
            "body": f"Source commit: {sha}\n\nPlatforms become available independently. A ready-PLATFORM.json manifest marks a complete, checksummed launcher/proxy pair. Missing platforms remain on their previous ready build. The legacy latest release is updated only after every platform succeeds.",
        },
    )


def publish(repo, sha, platform, directory):
    started = time.monotonic()
    tag = f"cli-{sha}"
    manifest_name = f"ready-{platform}.json"
    # REST lookup by tag excludes drafts. gh resolves drafts through GraphQL;
    # subsequent reads by numeric ID work both before and after publication.
    release_id = json.loads(
        run("gh", "release", "view", tag, "--repo", repo, "--json", "databaseId")
    )["databaseId"]
    release = api(f"repos/{repo}/releases/{release_id}")
    assert release["target_commitish"] == sha
    with tempfile.TemporaryDirectory() as temp:
        manifest_path = Path(temp) / manifest_name
        if any(a["name"] == manifest_name for a in release["assets"]):
            # An already completed attempt is immutable, even if rebuilding
            # the same source produces a different timestamp or signature.
            run(
                "gh",
                "release",
                "download",
                tag,
                "--repo",
                repo,
                "--pattern",
                manifest_name,
                "--output",
                str(manifest_path),
            )
            manifest = json.loads(manifest_path.read_text())
        else:
            manifest = manifest_for(directory, platform, sha, version())
            for name in artifact_names(platform):
                existing = next(
                    (a for a in release["assets"] if a["name"] == name), None
                )
                if existing:
                    # Resume an interrupted attempt using its first immutable
                    # asset from this exact source commit, not a rebuilt timestamp.
                    digest = existing["digest"]
                    assert digest.startswith("sha256:") and len(digest) == 71
                    assert existing["state"] == "uploaded" and existing["size"] > 0
                    manifest["artifacts"][name] = {
                        "sha256": digest[7:],
                        "size": existing["size"],
                    }
                else:
                    run(
                        "gh",
                        "release",
                        "upload",
                        tag,
                        str(directory / name),
                        "--repo",
                        repo,
                    )
            manifest_path.write_text(json.dumps(manifest, sort_keys=True) + "\n")
            run("gh", "release", "upload", tag, str(manifest_path), "--repo", repo)
        release = api(f"repos/{repo}/releases/{release_id}")
        validate_manifest(manifest, platform, sha, release["assets"])
        if release["draft"]:
            run(
                "gh",
                "api",
                f"repos/{repo}/releases/{release['id']}",
                "--method",
                "PATCH",
                "--input",
                "-",
                input=json.dumps({"draft": False, "make_latest": "false"}),
            )
        advanced = channel_advance(repo, platform, sha)
    if not advanced:
        print(f"READY_PLATFORM_SKIPPED= superseded platform={platform} commit={sha}")
        return
    elapsed = time.monotonic() - started
    print(f"READY_PLATFORM_SECONDS={elapsed:.3f} platform={platform} commit={sha}")
    if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(summary, "a") as out:
            out.write(
                f"\n{platform}: ready publication took **{elapsed:.2f}s**, source `{sha}`. Other matrix lanes were not awaited.\n"
            )


def legacy(repo, sha, directory):
    # This job has its own concurrency group. The branch guard also rejects
    # manually rerun old source commits, regardless of their run number.
    current = subprocess.run(
        ["gh", "api", f"repos/{repo}/releases/tags/latest"],
        capture_output=True,
        text=True,
    )
    if current.returncode != 0 and "HTTP 404" not in current.stderr:
        raise RuntimeError(
            "Could not inspect legacy release; refusing to mutate release state"
        )
    if current.returncode == 0:
        body = json.loads(current.stdout).get("body") or ""
        import re

        old = re.search(r"\*\*Commit:\*\* ([0-9a-f]{40})", body)
        if not old:
            raise RuntimeError(
                "Legacy release lacks source commit; refusing an unverified replacement"
            )
        if old[1] != sha:
            run("git", "fetch", "origin", old[1])
            if (
                subprocess.run(
                    ["git", "merge-base", "--is-ancestor", sha, old[1]]
                ).returncode
                == 0
            ):
                print("Keeping newer complete legacy release")
                return
            if (
                subprocess.run(
                    ["git", "merge-base", "--is-ancestor", old[1], sha]
                ).returncode
                != 0
            ):
                raise RuntimeError("Refusing divergent legacy release source history")
    if not channel_advance(repo, "complete", sha):
        return
    expected = [name for platform in PLATFORMS for name in artifact_names(platform)]
    assert sorted(p.name for p in directory.iterdir() if p.is_file()) == sorted(
        expected
    )
    if current.returncode == 0:
        run("gh", "release", "delete", "latest", "--repo", repo, "--yes")
    run("git", "push", "--force", "origin", f"{sha}:refs/tags/latest")
    run(
        "gh",
        "release",
        "create",
        "latest",
        "--repo",
        repo,
        "--verify-tag",
        "--latest",
        "--title",
        f"Latest CLI {version()}",
        "--notes",
        f"Complete five-platform build.\n\n**Commit:** {sha}",
        *[str(directory / name) for name in sorted(expected)],
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=["prepare", "publish", "legacy"])
    parser.add_argument("--platform", choices=sorted(PLATFORMS))
    parser.add_argument("--directory", type=Path)
    args = parser.parse_args()
    assert (
        os.environ["GITHUB_REF"] == "refs/heads/main"
    ), "Only main may publish CLI releases"
    repo, sha = os.environ["GITHUB_REPOSITORY"], os.environ["GITHUB_SHA"]
    assert run("git", "rev-parse", "HEAD") == sha
    if args.action == "prepare":
        prepare(repo, sha)
    elif args.action == "publish":
        publish(repo, sha, args.platform, args.directory)
    else:
        legacy(repo, sha, args.directory)
