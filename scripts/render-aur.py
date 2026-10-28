#!/usr/bin/env python3
"""Render AUR source files only after the final release assets exist."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parents[1]


def checksum(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def inspect_archive(path, expected_version, desktop):
    with tarfile.open(path, "r:gz") as archive:
        prefix = path.name.removesuffix(".tar.gz")
        metadata = json.load(archive.extractfile(f"{prefix}/BUILD-METADATA.json"))
        required = {f"{prefix}/bin/magpie-desktop", f"{prefix}/share/applications/magpie-rs.desktop",
                    f"{prefix}/share/icons/hicolor/scalable/apps/magpie-rs.svg"} if desktop else {
                        f"{prefix}/bin/magpie", f"{prefix}/LICENSE"
                    }
        if not required.issubset(archive.getnames()):
            raise ValueError(f"{path} is missing required files")
        if (metadata["version"] != expected_version or metadata["gui"] != desktop
                or metadata["target"] != "x86_64-unknown-linux-gnu"):
            raise ValueError(f"Unexpected capabilities, version or target in {path}")
        return metadata


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--asset-dir", type=Path, required=True)
    parser.add_argument("--out-dir", type=Path, required=True)
    args = parser.parse_args()
    if not re.fullmatch(r"0\.\d+\.\d+", args.version):
        parser.error("Expected an early preview version, e.g. 0.2.0")
    out = args.out_dir.resolve()
    if out == ROOT or ROOT in out.parents:
        parser.error("Use an output directory outside the source checkout")
    cli = args.asset_dir / f"magpie-rs-{args.version}-x86_64-unknown-linux-gnu.tar.gz"
    gui = args.asset_dir / f"magpie-rs-gui-{args.version}-x86_64-unknown-linux-gnu.tar.gz"
    cli_meta = inspect_archive(cli, args.version, False)
    gui_meta = inspect_archive(gui, args.version, True)
    print(f"CLI source: {cli_meta['source_commit']} dirty={cli_meta['source_dirty']}")
    print(f"GUI source: {gui_meta['source_commit']} dirty={gui_meta['source_dirty']}")
    if cli_meta["source_commit"] != gui_meta["source_commit"] or cli_meta["source_dirty"] or gui_meta["source_dirty"]:
        print("WARNING: different commits or dirty build; review provenance before publishing")
    templates = ROOT / "packaging/aur"
    replacements = {
        "@VERSION@": args.version,
        "@CLI_SHA256@": checksum(cli),
        "@GUI_SHA256@": checksum(gui),
    }
    for name in ["magpie-rs-bin", "magpie-rs-git"]:
        destination = out / name
        destination.mkdir(parents=True, exist_ok=True)
        source = templates / name / ("PKGBUILD" if name == "magpie-rs-git" else "PKGBUILD.in")
        text = source.read_text()
        for key, value in replacements.items():
            text = text.replace(key, value)
        if re.search(r"@[A-Z_]+@", text):
            raise ValueError(f"Unfilled placeholder in {name}")
        (destination / "PKGBUILD").write_text(text)
        shutil.copyfile(templates / "LICENSE", destination / "LICENSE")
        result = subprocess.check_output(["makepkg", "--printsrcinfo"], cwd=destination, text=True)
        (destination / ".SRCINFO").write_text(result)
        print(destination)


if __name__ == "__main__":
    main()
