#!/usr/bin/env python3
"""Exercise artifact identity derivation without compiling the workspace."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


def run(*args, cwd=None, env=None):
    return subprocess.check_output(args, cwd=cwd, env=env, text=True).strip()


class BuildIdentity(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.root = Path(cls.temp.name)
        source = cls.root / "probe.rs"
        source.write_text(
            f'#[path = "{ROOT / "build-support/identity.rs"}"] mod identity;\n'
            "fn main() { identity::emit(); }\n"
        )
        cls.probe = cls.root / "probe"
        run("rustc", "--edition=2021", str(source), "-o", str(cls.probe))
        cls.repo = cls.root / "repo"
        cls.repo.mkdir()
        run("git", "init", "-q", str(cls.repo))
        run("git", "config", "user.name", "Fixture", cwd=cls.repo)
        run("git", "config", "user.email", "fixture@example.test", cwd=cls.repo)
        (cls.repo / "file").write_text("source identical across commits")
        run("git", "add", "file", cwd=cls.repo)
        run("git", "commit", "-qm", "first", cwd=cls.repo)
        run("git", "commit", "--allow-empty", "-qm", "second", cwd=cls.repo)

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def identity(self, cwd):
        output = run(
            str(self.probe), cwd=cwd,
            env=dict(os.environ, CARGO_PKG_VERSION_MAJOR="2", CARGO_PKG_VERSION_MINOR="15", CARGO_PKG_VERSION_PATCH="17"),
        )
        return dict(line.removeprefix("cargo:rustc-env=").split("=", 1)
                    for line in output.splitlines() if line.startswith("cargo:rustc-env=")), output

    def test_full_history_and_worktree_identity(self):
        info, _ = self.identity(self.repo)
        self.assertEqual(info["PORTAL_VERSION"], "2.15.2")
        self.assertEqual(info["PORTAL_GIT_HASH"], run("git", "rev-parse", "--short", "HEAD", cwd=self.repo))
        worktree = self.root / "worktree"
        run("git", "worktree", "add", "--detach", str(worktree), "HEAD~1", cwd=self.repo)
        info, output = self.identity(worktree)
        self.assertEqual(info["PORTAL_VERSION"], "2.15.1")
        self.assertEqual(info["PORTAL_GIT_HASH"], run("git", "rev-parse", "--short", "HEAD", cwd=worktree))
        gitdir = run("git", "rev-parse", "--absolute-git-dir", cwd=worktree)
        self.assertIn(f"cargo:rerun-if-changed={gitdir}/HEAD", output)

    def test_shallow_uses_manifest_patch_but_current_revision(self):
        shallow = self.root / "shallow"
        run("git", "clone", "-q", "--depth=1", self.repo.as_uri(), str(shallow))
        info, _ = self.identity(shallow)
        self.assertEqual(info["PORTAL_VERSION"], "2.15.17")
        self.assertEqual(info["PORTAL_GIT_HASH"], run("git", "rev-parse", "--short", "HEAD", cwd=shallow))

    def test_source_tarball_has_explicit_unknown_revision(self):
        tarball = self.root / "tarball"
        tarball.mkdir()
        info, _ = self.identity(tarball)
        self.assertEqual(info["PORTAL_VERSION"], "2.15.17")
        self.assertEqual(info["PORTAL_GIT_HASH"], "unknown")
        self.assertTrue(info["PORTAL_BUILD_TIME"])


if __name__ == "__main__":
    unittest.main()
