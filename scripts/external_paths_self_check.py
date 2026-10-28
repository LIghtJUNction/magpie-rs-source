#!/usr/bin/env python3
"""Small offline Git fixtures for both packaging and benchmark path checks."""

import contextlib
import importlib
import io
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
import external_paths as paths

licenses = importlib.import_module("collect-licenses")
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "benchmark"))
import forwarding as bench


class Controls(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(
            prefix="magpie-external-path-control-", dir="/tmp")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def git(self, *arguments):
        env = {key: value for key, value in os.environ.items()
               if not key.upper().startswith("GIT_")}
        env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
        return subprocess.run(
            ["git", "-c", "init.templateDir=", *map(str, arguments)],
            env=env, capture_output=True, check=True, timeout=5,
        )

    def repository(self, name="repo", *arguments):
        directory = self.root / name
        self.git("init", "--quiet", *arguments, directory)
        return directory

    def check(self, output, *, allowed):
        for entry in ("packaging", "benchmark"):
            with self.subTest(entry=entry, output=str(output), allowed=allowed):
                if entry == "packaging":
                    if allowed:
                        self.assertEqual(licenses.external_path(output), output.resolve())
                    else:
                        with self.assertRaises(ValueError):
                            licenses.external_path(output)
                else:
                    argv = ["forwarding.py", "--binary", sys.executable,
                            "--output", str(output)]
                    with patch.object(sys, "argv", argv), \
                            contextlib.redirect_stderr(io.StringIO()):
                        if allowed:
                            self.assertEqual(bench.parse_options().output, output.resolve())
                        else:
                            with self.assertRaises(SystemExit) as error:
                                bench.parse_options()
                            self.assertEqual(error.exception.code, 2)

    def test_real_unborn_repository_and_bare_repository(self):
        for name, arguments in (("normal", ()), ("bare", ("--bare",))):
            directory = self.repository(name, *arguments)
            self.check(directory / "new/deep/output.json", allowed=False)
        self.check(self.root / "outside/new/output.json", allowed=True)
        self.check(Path("relative/output.json"), allowed=False)

    def test_separate_git_directory_and_relative_or_absolute_gitfiles(self):
        metadata = self.root / "metadata"
        repository = self.repository("checkout", f"--separate-git-dir={metadata}")
        for target in (str(metadata), "../metadata"):
            (repository / ".git").write_text(f"gitdir: {target}\n")
            self.check(repository / "output.json", allowed=False)
        self.check(metadata / "new/output.json", allowed=False)

    def test_real_linked_worktree_with_shared_common_directory(self):
        repository = self.repository()
        linked = self.root / "linked"
        # An unborn linked worktree needs no synthetic or project commits.
        self.git("-C", repository, "worktree", "add", "--quiet", "--orphan",
                 "--relative-paths", linked)
        self.assertTrue((linked / ".git").is_file())
        self.check(linked / "new/output.json", allowed=False)

    def test_symlinks_into_and_out_of_repositories(self):
        repository = self.repository()
        outside = self.root / "outside"
        outside.mkdir()
        alias = self.root / "alias"
        alias.symlink_to(repository, target_is_directory=True)
        (repository / "external").symlink_to(outside, target_is_directory=True)
        plain_alias = self.root / "plain-alias"
        plain_alias.symlink_to(outside, target_is_directory=True)
        self.check(alias / "new/output.json", allowed=False)
        self.check(repository / "external/output.json", allowed=False)
        self.check(plain_alias / "new/output.json", allowed=True)
        self.assertEqual(list(outside.iterdir()), [])

    def test_symlinked_git_directory_is_still_a_repository(self):
        repository = self.repository()
        checkout = self.root / "checkout"
        checkout.mkdir()
        (checkout / ".git").symlink_to(repository / ".git", target_is_directory=True)
        self.check(checkout / "output.json", allowed=False)

    def test_empty_and_invalid_markers_allow_external_paths(self):
        for name, raw in (
            ("empty-directory", None), ("empty-file", b""), ("malformed", b"garbage\n"),
            ("empty-target", b"gitdir: \n"), ("missing-target", b"gitdir: missing\n"),
            ("file-target", b"gitdir: regular-file\n"),
            ("empty-target-directory", b"gitdir: empty\n"),
            ("nul-target", b"gitdir: invalid\0name\n"),
        ):
            directory = self.root / name
            directory.mkdir()
            (directory / "regular-file").write_text("not a directory")
            (directory / "empty").mkdir()
            if raw is None:
                (directory / ".git").mkdir()
            else:
                (directory / ".git").write_bytes(raw)
            self.check(directory / ".cache/magpie/new/output.json", allowed=True)
        broken = self.root / "broken-marker"
        broken.mkdir()
        (broken / ".git").symlink_to("missing")
        self.check(broken / "output.json", allowed=True)

    def test_nested_invalid_markers_do_not_hide_real_outer_repository(self):
        repository = self.repository()
        for name, raw in (("empty", None), ("malformed", b"invalid"),
                          ("missing", b"gitdir: missing\n")):
            inner = repository / name
            inner.mkdir()
            if raw is None:
                (inner / ".git").mkdir()
            else:
                (inner / ".git").write_bytes(raw)
            self.check(inner / "new/output.json", allowed=False)
        outer = self.root / "empty-outer"
        outer.mkdir()
        (outer / ".git").mkdir()
        inner = outer / "real-inner"
        self.git("init", "--quiet", inner)
        self.check(inner / "output.json", allowed=False)
        self.check(outer / "outside/output.json", allowed=True)

    def test_inherited_git_environment_cannot_redirect_probes(self):
        repository = self.repository()
        env = {"GIT_DIR": str(self.root / "missing"), "GIT_WORK_TREE": "/",
               "GIT_COMMON_DIR": str(self.root), "GIT_OBJECT_DIRECTORY": "/missing",
               "GIT_CONFIG_COUNT": "invalid", "GIT_CONFIG_PARAMETERS": "invalid",
               "GIT_CEILING_DIRECTORIES": "/", "GIT_CONFIG": "/missing"}
        with patch.dict(os.environ, env):
            self.assertTrue(paths._confirm_git_directory(repository / ".git"))
            self.check(repository / "output.json", allowed=False)
            self.check(self.root / "outside/output.json", allowed=True)

    def test_unavailable_git_fails_closed(self):
        repository = self.repository()
        with patch.dict(os.environ, {"PATH": ""}):
            self.check(repository / "output.json", allowed=False)
            with self.assertRaisesRegex(ValueError, "Cannot determine"):
                licenses.external_path(repository / "output.json")

    def test_git_errors_timeouts_and_invalid_success_fail_closed(self):
        repository = self.repository()
        outcomes = [
            {"side_effect": subprocess.TimeoutExpired("git", 5)},
            {"side_effect": PermissionError("synthetic execution denial")},
            {"return_value": subprocess.CompletedProcess("git", 128, b"", b"unreadable")},
            {"return_value": subprocess.CompletedProcess("git", 0, b"", b"")},
        ]
        for outcome in outcomes:
            with self.subTest(outcome=str(outcome)), \
                    patch.object(paths.subprocess, "run", **outcome):
                self.check(repository / "output.json", allowed=False)

    def test_unreadable_metadata_and_marker_fail_closed(self):
        repository = self.repository()
        original_stat = paths._stat
        def denied(path, **kwargs):
            if path == repository / ".git":
                raise PermissionError("synthetic metadata denial")
            return original_stat(path, **kwargs)
        with patch.object(paths, "_stat", side_effect=denied):
            self.check(repository / "output.json", allowed=False)
        checkout = self.root / "checkout"
        checkout.mkdir()
        (checkout / ".git").write_text(f"gitdir: {repository / '.git'}\n")
        with patch.object(Path, "open", side_effect=PermissionError("synthetic read denial")):
            self.check(checkout / "output.json", allowed=False)

    def test_damaged_plausible_git_directory_fails_closed(self):
        repository = self.repository()
        (repository / ".git/HEAD").write_text("damaged HEAD\n")
        self.check(repository / "output.json", allowed=False)
        with self.assertRaisesRegex(ValueError, "Cannot validate"):
            licenses.external_path(repository / "output.json")

    def test_special_or_oversized_markers_and_symlink_cycles_fail_closed(self):
        for name in ("fifo", "oversized", "cycle"):
            directory = self.root / name
            directory.mkdir()
            if name == "fifo":
                os.mkfifo(directory / ".git")
            elif name == "oversized":
                (directory / ".git").write_bytes(b"x" * 65537)
            else:
                (directory / ".git").symlink_to(".git")
            self.check(directory / "output.json", allowed=False)
        cycle = self.root / "output-cycle"
        cycle.symlink_to("output-cycle")
        self.check(cycle / "output.json", allowed=False)

    def test_explicit_root_and_existing_output_guards_remain(self):
        plain = self.root / "plain-source"
        plain.mkdir()
        with patch.object(licenses, "ROOT", plain), patch.object(bench, "ROOT", plain):
            self.check(plain / "new/output.json", allowed=False)
        existing = self.root / "existing"
        existing.mkdir()
        sentinel = existing / "keep"
        sentinel.write_text("unfinished work")
        with self.assertRaises(FileExistsError):
            licenses.new_directory(existing)
        # parse_options rejects existing files and final symlinks separately.
        alias = self.root / "final-alias"
        alias.symlink_to(sentinel)
        dangling = self.root / "final-dangling"
        dangling.symlink_to("missing-output")
        for output in (sentinel, alias, dangling):
            if output != sentinel:
                with self.assertRaises(ValueError):
                    licenses.new_directory(output)
            argv = ["forwarding.py", "--binary", sys.executable, "--output", str(output)]
            with patch.object(sys, "argv", argv), contextlib.redirect_stderr(io.StringIO()), \
                    self.assertRaises(SystemExit):
                bench.parse_options()
        self.assertEqual(sentinel.read_text(), "unfinished work")


if __name__ == "__main__":
    unittest.main()
