#!/usr/bin/env python3
"""Render AUR source files only after the final release assets exist."""

import argparse
import hashlib
import importlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parents[1]
licenses = importlib.import_module("collect-licenses")


def checksum(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def source_recipe(text):
    old = '''_target_dir() {
  printf '%s' "${MAGPIE_RS_TARGET_DIR:-${XDG_CACHE_HOME:-${HOME}/.cache}/magpie-rs-build/aur-git}"
}'''
    new = '''prepare() {
  cd "${srcdir}/magpie-rs-source"
  local state="${srcdir}/magpie-rs-build-state"
  python3 -B scripts/collect-licenses.py --reserve-build-dir "${state}" >/dev/null || return
  local target
  if [[ -n "${MAGPIE_RS_TARGET_DIR:-}" ]]; then
    target="$(python3 -B scripts/collect-licenses.py --reserve-build-dir "${MAGPIE_RS_TARGET_DIR}")" || return
  else
    target="$(python3 -B scripts/collect-licenses.py --allocate-build-dir)" || return
  fi
  printf '%s' "${target}" > "${state}/target"
  export CARGO_TARGET_DIR="${target}"
  export CARGO_BUILD_BUILD_DIR="${target}/intermediates"
  python3 -B scripts/collect-licenses.py --prepare-cache --cli --desktop
}

_target_dir() {
  cat "${srcdir}/magpie-rs-build-state/target"
}'''
    if text.count(old) != 1:
        raise ValueError("AUR source target-directory template changed; review it before rendering")
    text = text.replace(old, new)
    anchor = '  export CARGO_TARGET_DIR="$(_target_dir)"'
    if text.count(anchor) != 1:
        raise ValueError("AUR source build template changed; review it before rendering")
    return text.replace(anchor, '''  local target
  target="$(_target_dir)" || return
  export CARGO_TARGET_DIR="${target}"
  python3 -B scripts/collect-licenses.py --reserve-build-dir "${srcdir}/magpie-rs-build-state/build-started" >/dev/null || return
  export CARGO_BUILD_BUILD_DIR="${CARGO_TARGET_DIR}/intermediates"''')


def inspect_archive(path, expected_version, desktop):
    with tarfile.open(path, "r:gz") as archive:
        prefix = path.name.removesuffix(".tar.gz")
        members = {}
        for member in archive:
            if member.name in members:
                raise ValueError(f"Duplicate archive member: {member.name}")
            if (member.issym() or member.islnk() or member.name.startswith("/")
                    or ".." in Path(member.name).parts):
                raise ValueError(f"Unsafe archive member: {member.name}")
            members[member.name] = member

        def read(name, limit):
            member = members.get(f"{prefix}/{name}")
            if member is None or not member.isfile() or member.size > limit:
                raise ValueError(f"Missing or oversized release file: {name}")
            return archive.extractfile(member).read(limit + 1)

        metadata = json.loads(read("BUILD-METADATA.json", 2 * 1024 * 1024))
        required = {f"{prefix}/bin/magpie-desktop", f"{prefix}/share/applications/magpie-rs.desktop",
                    f"{prefix}/share/icons/hicolor/scalable/apps/magpie-rs.svg"} if desktop else {
                        f"{prefix}/bin/magpie", f"{prefix}/LICENSE"
                    }
        required.add(f"{prefix}/LICENSE")
        if not required.issubset(members) or any(not members[name].isfile() for name in required):
            raise ValueError(f"{path} is missing required files")
        if (metadata["version"] != expected_version or metadata["gui"] is not desktop
                or not desktop and metadata.get("cli") is not True
                or metadata["target"] != "x86_64-unknown-linux-gnu"):
            raise ValueError(f"Unexpected capabilities, version or target in {path}")
        declaration = metadata["third_party_notices"]
        if (declaration["incomplete_notices_allowed"] is not False
                or declaration["missing_license_file_count"] != 0
                or declaration.get("unresolved_source_code_count") != 0):
            raise ValueError("Diagnostic or incomplete notices cannot be published to AUR")
        raw_inventory = read(licenses.INVENTORY, 32 * 1024 * 1024)
        raw_notices = read(licenses.NOTICE, 64 * 1024 * 1024)
        if (declaration["inventory"] != licenses.INVENTORY
                or declaration["text"] != licenses.NOTICE
                or declaration["inventory_sha256"] != hashlib.sha256(raw_inventory).hexdigest()
                or declaration["text_sha256"] != hashlib.sha256(raw_notices).hexdigest()):
            raise ValueError("Release notice hashes do not match the archive")
        inventory = json.loads(raw_inventory)
        packages = inventory["packages"]
        if (inventory["schema_version"] != 2 or inventory["target"] != metadata["target"]
                or inventory["missing_license_files"] or inventory.get("unresolved_source_code") != []
                or not packages
                or declaration["package_count"] != len(packages)
                or inventory["notices"]["sha256"] != declaration["text_sha256"]
                or any(package["missing_license_file"] is not False or not package["texts"]
                       or package["license_text_status"] in {"unknown-license", "unresolved-known-license"}
                       or package.get("source_code", {}).get("kind") != "local"
                       and not package.get("source_code", {}).get("url")
                       for package in packages)):
            raise ValueError("Notice inventory is incomplete or inconsistent")
        for package in packages:
            for text in package["texts"]:
                offset, length = text["notice_offset"], text["notice_length"]
                if (type(offset) is not int or type(length) is not int
                        or offset < 0 or length <= 0 or offset + length > len(raw_notices)
                        or hashlib.sha256(raw_notices[offset:offset + length]).hexdigest() != text["sha256"]):
                    raise ValueError("Notice inventory contains invalid text ranges or hashes")
        for label, binary in (("cli", "magpie"), ("desktop", "magpie-desktop")):
            if label == "desktop" and not desktop or label == "cli" and not metadata.get("cli"):
                continue
            member = members.get(f"{prefix}/bin/{binary}")
            if member is None or not member.isfile():
                raise ValueError(f"Missing binary: {binary}")
            actual = hashlib.file_digest(archive.extractfile(member), "sha256").hexdigest()
            if (metadata["binaries"][binary] != actual
                    or inventory["binary_sha256"][label] != actual):
                raise ValueError(f"Notice inventory identifies a different binary: {binary}")
        return metadata


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--asset-dir", type=Path, required=True)
    parser.add_argument("--out-dir", type=Path, required=True)
    args = parser.parse_args()
    if not re.fullmatch(r"0\.\d+\.\d+", args.version):
        parser.error("Expected an early preview version, e.g. 0.2.0")
    out = licenses.external_path(args.out_dir)
    if out.exists():
        parser.error("Use a new exclusive output directory")
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
    licenses.new_directory(out)
    for name in ["magpie-rs-bin", "magpie-rs-git"]:
        destination = out / name
        destination.mkdir(parents=True, exist_ok=True)
        source = templates / name / ("PKGBUILD" if name == "magpie-rs-git" else "PKGBUILD.in")
        text = source.read_text()
        if name == "magpie-rs-git":
            text = source_recipe(text)
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
    try:
        main()
    except (OSError, ValueError, KeyError, TypeError, tarfile.TarError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"AUR rendering failed: {error}")
