#!/usr/bin/env python3
"""Local publication simulation plus real Git fast-forward/race guard tests."""
import contextlib
import copy
import hashlib
import io
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "release_platform", Path(__file__).with_name("release-platform.py")
)
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


class PublicationTests(unittest.TestCase):
    def test_ready_linux_publishes_without_windows_and_retry_is_immutable(self):
        sha = "a" * 40
        release = dict(id=1, target_commitish=sha, draft=True, assets=[])
        uploads, channels, contents = [], [], {}

        def api(path, payload=None):
            self.assertEqual(
                path,
                "repos/owner/repo/releases/1",
                "drafts must be fetched by ID, not tag",
            )
            return copy.deepcopy(release)

        def run(*args, input=None):
            if args[:3] == ("gh", "release", "view"):
                return json.dumps({"databaseId": 1})
            elif args[:3] == ("gh", "release", "upload"):
                path = Path(args[4])
                data = path.read_bytes()
                name = path.name
                uploads.append(name)
                contents[name] = data
                release["assets"].append(
                    dict(
                        name=name,
                        state="uploaded",
                        size=len(data),
                        digest="sha256:" + hashlib.sha256(data).hexdigest(),
                    )
                )
            elif args[:3] == ("gh", "release", "download"):
                name = args[args.index("--pattern") + 1]
                Path(args[args.index("--output") + 1]).write_bytes(contents[name])
            elif args[:2] == ("gh", "api"):
                assert json.loads(input)["make_latest"] == "false"
                release["draft"] = False
            else:
                raise AssertionError(args)
            return ""

        def advance(repo, platform, commit):
            self.assertFalse(release["draft"])
            self.assertIn("ready-linux-x86_64.json", uploads)
            channels.append((platform, commit))
            return True

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in m.artifact_names("linux-x86_64"):
                (root / name).write_bytes(name.encode())
            with patch.object(m, "api", api), patch.object(m, "run", run), patch.object(
                m, "channel_advance", advance
            ), patch.object(m, "version", lambda: "2.15.2000"):
                m.publish("owner/repo", sha, "linux-x86_64", root)
                self.assertEqual(channels, [("linux-x86_64", sha)])
                self.assertFalse(any("windows" in name for name in uploads))
                # A rebuilt timestamp/signature must not overwrite the first ready pair.
                for name in m.artifact_names("linux-x86_64"):
                    (root / name).write_bytes(b"rebuilt")
                m.publish("owner/repo", sha, "linux-x86_64", root)
                self.assertEqual(len(uploads), 3)
                with patch.object(
                    m, "channel_advance", return_value=False
                ), contextlib.redirect_stdout(io.StringIO()) as output:
                    m.publish("owner/repo", sha, "linux-x86_64", root)
                self.assertIn("READY_PLATFORM_SKIPPED", output.getvalue())
                self.assertNotIn("READY_PLATFORM_SECONDS", output.getvalue())

    def test_manifest_rejects_missing_or_corrupt_pair(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in m.artifact_names("linux-x86_64"):
                (root / name).write_bytes(b"canary")
            manifest = m.manifest_for(root, "linux-x86_64", "a" * 40, "2.15.2000")
            with self.assertRaises((StopIteration, AssertionError)):
                m.validate_manifest(manifest, "linux-x86_64", "a" * 40, [])
            assets = [
                dict(
                    name=name,
                    state="uploaded",
                    size=entry["size"],
                    digest="sha256:" + entry["sha256"],
                )
                for name, entry in manifest["artifacts"].items()
            ]
            m.validate_manifest(manifest, "linux-x86_64", "a" * 40, assets)
            assets[0]["digest"] = "sha256:" + "0" * 64
            with self.assertRaises(AssertionError):
                m.validate_manifest(manifest, "linux-x86_64", "a" * 40, assets)

    def test_legacy_api_failure_does_not_mutate_tags_or_release(self):
        failure = subprocess.CompletedProcess(
            [], 1, "", "gh: API rate limit exceeded (HTTP 403)"
        )
        with patch.object(m.subprocess, "run", return_value=failure), patch.object(
            m, "channel_advance"
        ) as advance:
            with self.assertRaisesRegex(RuntimeError, "refusing to mutate"):
                m.legacy("owner/repo", "a" * 40, Path("unused"))
            advance.assert_not_called()

    def test_orphan_legacy_tag_is_removed_before_create_target(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for platform in m.PLATFORMS:
                for name in m.artifact_names(platform):
                    (root / name).write_bytes(b"fixture")
            source, old = "a" * 40, "b" * 40
            removed = []

            def process(args, *pos, **kwargs):
                if args[:2] == ["gh", "api"]:
                    return subprocess.CompletedProcess(
                        args, 1, "", "gh: Not Found (HTTP 404)"
                    )
                if args[:3] == ["git", "merge-base", "--is-ancestor"]:
                    return subprocess.CompletedProcess(
                        args, 0 if args[3:] == [old, source] else 1
                    )
                raise AssertionError(args)

            def run(*args, input=None):
                if args[:2] == ("git", "ls-remote"):
                    return f"{old}\trefs/tags/latest"
                if args[:2] == ("git", "fetch"):
                    return ""
                if args[:2] == ("git", "rev-parse"):
                    return old
                if args[:2] == ("gh", "api"):
                    self.assertEqual(
                        args[2:],
                        ("repos/owner/repo/git/refs/tags/latest", "--method", "DELETE"),
                    )
                    removed.append(True)
                    return ""
                if args[:3] == ("gh", "release", "create"):
                    self.assertTrue(removed)
                    self.assertEqual(args[args.index("--target") + 1], source)
                    return ""
                raise AssertionError(args)

            with patch.object(m.subprocess, "run", process), patch.object(
                m, "run", run
            ), patch.object(m, "channel_advance", return_value=True), patch.object(
                m, "version", return_value="2.15.2000"
            ):
                m.legacy("owner/repo", source, root)
            self.assertEqual(removed, [True])

    def test_newer_or_divergent_orphan_tag_is_never_deleted(self):
        source, other = "a" * 40, "b" * 40
        for newer in [True, False]:

            def process(args, *pos, **kwargs):
                if args[:2] == ["gh", "api"]:
                    return subprocess.CompletedProcess(
                        args, 1, "", "gh: Not Found (HTTP 404)"
                    )
                if args[:3] == ["git", "merge-base", "--is-ancestor"]:
                    return subprocess.CompletedProcess(
                        args, 0 if newer and args[3:] == [source, other] else 1
                    )
                raise AssertionError(args)

            def run(*args, input=None):
                if args[:2] == ("git", "ls-remote"):
                    return f"{other}\trefs/tags/latest"
                if args[:2] == ("git", "fetch"):
                    return ""
                if args[:2] == ("git", "rev-parse"):
                    return other
                raise AssertionError(f"Unexpected mutation: {args}")

            with patch.object(m.subprocess, "run", process), patch.object(
                m, "run", run
            ), patch.object(m, "channel_advance") as advance:
                if newer:
                    m.legacy("owner/repo", source, Path("unused"))
                else:
                    with self.assertRaisesRegex(RuntimeError, "divergent legacy tag"):
                        m.legacy("owner/repo", source, Path("unused"))
                advance.assert_not_called()

    def test_pointer_validation_rejects_schema_channel_and_source(self):
        valid = {"schema": 1, "channel": "linux-x86_64", "source": "a" * 40}
        self.assertEqual(m.pointer_source(valid, "linux-x86_64"), "a" * 40)
        for field, value in [
            ("schema", 2),
            ("channel", "windows-x86_64"),
            ("source", "../main"),
        ]:
            with self.assertRaises(RuntimeError):
                m.pointer_source(dict(valid, **{field: value}), "linux-x86_64")

    def test_data_only_channel_retries_cas_and_cannot_regress_source(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            remote = root / "remote"
            work = root / "work"
            subprocess.run(
                ["git", "init", "--bare", str(remote)], check=True, capture_output=True
            )
            subprocess.run(
                ["git", "clone", str(remote), str(work)],
                check=True,
                capture_output=True,
            )
            # Model GitHub's ordinary Actions token: source trees containing
            # workflow files cannot be introduced on release-ready branches.
            hook = remote / "hooks/pre-receive"
            hook.write_text(
                """#!/bin/sh
while read old new ref; do
  case "$ref" in
    refs/heads/release-ready-v2/*)
      if git ls-tree -r --name-only "$new" | grep -q '^.github/workflows/'; then
        echo "workflows permission required" >&2
        exit 1
      fi ;;
  esac
done
"""
            )
            hook.chmod(0o755)
            with contextlib.chdir(work):
                m.run("git", "config", "user.email", "test@example.invalid")
                m.run("git", "config", "user.name", "Test")
                Path(".github/workflows").mkdir(parents=True)
                Path(".github/workflows/ci.yml").write_text("name: fixture\n")
                m.run("git", "add", ".")
                m.run("git", "commit", "-m", "old")
                old = m.run("git", "rev-parse", "HEAD")
                m.run("git", "commit", "--allow-empty", "-m", "middle")
                middle = m.run("git", "rev-parse", "HEAD")
                m.run("git", "commit", "--allow-empty", "-m", "new")
                new = m.run("git", "rev-parse", "HEAD")
                ref = "refs/heads/release-ready-v2/linux-x86_64"
                self.assertNotEqual(
                    subprocess.run(
                        ["git", "push", "origin", f"{new}:{ref}"], capture_output=True
                    ).returncode,
                    0,
                )
                self.assertTrue(m.channel_advance("owner/repo", "linux-x86_64", old))
                first_pointer = m.run("git", "ls-remote", "origin", ref).split()[0]
                original_run = subprocess.run
                raced = False

                def race(args, *pos, **kwargs):
                    nonlocal raced
                    if args[:3] == ["git", "push", "origin"] and not raced:
                        raced = True
                        self.assertTrue(
                            m.channel_advance("owner/repo", "linux-x86_64", new)
                        )
                    return original_run(args, *pos, **kwargs)

                with patch.object(m.subprocess, "run", race):
                    self.assertFalse(
                        m.channel_advance("owner/repo", "linux-x86_64", middle)
                    )
                self.assertTrue(raced)
                self.assertFalse(m.channel_advance("owner/repo", "linux-x86_64", old))
                self.assertTrue(m.channel_advance("owner/repo", "linux-x86_64", new))
                data_commit = m.run("git", "ls-remote", "origin", ref).split()[0]
                self.assertNotEqual(data_commit, new)
                self.assertEqual(
                    m.run("git", "ls-tree", "-r", "--name-only", data_commit),
                    "readiness.json",
                )
                self.assertEqual(
                    json.loads(m.run("git", "show", f"{data_commit}:readiness.json"))[
                        "source"
                    ],
                    new,
                )
                self.assertEqual(
                    json.loads(m.run("git", "show", f"{first_pointer}:readiness.json"))[
                        "source"
                    ],
                    old,
                    "pinned old read remains consistent",
                )
                self.assertEqual(
                    original_run(
                        [
                            "git",
                            "merge-base",
                            "--is-ancestor",
                            first_pointer,
                            data_commit,
                        ]
                    ).returncode,
                    0,
                )


if __name__ == "__main__":
    unittest.main()
