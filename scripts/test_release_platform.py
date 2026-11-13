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
            return copy.deepcopy(release)

        def run(*args, input=None):
            if args[:3] == ("gh", "release", "upload"):
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

    def test_older_completion_cannot_regress_platform_channel(self):
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
            with contextlib.chdir(work):
                m.run("git", "config", "user.email", "test@example.invalid")
                m.run("git", "config", "user.name", "Test")
                m.run("git", "commit", "--allow-empty", "-m", "old")
                old = m.run("git", "rev-parse", "HEAD")
                m.run("git", "commit", "--allow-empty", "-m", "new")
                new = m.run("git", "rev-parse", "HEAD")
                self.assertTrue(m.channel_advance("owner/repo", "linux-x86_64", new))
                self.assertFalse(m.channel_advance("owner/repo", "linux-x86_64", old))
                self.assertTrue(m.channel_advance("owner/repo", "linux-x86_64", new))
                self.assertTrue(
                    m.run(
                        "git",
                        "ls-remote",
                        "origin",
                        "refs/heads/release-ready/linux-x86_64",
                    ).startswith(new)
                )


if __name__ == "__main__":
    unittest.main()
