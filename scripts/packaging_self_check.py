#!/usr/bin/env python3
"""Small isolated packaging controls; no Cargo, network or native build tools."""

import concurrent.futures
import contextlib
import hashlib
import importlib
import io
import json
import os
from pathlib import Path
import sys
import subprocess
import shutil
import tarfile
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
licenses = importlib.import_module("collect-licenses")
release = importlib.import_module("package-release")
aur = importlib.import_module("render-aur")


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


class Controls(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="magpie-packaging-control-", dir="/tmp")
        self.root = Path(self.temporary.name)
        self.addCleanup(self.temporary.cleanup)

    def test_checkout_paths_relative_paths_and_symlinks_are_rejected(self):
        repository = self.root / "recipe"
        source = repository / "generated" / "source"
        source.mkdir(parents=True)
        subprocess.run(["git", "init", "--quiet", str(repository)], check=True)
        (source / ".git").write_text("gitdir: elsewhere")
        alias = self.root / "alias"
        alias.symlink_to(repository, target_is_directory=True)
        for path in (Path("relative-output"), repository / "dist", source / "dist", alias / "dist"):
            with self.subTest(path=path), self.assertRaises(ValueError):
                licenses.new_directory(path)
        self.assertFalse((repository / "dist").exists())

    def test_existing_and_concurrent_directory_reservations_fail(self):
        output = self.root / "exclusive"
        def reserve(_):
            try:
                return licenses.new_directory(output)
            except FileExistsError:
                return None
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(reserve, range(2)))
        self.assertEqual(sum(result is not None for result in results), 1)
        sentinel = output / "keep"
        sentinel.write_bytes(b"existing work")
        with self.assertRaises(FileExistsError):
            licenses.new_directory(output)
        self.assertEqual(sentinel.read_bytes(), b"existing work")

    def test_cache_preparation_fetches_both_manifests_without_target_or_build(self):
        for directory in (self.root, self.root / "apps/desktop"):
            directory.mkdir(parents=True, exist_ok=True)
            (directory / "Cargo.lock").write_text("synthetic lock")
        with patch.object(licenses, "ROOT", self.root), patch.object(licenses, "command") as command:
            licenses.prepare_cache(True, True)
        self.assertEqual(command.call_count, 2)
        for call in command.call_args_list:
            self.assertEqual(call.args[:3], ("cargo", "fetch", "--locked"))
            self.assertNotIn("--target", call.args)
            self.assertNotIn("build", call.args)

    def make_archive(self, metadata_change=None, inventory_change=None, omitted=(), corrupt=(),
                     desktop=False, extra_members=()):
        name = f"magpie-rs-{'gui-' if desktop else ''}0.2.0-x86_64-unknown-linux-gnu"
        binary_name = "magpie-desktop" if desktop else "magpie"
        binary_label = "desktop" if desktop else "cli"
        binary = b"synthetic CLI bytes"
        notice = b"Original synthetic license text\n"
        inventory = {
            "schema_version": 2, "target": "x86_64-unknown-linux-gnu",
            "unresolved_source_code": [],
            "missing_license_files": [], "binary_sha256": {binary_label: digest(binary)},
            "notices": {"sha256": digest(notice)}, "packages": [{
                "missing_license_file": False, "texts": [{
                    "path": "LICENSE", "notice_offset": 0, "notice_length": len(notice),
                    "sha256": digest(notice),
                }],
                "license_text_status": "source-files",
                "source_code": {"kind": "published-crate",
                                "url": "https://crates.io/api/v1/crates/synthetic/1.0.0/download"},
            }],
        }
        if inventory_change:
            inventory_change(inventory)
        raw_inventory = json.dumps(inventory).encode()
        metadata = {
            "version": "0.2.0", "target": inventory["target"], "gui": desktop, "cli": not desktop,
            "source_commit": "a" * 40, "source_dirty": False,
            "binaries": {binary_name: digest(binary)}, "third_party_notices": {
                "inventory": licenses.INVENTORY, "text": licenses.NOTICE,
                "incomplete_notices_allowed": False, "missing_license_file_count": 0,
                "unresolved_source_code_count": 0,
                "package_count": 1, "inventory_sha256": digest(raw_inventory),
                "text_sha256": digest(notice),
            },
        }
        if metadata_change:
            metadata_change(metadata)
        files = {"BUILD-METADATA.json": json.dumps(metadata).encode(), f"bin/{binary_name}": binary,
                 "LICENSE": b"synthetic project license", licenses.INVENTORY: raw_inventory,
                 licenses.NOTICE: notice}
        if desktop:
            files.update({"share/applications/magpie-rs.desktop": b"synthetic entry",
                          "share/icons/hicolor/scalable/apps/magpie-rs.svg": b"synthetic icon"})
        output = self.root / (name + ".tar.gz")
        with tarfile.open(output, "w:gz") as archive:
            for filename, raw in files.items():
                if filename in omitted:
                    continue
                if filename in corrupt:
                    raw += b"altered"
                info = tarfile.TarInfo(f"{name}/{filename}")
                info.size = len(raw)
                archive.addfile(info, io.BytesIO(raw))
            for info in extra_members:
                archive.addfile(info)
        return output

    def test_complete_archive_passes(self):
        archive = self.make_archive()
        self.assertFalse(aur.inspect_archive(archive, "0.2.0", False)["gui"])
        self.assertTrue(aur.inspect_archive(self.make_archive(desktop=True), "0.2.0", True)["gui"])

    def test_aur_rejects_missing_tampered_diagnostic_and_inconsistent_notices(self):
        cases = [
            {"omitted": (licenses.NOTICE,)},
            {"corrupt": (licenses.NOTICE,)},
            {"corrupt": (licenses.INVENTORY,)},
            {"corrupt": ("bin/magpie",)},
            {"metadata_change": lambda m: m["third_party_notices"].update(incomplete_notices_allowed=True)},
            {"metadata_change": lambda m: m["third_party_notices"].update(missing_license_file_count=1)},
            {"inventory_change": lambda i: i["missing_license_files"].append("unresolved")},
            {"inventory_change": lambda i: i["packages"][0].update(missing_license_file=True)},
            {"inventory_change": lambda i: i["packages"][0].update(texts=[])},
            {"inventory_change": lambda i: i["unresolved_source_code"].append("unknown")},
            {"inventory_change": lambda i: i["packages"][0].update(source_code={})},
            {"inventory_change": lambda i: i["packages"][0]["texts"][0].update(notice_offset=-1)},
            {"inventory_change": lambda i: i["packages"][0]["texts"][0].update(sha256="wrong")},
        ]
        for case in cases:
            with self.subTest(case=case), self.assertRaises(ValueError):
                aur.inspect_archive(self.make_archive(**case), "0.2.0", False)

    def test_packager_collects_into_separate_exclusive_directory(self):
        from types import SimpleNamespace
        payload = self.root / "payload"
        payload.mkdir()
        args = SimpleNamespace(target="synthetic", binary=None, desktop_binary=None,
                               allow_incomplete_notices=False)
        def collector(command, check):
            destination = Path(command[command.index("--out-dir") + 1])
            licenses.new_directory(destination)
            (destination / licenses.NOTICE).write_text("synthetic")
            (destination / licenses.INVENTORY).write_text("{}")
        with patch.object(release.subprocess, "run", side_effect=collector):
            self.assertEqual(release.collect_notices(payload, args, ""), {})
        self.assertEqual((payload / licenses.NOTICE).read_text(), "synthetic")

    def test_official_source_urls_keep_exact_versions_and_revisions(self):
        package = {"name": "example", "version": "1.2.3+build",
                   "source": "registry+https://github.com/rust-lang/crates.io-index"}
        result = licenses.source_code(package, "locked-checksum")
        self.assertEqual(result["url"], "https://crates.io/api/v1/crates/example/1.2.3%2Bbuild/download")
        self.assertEqual(result["sha256"], "locked-checksum")
        package["source"] = "git+https://github.com/owner/repo.git?rev=main#" + "a" * 40
        self.assertEqual(licenses.source_code(package, None)["url"],
                         "https://github.com/owner/repo/tree/" + "a" * 40)
        package["source"] = "registry+https://unreviewed.example/index"
        self.assertNotIn("url", licenses.source_code(package, "checksum"))

    def test_rendered_source_recipe_prepares_cache_and_refuses_shared_targets(self):
        original = (aur.ROOT / "packaging/aur/magpie-rs-git/PKGBUILD").read_text()
        text = aur.source_recipe(original)
        self.assertIn("--prepare-cache --cli --desktop", text)
        self.assertIn("--allocate-build-dir", text)
        self.assertIn("build-started", text)
        self.assertNotIn("/magpie-rs-build/aur-git", text)
        subprocess.run(["bash", "-n"], input=text.encode(), check=True)

    def test_collection_pipeline_with_synthetic_metadata_and_no_cargo(self):
        from types import SimpleNamespace
        repository = self.root / "source"
        (repository / "packaging").mkdir(parents=True)
        (repository / "Cargo.toml").write_text('[package]\nname="magpie-cli"\nversion="0.0.0"\n')
        (repository / "Cargo.lock").write_text('version=3\n[[package]]\nname="magpie-cli"\nversion="0.0.0"\n')
        (repository / "LICENSE").write_text("Synthetic fixture license\n")
        supplement = repository / "packaging/license-supplements.json"
        supplement.write_text('{"schema_version":1,"packages":{},"texts":{}}')
        metadata = {"packages": [{
            "id": "root", "name": "magpie-cli", "version": "0.0.0", "source": None,
            "manifest_path": str(repository / "Cargo.toml"), "license": "MIT",
            "license_file": None, "targets": [{"kind": ["bin"]}],
        }], "resolve": {"nodes": [{"id": "root", "deps": [], "features": []}]}}
        def command(*args, env=None):
            if args[0] == "cargo":
                self.assertEqual(args[:2], ("cargo", "metadata"))
                self.assertIn("--offline", args)
                self.assertIn("--locked", args)
                self.assertTrue(Path(env["CARGO_TARGET_DIR"]).is_dir())
                json.loads(args[args.index("--config") + 1].split("=", 1)[1])
                return json.dumps(metadata).encode()
            if "ls-files" in args:
                return b"Cargo.toml\0LICENSE\0"
            if "rev-parse" in args:
                return b"a" * 40
            if "status" in args:
                return b""
            self.fail(f"Unexpected command: {args}")
        args = SimpleNamespace(cli=True, desktop=False, binary=None, desktop_binary=None, target="synthetic")
        with patch.object(licenses, "ROOT", repository), patch.object(licenses, "SUPPLEMENTS", supplement), \
                patch.object(licenses, "command", side_effect=command):
            result = licenses.collect(args, self.root / "notices")
        self.assertEqual(result["missing_license_files"], [])
        self.assertEqual(result["unresolved_source_code"], [])
        self.assertEqual(result["roots"][0]["package_count"], 1)
        raw = (self.root / "notices" / licenses.NOTICE).read_bytes()
        for segment in result["packages"][0]["texts"]:
            start, length = segment["notice_offset"], segment["notice_length"]
            self.assertEqual(digest(raw[start:start + length]), segment["sha256"])

    def test_build_orchestrator_uses_owned_directory_and_preserves_existing_work(self):
        from types import SimpleNamespace
        import contextlib
        target = self.root / "build"
        args = SimpleNamespace(target="x86_64-unknown-linux-gnu", with_desktop=True)
        with patch.dict("os.environ", {"MAGPIE_RS_TARGET_DIR": str(target)}), \
                patch.object(licenses, "prepare_cache") as preparation, \
                patch.object(release.subprocess, "run") as build:
            with contextlib.ExitStack() as stack:
                release.build_binaries(args, stack)
                self.assertTrue(target.is_dir())
                self.assertEqual(build.call_count, 2)
                self.assertEqual(preparation.call_args.kwargs["env"]["CARGO_TARGET_DIR"], str(target))
                for call in build.call_args_list:
                    self.assertEqual(call.kwargs["env"]["CARGO_TARGET_DIR"], str(target))
            self.assertFalse(target.exists())
            target.mkdir()
            (target / "keep").write_text("unfinished work")
            with contextlib.ExitStack() as stack, self.assertRaises(FileExistsError):
                release.build_binaries(args, stack)
            self.assertEqual((target / "keep").read_text(), "unfinished work")

    def test_generated_aur_functions_run_only_synthetic_fetches_and_reject_reuse(self):
        source = self.root / "src/magpie-rs-source"
        (source / "scripts").mkdir(parents=True)
        (source / ".git").mkdir()
        (source / "apps/desktop").mkdir(parents=True)
        for path in (source, source / "apps/desktop"):
            (path / "Cargo.lock").write_text("synthetic lock")
        shutil.copyfile(licenses.__file__, source / "scripts/collect-licenses.py")
        shutil.copyfile(Path(licenses.__file__).with_name("external_paths.py"),
                        source / "scripts/external_paths.py")
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        cargo = bin_dir / "cargo"
        cargo.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$CONTROL_LOG"\n')
        cargo.chmod(0o700)
        temporary = self.root / "temp"
        temporary.mkdir()
        recipe = self.root / "PKGBUILD"
        recipe.write_text(aur.source_recipe((aur.ROOT / "packaging/aur/magpie-rs-git/PKGBUILD").read_text()))
        log = self.root / "commands"
        env = {**os.environ, "PATH": str(bin_dir) + os.pathsep + os.environ["PATH"],
               "TMPDIR": str(temporary), "CONTROL_LOG": str(log)}
        env.pop("MAGPIE_RS_TARGET_DIR", None)
        # Exercise without errexit too: each failed reservation must propagate.
        script = 'source "$1"; srcdir="$2"; prepare && build'
        result = subprocess.run(["bash", "-c", script, "control", str(recipe), str(source.parent)],
                                env=env, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        commands = log.read_text().splitlines()
        self.assertEqual(len(commands), 4)
        self.assertTrue(all(line.startswith("fetch --locked") and "--target" not in line for line in commands[:2]))
        target = Path((source.parent / "magpie-rs-build-state/target").read_text())
        self.assertTrue(target.is_relative_to(temporary))
        again = subprocess.run(["bash", "-c", script, "control", str(recipe), str(source.parent)],
                               env=env, capture_output=True)
        self.assertNotEqual(again.returncode, 0)
        self.assertEqual(log.read_text().splitlines(), commands)
        second_build = subprocess.run(
            ["bash", "-c", 'source "$1"; srcdir="$2"; build', "control", str(recipe), str(source.parent)],
            env=env, capture_output=True)
        self.assertNotEqual(second_build.returncode, 0)
        self.assertEqual(log.read_text().splitlines(), commands)

    def test_aur_inspects_both_archives_before_writing_and_generates_both_recipes(self):
        self.make_archive()
        self.make_archive(desktop=True, metadata_change=lambda m: m["third_party_notices"].update(
            incomplete_notices_allowed=True))
        destination = self.root / "rendered"
        argv = ["render-aur.py", "--version", "0.2.0", "--asset-dir", str(self.root),
                "--out-dir", str(destination)]
        with patch.object(sys, "argv", argv), self.assertRaises(ValueError):
            aur.main()
        self.assertFalse(destination.exists())
        self.make_archive(desktop=True)
        with patch.object(sys, "argv", argv), patch.object(aur.subprocess, "check_output",
                return_value="synthetic srcinfo\n"), contextlib.redirect_stdout(io.StringIO()):
            aur.main()
        for name in ("magpie-rs-bin", "magpie-rs-git"):
            self.assertTrue((destination / name / "PKGBUILD").is_file())
            self.assertEqual((destination / name / ".SRCINFO").read_text(), "synthetic srcinfo\n")
            recipe = (destination / name / "PKGBUILD").read_text()
            # These packages install the same commands and must exclude one
            # another, including the older split CLI package.
            conflicts = next(line for line in recipe.splitlines() if line.startswith("conflicts="))
            for package in {"magpie", "magpie-git", "magpie-bin", "magpie-rs",
                            "magpie-rs-bin", "magpie-rs-git", "magpie-rs-cli"} - {name}:
                self.assertIn(f"'{package}'", conflicts)
            provides = next(line for line in recipe.splitlines() if line.startswith("provides="))
            for package in ("magpie", "magpie-rs"):
                self.assertIn(f"'{package}'", provides)
            # Shared-core keyring support links libdbus in both executables;
            # the rendered package must install it even for CLI-only use.
            dependencies = subprocess.check_output(
                ["bash", "-c", 'source "$1"; printf "%s\\n" "${depends[@]}"',
                 "bash", str(destination / name / "PKGBUILD")], text=True,
            ).splitlines()
            self.assertIn("dbus", dependencies)
        with patch.object(sys, "argv", argv), self.assertRaises(SystemExit), \
                contextlib.redirect_stderr(io.StringIO()):
            aur.main()

    def test_aur_rejects_duplicate_and_linked_members(self):
        prefix = "magpie-rs-0.2.0-x86_64-unknown-linux-gnu/"
        duplicate = tarfile.TarInfo(prefix + "LICENSE")
        link = tarfile.TarInfo(prefix + "other")
        link.type, link.linkname = tarfile.SYMTYPE, "LICENSE"
        for member in (duplicate, link):
            with self.subTest(member=member.name), self.assertRaises(ValueError):
                aur.inspect_archive(self.make_archive(extra_members=(member,)), "0.2.0", False)


if __name__ == "__main__":
    unittest.main()
