#!/usr/bin/env python3
"""Collect locked, target-specific Cargo notices without building or downloading."""

import argparse
import contextlib
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import re
import os
import subprocess
import sys
import tarfile
import tempfile
import tomllib
from urllib.parse import quote, urlsplit, urlunsplit

from external_paths import external_path as checked_external_path

ROOT = Path(__file__).resolve().parents[1]
NOTICE = "THIRD-PARTY-NOTICES.txt"
INVENTORY = "THIRD-PARTY-LICENSES.json"
SUPPLEMENTS = ROOT / "packaging/license-supplements.json"
NOTICE_NAME = re.compile(
    r"^(?:licen[cs]es?|copying|copyright|notices?|unlicen[cs]e)(?:$|[._-])",
    re.IGNORECASE,
)
LICENSE_TEXT_NAME = re.compile(
    r"^(?:apache(?:-2\.0)?|mit|bsd(?:-[23]-clause)?|ofl|ftl|[al]?gpl(?:v?\d(?:\.\d)?)?)"
    r"(?:\.(?:txt|md|rst))?$",
    re.IGNORECASE,
)
TEXT_SUFFIXES = {
    "", ".txt", ".md", ".rst", ".html", ".apache", ".mit", ".bsd",
    ".unicode", ".zlib", ".gpl", ".lgpl", ".ofl", ".chromium",
}


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def command(*args, env=None):
    return subprocess.check_output([str(arg) for arg in args], env=env)


def external_path(path):
    return checked_external_path(path, root=ROOT)


def new_directory(path):
    if Path(path).is_symlink():
        raise ValueError(f"Output path already exists as a symlink: {path}")
    path = external_path(path)
    # mkdir is the reservation: existing, shared and stale directories all fail.
    path.mkdir(parents=True, exist_ok=False)
    return path


@contextlib.contextmanager
def temporary_directory(prefix):
    base = external_path(Path(tempfile.gettempdir()))
    with tempfile.TemporaryDirectory(prefix=prefix, dir=base) as scratch:
        yield Path(scratch)


def prepare_cache(cli, desktop, env=None):
    manifests = ([ROOT / "Cargo.toml"] if cli else []) + (
        [ROOT / "apps/desktop/Cargo.toml"] if desktop else []
    )
    for manifest in manifests:
        before = manifest.with_name("Cargo.lock").read_bytes()
        # metadata's packages include other platforms even with --filter-platform.
        command("cargo", "fetch", "--locked", "--manifest-path", manifest, env=env)
        if manifest.with_name("Cargo.lock").read_bytes() != before:
            raise ValueError(f"Lockfile changed during fetch: {manifest}")


def notice_path(path):
    path = PurePosixPath(path)
    return path.suffix.lower() in TEXT_SUFFIXES and (
        NOTICE_NAME.match(path.name)
        or LICENSE_TEXT_NAME.fullmatch(path.name)
        or any(part.lower() in {"licenses", "licences"} for part in path.parts[:-1])
    )


def license_path(path):
    return re.match(r"^(?:licen[cs]e|copying|unlicen[cs]e)", path.name, re.I) is not None


def package_key(package):
    source = package["source"]
    if source is None:
        directory = Path(package["manifest_path"]).resolve().parent
        source = "path:" + directory.relative_to(ROOT).as_posix()
    return f"{package['name']}@{package['version']} ({source})"


def source_code(package, checksum):
    source = package["source"]
    if source == "registry+https://github.com/rust-lang/crates.io-index":
        name, version = quote(package["name"], safe=""), quote(package["version"], safe="")
        return {
            "kind": "published-crate", "registry": source,
            "url": f"https://crates.io/api/v1/crates/{name}/{version}/download",
            "sha256": checksum, "repository_declared_by_manifest": package.get("repository"),
            "evidence": "Cached published archive verified against Cargo.lock; URL is the official versioned crates.io endpoint.",
        }
    if source and source.startswith("git+"):
        url = urlsplit(source[4:])
        revision = url.fragment
        repository = urlunsplit((url.scheme, url.netloc, url.path, "", ""))
        result = {"kind": "git", "repository": repository, "revision": revision,
                  "evidence": "License files read from Git objects at this locked revision."}
        if url.hostname == "github.com" and re.fullmatch(r"[0-9a-f]{40}", revision):
            result["url"] = repository.removesuffix(".git") + "/tree/" + revision
        return result
    if source is None:
        return {"kind": "local", "manifest": Path(package["manifest_path"]).resolve().relative_to(ROOT).as_posix(),
                "evidence": "Current checkout; source_commit/source_dirty describe it, not a verified published source archive."}
    return {"kind": "unresolved", "registry": source,
            "evidence": "No official versioned source download endpoint has been established."}


def source_path(path):
    path = PurePosixPath(path)
    if path.is_absolute():
        raise ValueError(f"Absolute path in source license reference: {path}")
    parts = []
    for part in path.parts:
        if part == "..":
            if not parts:
                raise ValueError(f"License reference escapes its source: {path}")
            parts.pop()
        elif part != ".":
            parts.append(part)
    return PurePosixPath(*parts).as_posix()


def dependency_closure(metadata, package_name):
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    root = next(
        package["id"] for package in packages.values()
        if package["name"] == package_name and package["source"] is None
    )
    roles = {}
    pending = [(root, "runtime")]
    while pending:
        identifier, role = pending.pop()
        package = packages[identifier]
        if any("proc-macro" in target["kind"] for target in package["targets"]):
            role = "build"
        if role in roles.get(identifier, set()):
            continue
        roles.setdefault(identifier, set()).add(role)
        for dependency in nodes[identifier]["deps"]:
            for kind in dependency["dep_kinds"]:
                if kind["kind"] in (None, "build"):
                    pending.append((
                        dependency["pkg"],
                        "build" if role == "build" or kind["kind"] == "build" else "runtime",
                    ))
    return packages, nodes, roles


class Sources:
    def __init__(self):
        self.git_trees = {}

    def files(self, package, checksum):
        directory = Path(package["manifest_path"]).resolve().parent
        source = package["source"]
        if source is None:
            directory.relative_to(ROOT)
            names = command("git", "-C", ROOT, "ls-files", "-z").decode().split("\0")

            def read(name):
                path = (ROOT / name).resolve()
                path.relative_to(ROOT)
                return path.read_bytes(), path.relative_to(ROOT).as_posix()

            return PurePosixPath(directory.relative_to(ROOT).as_posix()), {
                name for name in names if name
            }, read
        if source.startswith("registry+"):
            # Read the actual cached crate archive, checked against Cargo.lock.
            archive_path = directory.parents[2] / "cache" / directory.parent.name / (
                directory.name + ".crate"
            )
            raw = archive_path.read_bytes()
            if not checksum or sha256(raw) != checksum:
                raise ValueError(f"Cached crate checksum differs from Cargo.lock: {package['name']}")
            archive = tarfile.open(fileobj=io.BytesIO(raw), mode="r:gz")
            members = {}
            prefix = directory.name + "/"
            for member in archive.getmembers():
                path = PurePosixPath(member.name)
                if path.is_absolute() or ".." in path.parts or not member.name.startswith(prefix):
                    raise ValueError(f"Unsafe path in cached crate: {package['name']}")
                if member.isfile():
                    members[member.name[len(prefix):]] = member

            def read(name):
                return archive.extractfile(members[name]).read(), name

            return PurePosixPath("."), set(members), read
        if not source.startswith("git+"):
            raise ValueError(f"Unsupported Cargo source: {source}")
        revision = source.rsplit("#", 1)[-1]
        if not re.fullmatch(r"[0-9a-f]{40}", revision):
            raise ValueError(f"Git source has no immutable revision: {source}")
        repository = Path(command("git", "-C", directory, "rev-parse", "--show-toplevel").decode().strip())
        cache_key = (repository, revision)
        if cache_key not in self.git_trees:
            tree = command("git", "-C", repository, "ls-tree", "-rz", revision)
            self.git_trees[cache_key] = {
                entry.split(b"\t", 1)[1].decode(): entry.split(b" ", 1)[0].decode()
                for entry in tree.split(b"\0") if entry
            }
        modes = self.git_trees[cache_key]
        names = {name for name, mode in modes.items() if mode in {"100644", "100755", "120000"}}

        def read(name):
            visited = set()
            while name not in visited:
                visited.add(name)
                if name not in names:
                    raise ValueError(f"Git license reference missing from locked source: {name}")
                raw = command("git", "-C", repository, "show", f"{revision}:{name}")
                if modes[name] != "120000":
                    return raw, name
                name = source_path(PurePosixPath(name).parent / raw.decode("utf-8"))
            raise ValueError(f"Git license symlink cycle: {name}")

        return PurePosixPath(directory.relative_to(repository).as_posix()), names, read


def collect_texts(package, checksum, sources):
    directory, names, read = sources.files(package, checksum)
    selected = {
        name for name in names
        if PurePosixPath(name).is_relative_to(directory) and notice_path(name)
    }
    primary = {
        name for name in selected
        if license_path(PurePosixPath(name)) and (
            PurePosixPath(name).parent == directory
            or PurePosixPath(name).relative_to(directory).parts[0].lower() in {"licenses", "licences"}
        )
    }
    declared = package["license_file"]
    if declared:
        path = directory / PurePosixPath(declared.replace("\\", "/"))
        name = source_path(path)
        if name not in names:
            raise ValueError(f"Declared license_file missing from source: {package['name']}: {declared}")
        selected.add(name)
        primary.add(name)
    if not primary and (package["source"] is None or package["source"].startswith("git+")):
        # In a Git monorepo, inherit only from the nearest ancestor containing a
        # license; never borrow a different crate's text or the Cargo cache root.
        for parent in directory.parents:
            inherited = {name for name in names if PurePosixPath(name).parent == parent and notice_path(name)}
            licenses = {name for name in inherited if license_path(PurePosixPath(name))}
            if licenses:
                selected.update(inherited)
                primary.update(licenses)
                break
    texts = []
    for name in sorted(selected):
        raw, origin = read(name)
        texts.append((name, origin, "file", raw))
    if not primary:
        for name in sorted(names):
            path = PurePosixPath(name)
            if path.parent != directory or not path.name.lower().startswith("readme"):
                continue
            raw, origin = read(name)
            section = re.search(
                rb"(?im)^(#{1,6})[ \t]+(?:licen[cs]e|copyright)[^\r\n]*\r?$", raw
            )
            if section:
                rest = raw[section.end():]
                end = re.search(rb"(?m)^#{1," + str(len(section[1])).encode() + rb"}[ \t]+", rest)
                text = raw[section.start():section.end() + end.start()] if end else raw[section.start():]
                texts.append((name, origin, "readme-license-section", text))
    return texts, not bool(primary)


def load_supplements():
    raw = SUPPLEMENTS.read_bytes()
    data = json.loads(raw)
    if data["schema_version"] != 1:
        raise ValueError("Unsupported license supplement schema")
    for expected, text in data["texts"].items():
        if sha256(text.encode("utf-8")) != expected:
            raise ValueError(f"License supplement text hash differs: {expected}")
    return data, sha256(raw)


def supplement_texts(package, checksum, sources, data):
    binding = data["packages"].get(f"{package['name']}@{package['version']}")
    if binding is None:
        return [], None
    for field, actual in (
        ("source", package["source"]), ("license", package["license"]),
        ("crate_sha256", checksum),
    ):
        if binding[field] != actual:
            raise ValueError(f"License supplement {field} differs: {package_key(package)}")
    _, names, read = sources.files(package, checksum)
    if json.loads(read(".cargo_vcs_info.json")[0]) != binding["archive_vcs"]:
        raise ValueError(f"License supplement VCS identity differs: {package_key(package)}")
    for check in binding["archive_checks"]:
        path = check["path"]
        if path not in names or sha256(read(path)[0]) != check["sha256"]:
            raise ValueError(f"License supplement archive evidence differs: {package['name']}: {path}")
    texts = []
    for item in binding["files"]:
        raw = data["texts"][item["sha256"]].encode("utf-8")
        texts.append((item["path"], item["path"], item["kind"], raw))
    if not any(item["role"] == "license-text" for item in binding["files"]):
        raise ValueError(f"License supplement contains no license text: {package_key(package)}")
    for item in binding["archive_notices"]:
        source = read(item["path"])[0]
        start, length = item["offset"], item["length"]
        raw = source[start:start + length]
        if start < 0 or length <= 0 or len(raw) != length or sha256(raw) != item["sha256"]:
            raise ValueError(f"License supplement notice range differs: {package_key(package)}")
        texts.append((item["path"], item["path"], "archive-license-notice", raw))
    return texts, binding


def collect(args, output):
    output = new_directory(output)
    entries = {}
    roots = []
    sources = Sources()
    supplements, supplement_sha256 = load_supplements()
    binaries = {}
    tracked = command("git", "-C", ROOT, "ls-files", "-z").decode().split("\0")
    manifest_inputs = {
        name: (ROOT / name).read_bytes()
        for name in tracked if PurePosixPath(name).name == "Cargo.toml"
    }
    local_manifests = {}
    lock_inputs = {}
    notices = io.BytesIO()
    notices.write(
        b"Magpie Rust third-party notices\n\n"
        b"Original Cargo license expressions and source notice texts follow.\n"
        b"Both sides of dual-license expressions are retained when present.\n"
        b"This is a conservative Cargo dependency inventory, not a linker map\n"
        b"or a determination that all distribution obligations are satisfied.\n\n"
    )
    choices = [
        ("cli", "magpie-cli", ROOT / "Cargo.toml", args.cli or args.binary, args.binary),
        ("desktop", "magpie-desktop", ROOT / "apps/desktop/Cargo.toml",
         args.desktop or args.desktop_binary, args.desktop_binary),
    ]
    for label, package_name, manifest, enabled, binary in choices:
        if not enabled:
            continue
        lock_path = manifest.with_name("Cargo.lock")
        manifest_name = manifest.relative_to(ROOT).as_posix()
        local_manifests[manifest_name] = sha256(manifest_inputs[manifest_name])
        lock_bytes = lock_path.read_bytes()
        lock_inputs[lock_path] = lock_bytes
        lock = tomllib.loads(lock_bytes.decode("utf-8"))
        checksums = {
            (p["name"], p["version"], p.get("source")): p.get("checksum")
            for p in lock["package"]
        }
        with temporary_directory("magpie-license-metadata-") as scratch:
            metadata = json.loads(command(
                "cargo", "metadata", "--format-version", "1", "--locked", "--offline",
                "--filter-platform", args.target, "--manifest-path", manifest,
                "--config", f"build.target-dir={json.dumps(str(scratch))}",
                env={**os.environ, "CARGO_TARGET_DIR": str(scratch),
                     "CARGO_BUILD_BUILD_DIR": str(scratch / "intermediates")},
            ))
        if lock_path.read_bytes() != lock_bytes:
            raise ValueError(f"Lockfile changed during collection: {label}")
        packages, nodes, roles = dependency_closure(metadata, package_name)
        roots.append({
            "name": label, "package": package_name,
            "manifest": manifest_name, "manifest_sha256": local_manifests[manifest_name],
            "lockfile": lock_path.relative_to(ROOT).as_posix(),
            "lockfile_sha256": sha256(lock_bytes),
            "package_count": len(roles),
        })
        if binary:
            with binary.open("rb") as stream:
                binaries[label] = hashlib.file_digest(stream, "sha256").hexdigest()
        for identifier, dependency_roles in roles.items():
            package = packages[identifier]
            if package["source"] is None:
                name = Path(package["manifest_path"]).resolve().relative_to(ROOT).as_posix()
                local_manifests[name] = sha256(manifest_inputs[name])
                # Path crates may inherit workspace.package/dependencies.
                for parent in Path(name).parents:
                    workspace_manifest = (parent / "Cargo.toml").as_posix()
                    if workspace_manifest in manifest_inputs:
                        local_manifests[workspace_manifest] = sha256(manifest_inputs[workspace_manifest])
            key = package_key(package)
            if key not in entries:
                checksum = checksums.get((package["name"], package["version"], package["source"]))
                texts, missing = collect_texts(package, checksum, sources)
                in_archive = not missing if package["source"] and package["source"].startswith("registry+") else None
                resolution = None
                if missing:
                    extra, resolution = supplement_texts(package, checksum, sources, supplements)
                    texts.extend(extra)
                    missing = resolution is None
                status = "source-files"
                if resolution:
                    status = resolution["resolution"]
                elif missing:
                    status = "unresolved-known-license" if package["license"] else "unknown-license"
                entries[key] = {
                    "package": key, "name": package["name"], "version": package["version"],
                    "source": package["source"] or "path:" + Path(package["manifest_path"]).resolve().parent.relative_to(ROOT).as_posix(),
                    "license": package["license"], "license_file": package["license_file"],
                    "source_code": source_code(package, checksum),
                    "crate_sha256": checksum, "used_by": {}, "texts": [],
                    "missing_license_file": missing, "_texts": texts,
                    "license_file_in_published_crate": in_archive,
                    "license_text_status": status,
                    "license_resolution": resolution,
                }
            entries[key]["used_by"][label] = {
                "roles": sorted(dependency_roles), "features": sorted(nodes[identifier]["features"]),
            }
    for key, entry in sorted(entries.items()):
        notices.write((
            f"=== {key} ===\nCargo license expression: {entry['license']}\n"
        ).encode())
        source = entry["source_code"]
        if source.get("url"):
            notices.write(f"Source code: {source['url']}\n".encode())
        if source.get("sha256"):
            notices.write(f"Published source archive SHA-256: {source['sha256']}\n".encode())
        if source.get("kind") == "git":
            notices.write(f"Source revision: {source['revision']}\n".encode())
        if entry["missing_license_file"]:
            notices.write(b"LIMITATION: license text has not been resolved from verified sources.\n")
        for name, origin, kind, raw in entry.pop("_texts"):
            notices.write(f"\n--- {name} ({kind}; sha256 {sha256(raw)}) ---\n".encode())
            provenance = None
            if entry["license_resolution"]:
                provenance = next((
                    item for item in entry["license_resolution"]["files"]
                    if item["path"] == name and item["sha256"] == sha256(raw)
                ), None)
                if provenance:
                    notices.write(f"Upstream: {provenance['url']}\n".encode())
            offset = notices.tell()
            notices.write(raw)
            notices.write(b"\n")
            text = {
                "path": name, "resolved_path": origin, "kind": kind, "sha256": sha256(raw),
                "notice_offset": offset, "notice_length": len(raw),
            }
            if provenance:
                text["provenance"] = provenance
            entry["texts"].append(text)
        notices.write(b"\n")
    notice = notices.getvalue()
    for name, expected in local_manifests.items():
        if sha256((ROOT / name).read_bytes()) != expected:
            raise ValueError(f"Manifest changed during collection: {name}")
    for path, expected in lock_inputs.items():
        if path.read_bytes() != expected:
            raise ValueError(f"Lockfile changed during collection: {path.relative_to(ROOT)}")
    inventory = {
        "schema_version": 2, "target": args.target, "roots": roots,
        "local_manifest_sha256": local_manifests,
        "source_commit": command("git", "-C", ROOT, "rev-parse", "HEAD").decode().strip(),
        "source_dirty": bool(command("git", "-C", ROOT, "status", "--porcelain")),
        "collector_sha256": sha256(Path(__file__).read_bytes()),
        "binary_sha256": binaries, "packages": [entries[key] for key in sorted(entries)],
        "missing_license_files": [key for key in sorted(entries) if entries[key]["missing_license_file"]],
        "unresolved_source_code": [
            key for key in sorted(entries)
            if entries[key]["source_code"]["kind"] != "local" and not entries[key]["source_code"].get("url")
        ],
        "resolved_archive_omissions": [
            key for key in sorted(entries) if entries[key]["license_resolution"] is not None
        ],
        "license_supplements": {
            "path": SUPPLEMENTS.relative_to(ROOT).as_posix(), "sha256": supplement_sha256,
        },
        "notices": {"file": NOTICE, "sha256": sha256(notice)},
        "limitations": [
            "Target-filtered Cargo non-dev closure; includes build dependencies and proc macros conservatively.",
            "Cargo metadata is not an exact linker or host/target compilation-unit inventory.",
            "Named nested notices are retained, including bundled native-code notices; their runtime use is not inferred.",
            "System libraries, Rust standard libraries, SDKs and external runtime assets are not enumerated.",
            "Binary hashes identify packaging inputs; correspondence to this source/lockfile build is not verified.",
            "Source URLs identify published crate bytes or locked Git trees; the collector does not fetch them or attest corresponding binary builds.",
            "License expressions are preserved verbatim; no license choice or legal compliance conclusion is made.",
        ],
    }
    (output / NOTICE).write_bytes(notice)
    (output / INVENTORY).write_text(json.dumps(inventory, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return inventory


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target")
    parser.add_argument("--prepare-cache", action="store_true",
                        help="Explicit online cargo fetch --locked for all platforms; no build")
    parser.add_argument("--reserve-build-dir", type=Path,
                        help="Reserve a new external build directory and print its absolute path")
    parser.add_argument("--allocate-build-dir", action="store_true",
                        help="Allocate and print a new external build directory for an AUR build")
    parser.add_argument("--check-output", type=Path,
                        help="Validate an unused external path without creating it")
    parser.add_argument("--cli", action="store_true")
    parser.add_argument("--desktop", action="store_true")
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--desktop-binary", type=Path)
    parser.add_argument("--out-dir", type=Path)
    parser.add_argument("--allow-incomplete-notices", action="store_true", help="Local audit only; retain missing-file warnings")
    args = parser.parse_args()
    if args.allocate_build_dir:
        base = external_path(Path(tempfile.gettempdir()))
        print(tempfile.mkdtemp(prefix="magpie-rs-aur-", dir=base))
        return
    if args.reserve_build_dir:
        print(new_directory(args.reserve_build_dir))
        return
    if args.check_output:
        path = external_path(args.check_output)
        if path.exists() or args.check_output.is_symlink():
            parser.error("Output path already exists; use a new exclusive directory")
        return
    if args.prepare_cache:
        if not (args.cli or args.desktop) or args.binary or args.desktop_binary or args.out_dir:
            parser.error("--prepare-cache requires --cli and/or --desktop, without binary/output options")
        prepare_cache(args.cli, args.desktop)
        return
    if not any((args.cli, args.desktop, args.binary, args.desktop_binary)):
        parser.error("Select --cli, --desktop, --binary or --desktop-binary")
    if not args.target or not args.out_dir:
        parser.error("--target and --out-dir are required for collection")
    output = external_path(args.out_dir)
    inventory = collect(args, output)
    missing = inventory["missing_license_files"]
    if missing or inventory["unresolved_source_code"]:
        message = (f"{len(missing)} packages lack verified license texts and "
                   f"{len(inventory['unresolved_source_code'])} lack official source URLs; see {output / INVENTORY}")
        if not args.allow_incomplete_notices:
            raise ValueError(message + "; resolve before release (--allow-incomplete-notices is for local audits)")
        print("WARNING: " + message, file=sys.stderr)
    print(f"Collected {len(inventory['packages'])} packages for {args.target}")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError, tarfile.TarError) as error:
        sys.exit(f"License collection failed: {error}")
