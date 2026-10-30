#!/usr/bin/env python3
"""Package existing release binaries. All scratch files live outside the checkout."""

import argparse
import contextlib
import gzip
import hashlib
import io
import importlib
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import struct
import subprocess
import sys
import tarfile
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[1]
licenses = importlib.import_module("collect-licenses")
TARGETS = {
    "x86_64-unknown-linux-gnu": ("linux", "x86_64", "amd64"),
    "aarch64-unknown-linux-gnu": ("linux", "aarch64", "arm64"),
    "x86_64-unknown-linux-musl": ("linux", "x86_64", "amd64"),
    "aarch64-unknown-linux-musl": ("linux", "aarch64", "arm64"),
    "armv7-unknown-linux-musleabihf": ("linux", "armv7", "armhf"),
    "riscv64gc-unknown-linux-musl": ("linux", "riscv64", "riscv64"),
    "aarch64-apple-darwin": ("macos", "aarch64", "arm64"),
    "x86_64-apple-darwin": ("macos", "x86_64", "x86_64"),
    "x86_64-pc-windows-msvc": ("windows", "x86_64", "x64"),
    "aarch64-pc-windows-msvc": ("windows", "aarch64", "arm64"),
    "aarch64-linux-android": ("android", "aarch64", "arm64"),
    "armv7-linux-androideabi": ("android", "armv7", "armv7"),
    "x86_64-linux-android": ("android", "x86_64", "x86_64"),
    "i686-linux-android": ("android", "i686", "x86"),
}
URL = "https://github.com/LIghtJUNction/magpie-rs"
CONFLICTS = ["magpie", "magpie-git", "magpie-bin"]
NOTICE_FILES = ("THIRD-PARTY-NOTICES.txt", "THIRD-PARTY-LICENSES.json")


def run(*args, **kwargs):
    return subprocess.check_output(
        [str(arg) for arg in args], text=True, **kwargs
    ).strip()


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def copy(source, destination, executable=False):
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)
    destination.chmod(0o755 if executable else 0o644)


def write(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    path.chmod(0o644)


def collect_notices(payload, args, extension):
    notice_dir = payload.parent / "notices"
    command = [
        sys.executable, ROOT / "scripts/collect-licenses.py",
        "--target", args.target, "--out-dir", notice_dir,
    ]
    if args.binary:
        command += ["--binary", payload / f"bin/magpie{extension}"]
    if args.desktop_binary:
        command += ["--desktop-binary", payload / f"bin/magpie-desktop{extension}"]
    if args.allow_incomplete_notices:
        command.append("--allow-incomplete-notices")
    subprocess.run([str(arg) for arg in command], check=True)
    for name in NOTICE_FILES:
        copy(notice_dir / name, payload / name)
    return json.loads((payload / NOTICE_FILES[1]).read_text(encoding="utf-8"))


def windows_script(path, replacements):
    text = path.read_text(encoding="utf-8")
    for original, replacement, count in replacements:
        if text.count(original) != count:
            raise ValueError(f"Notice insertion point changed in {path.name}")
        text = text.replace(original, replacement)
    return text


def check_binary(path, platform, arch):
    with path.open("rb") as stream:
        data = stream.read(4096)
    if platform in ("linux", "android"):
        expected = {"x86_64": 62, "aarch64": 183, "armv7": 40, "i686": 3, "riscv64": 243}[arch]
        valid = data[:4] == b"\x7fELF" and int.from_bytes(data[18:20], "little") == expected
    elif platform == "windows":
        offset = int.from_bytes(data[60:64], "little")
        with path.open("rb") as stream:
            stream.seek(offset)
            header = stream.read(6)
        valid = header[:4] == b"PE\0\0" and int.from_bytes(header[4:6], "little") == (
            0x8664 if arch == "x86_64" else 0xAA64
        )
    else:
        expected = 0x01000007 if arch == "x86_64" else 0x0100000C
        valid = data[:4] == b"\xcf\xfa\xed\xfe" and struct.unpack("<I", data[4:8])[0] == expected
    if not valid:
        raise ValueError(f"{path} is not a {platform}/{arch} release binary")


def tar_bytes(stage, prefix="", epoch=0):
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w", format=tarfile.PAX_FORMAT) as archive:
        for path in sorted(stage.rglob("*")):
            name = str(Path(prefix) / path.relative_to(stage)).replace("\\", "/")
            info = archive.gettarinfo(str(path), arcname=name)
            info.uid = info.gid = 0
            info.uname = info.gname = "root"
            info.mtime = epoch
            if path.is_file() and not path.is_symlink():
                with path.open("rb") as stream:
                    archive.addfile(info, stream)
            else:
                archive.addfile(info)
    return buffer.getvalue()


def archive_payload(stage, output, name, platform, epoch):
    if platform == "windows":
        path = output / f"{name}.zip"
        with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
            for item in sorted(stage.rglob("*")):
                if item.is_file():
                    info = zipfile.ZipInfo(f"{name}/{item.relative_to(stage).as_posix()}")
                    info.compress_type = zipfile.ZIP_DEFLATED
                    info.external_attr = (item.stat().st_mode & 0xFFFF) << 16
                    archive.writestr(info, item.read_bytes())
    else:
        path = output / f"{name}.tar.gz"
        with path.open("wb") as stream, gzip.GzipFile(
            filename="", mode="wb", fileobj=stream, mtime=epoch
        ) as compressed:
            compressed.write(tar_bytes(stage, name, epoch))
    return path


def stage_system(payload, system, platform, gui, version):
    for binary in (payload / "bin").glob("*"):
        destination = system / ("usr/local/bin" if platform == "macos" else "usr/bin") / binary.name
        if platform == "macos" and binary.name == "magpie-desktop":
            app = system / "Applications/Magpie Rust.app/Contents"
            copy(binary, app / "MacOS/magpie-desktop", True)
            plist = {
                "CFBundleIdentifier": "io.github.lightjunction.magpie-rs",
                "CFBundleName": "Magpie Rust",
                "CFBundleDisplayName": "Magpie Rust",
                "CFBundleExecutable": "magpie-desktop",
                "CFBundlePackageType": "APPL",
                "CFBundleShortVersionString": version,
                "CFBundleVersion": version,
                "LSMinimumSystemVersion": "13.0",
                "NSHighResolutionCapable": True,
            }
            (app / "Info.plist").write_bytes(plistlib.dumps(plist, sort_keys=True))
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.symlink_to("/Applications/Magpie Rust.app/Contents/MacOS/magpie-desktop")
        else:
            copy(binary, destination, True)
    if platform == "linux" and gui:
        shutil.copytree(payload / "share", system / "usr/share", dirs_exist_ok=True)
    docs = system / ("usr/local/share/doc/magpie-rs" if platform == "macos" else "usr/share/doc/magpie-rs")
    for name in ("INSTALL.md", "BUILD-METADATA.json"):
        copy(payload / name, docs / name)
    licenses = system / (
        "usr/local/share/licenses/magpie-rs" if platform == "macos"
        else "usr/share/licenses/magpie-rs"
    )
    for name in ("LICENSE", *NOTICE_FILES):
        copy(payload / name, licenses / name)
        if platform == "macos" and gui:
            copy(payload / name, system / "Applications/Magpie Rust.app/Contents/Resources" / name)


def linux_packages(payload, scratch, output, args, arch, debarch, epoch, gui):
    system = scratch / "system"
    stage_system(payload, system, "linux", gui, args.version)
    package = "magpie-rs" if gui else "magpie-rs-cli"
    variant = "CLI and GPUI desktop" if gui else "CLI only"
    conflicts = CONFLICTS + ["magpie-rs-bin", "magpie-rs-git", "magpie-rs" if not gui else "magpie-rs-cli"]
    env = {**os.environ, "SOURCE_DATE_EPOCH": str(epoch)}
    # Let native Debian tooling calculate ELF dependencies, including libdbus
    # for Linux system credentials and the required symbol versions.
    debian = scratch / "debian"
    write(debian / "control", f"Source: {package}\n\nPackage: {package}\nArchitecture: any\nDescription: Magpie Rust preview\n")
    executables = list((system / "usr/bin").glob("*"))
    dependencies = run(
        "dpkg-shlibdeps", "--ignore-missing-info", "-O",
        *[f"-e{binary}" for binary in executables], cwd=scratch
    ).removeprefix("shlibs:Depends=")
    size = sum(path.stat().st_size for path in system.rglob("*") if path.is_file())
    control = (
        f"Package: {package}\nVersion: {args.version}-1\nArchitecture: {debarch}\n"
        "Maintainer: Magpie Rust contributors <noreply@github.com>\n"
        f"Installed-Size: {(size + 1023) // 1024}\nSection: utils\nPriority: optional\n"
        f"Homepage: {URL}\nProvides: magpie, magpie-rs\n"
        f"Conflicts: {', '.join(conflicts)}\n"
        f"Description: Magpie Rust early preview ({variant})\n"
        " Incomplete reconstruction; see installed capability metadata.\n"
    )
    if dependencies:
        control += f"Depends: {dependencies}\n"
    write(system / "DEBIAN/control", control)
    deb = output / f"{package}_{args.version}-1_{debarch}.deb"
    run("dpkg-deb", "--root-owner-group", "--build", system, deb, env=env)
    shutil.rmtree(system / "DEBIAN")
    arch_dependencies = ["glibc", "gcc-libs", "dbus"]
    if gui:
        arch_dependencies += [
            "fontconfig", "freetype2", "libxcb", "libxkbcommon", "libxkbcommon-x11", "libx11",
            "libxi", "libxrandr", "wayland", "vulkan-icd-loader", "libxcursor",
            "openssl", "zlib",
        ]
    info = (
        f"pkgname = {package}\npkgbase = {package}\npkgver = {args.version}-1\n"
        f"pkgdesc = Magpie Rust early preview ({variant})\nurl = {URL}\n"
        f"builddate = {epoch}\npackager = Magpie Rust contributors\n"
        f"size = {size}\narch = {arch}\nlicense = MIT\nxdata = pkgtype=pkg\n"
        "provides = magpie\nprovides = magpie-rs\n"
    )
    info += "".join(f"depend = {item}\n" for item in arch_dependencies)
    info += "".join(f"conflict = {item}\n" for item in conflicts)
    write(system / ".PKGINFO", info)
    for path in system.rglob("*"):
        os.utime(path, (epoch, epoch), follow_symlinks=False)
    os.utime(system, (epoch, epoch))
    mtree = subprocess.check_output([
        "bsdtar", "-c", "--format=mtree", "--uid=0", "--gid=0",
        "--options=!all,use-set,type,uid,gid,mode,time,size,sha256,link",
        "-f", "-", "-C", str(system), ".",
    ])
    (system / ".MTREE").write_bytes(gzip.compress(mtree, mtime=epoch))
    pkg = output / f"{package}-{args.version}-1-{arch}.pkg.tar.zst"
    subprocess.run(
        ["zstd", "-q", "-19", "-T1", "-f", "-o", str(pkg)],
        input=tar_bytes(system, epoch=epoch), check=True,
    )
    return [deb, pkg]


def native_packages(payload, scratch, output, args, platform, arch, native_arch, epoch, gui):
    suffix = "gui" if gui else "cli"
    stem = f"magpie-rs-{args.version}-{args.target}-{suffix}"
    if platform == "linux":
        return linux_packages(payload, scratch, output, args, arch, native_arch, epoch, gui)
    if platform == "macos":
        system = scratch / "system"
        stage_system(payload, system, platform, gui, args.version)
        package = output / f"{stem}.pkg"
        run(
            "pkgbuild", "--root", system, "--identifier",
            f"io.github.lightjunction.magpie-rs.{suffix}",
            "--version", args.version, "--install-location", "/", package,
        )
        return [package]
    if platform == "windows" and arch == "x86_64":
        installer = output / f"{stem}-setup.exe"
        flag = "/" if os.name == "nt" else "-"
        defines = [
            f"{flag}DVERSION={args.version}", f"{flag}DPAYLOAD={payload}",
            f"{flag}DOUTPUT={installer}",
        ]
        if gui:
            defines.append(f"{flag}DHAS_DESKTOP")
        # Keep notice installation/removal in the generated installer without
        # maintaining another copy of the Windows template.
        script = scratch / "installer.nsi"
        write(script, windows_script(ROOT / "packaging/windows/installer.nsi", [
            ('  File "${PAYLOAD}\\LICENSE"',
             '  File "${PAYLOAD}\\LICENSE"\n' + "\n".join(
                 f'  File "${{PAYLOAD}}\\{name}"' for name in NOTICE_FILES
             ), 1),
            ('  Delete "$INSTDIR\\LICENSE"',
             '  Delete "$INSTDIR\\LICENSE"\n' + "\n".join(
                 f'  Delete "$INSTDIR\\{name}"' for name in NOTICE_FILES
             ), 1),
        ]))
        run("makensis", *defines, script)
        return [installer]
    raise ValueError(f"No native installer implemented for {args.target}; omit --native-packages")


def build_binaries(args, stack):
    requested = os.environ.get("MAGPIE_RS_TARGET_DIR")
    if requested:
        target_dir = licenses.new_directory(Path(requested))
        stack.callback(shutil.rmtree, target_dir)
    else:
        target_dir = stack.enter_context(licenses.temporary_directory("magpie-rs-release-"))
    env = {**os.environ, "CARGO_TARGET_DIR": str(target_dir),
           "CARGO_BUILD_BUILD_DIR": str(target_dir / "intermediates"),
           "PYTHONDONTWRITEBYTECODE": "1"}
    # Preparation is explicitly online. The later notice collection remains offline.
    licenses.prepare_cache(True, args.with_desktop, env=env)
    subprocess.run(["cargo", "build", "--release", "--locked", "--package", "magpie-cli",
                    "--target", args.target], cwd=ROOT, env=env, check=True)
    extension = ".exe" if TARGETS[args.target][0] == "windows" else ""
    args.binary = target_dir / args.target / "release" / f"magpie{extension}"
    if args.with_desktop:
        subprocess.run(["cargo", "build", "--release", "--locked", "--manifest-path",
                        str(ROOT / "apps/desktop/Cargo.toml"), "--target", args.target],
                       cwd=ROOT, env=env, check=True)
        args.desktop_binary = target_dir / args.target / "release" / f"magpie-desktop{extension}"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--check-version", action="store_true")
    parser.add_argument("--target", choices=TARGETS)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--desktop-binary", type=Path)
    parser.add_argument("--build", action="store_true", help="Prepare locked dependency cache, then build in a new external directory")
    parser.add_argument("--with-desktop", action="store_true", help="With --build, build both CLI and GPUI")
    parser.add_argument("--gui-only", action="store_true", help="Separate Linux GUI archive, without CLI")
    parser.add_argument("--native-packages", action="store_true")
    parser.add_argument("--allow-incomplete-notices", action="store_true",
                        help="Local audit only; package unresolved license-file warnings")
    parser.add_argument("--out-dir", type=Path)
    args = parser.parse_args()
    if not re.fullmatch(r"0\.\d+\.\d+", args.version):
        parser.error("Use an early preview numeric version, e.g. 0.2.0")
    manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())
    if manifest["workspace"]["package"]["version"] != args.version:
        parser.error("Version differs from workspace.package.version in Cargo.toml")
    if args.check_version:
        print(f"Version OK: {args.version}")
        return
    if not args.target or not args.out_dir or (not args.binary and not args.gui_only and not args.build):
        parser.error("--target, --out-dir and --binary (or --gui-only) are required")
    if (args.build and (args.binary or args.desktop_binary or args.gui_only)
            or args.with_desktop and not args.build):
        parser.error("--build takes source inputs only; --with-desktop requires --build")
    platform, arch, native_arch = TARGETS[args.target]
    gui = args.desktop_binary is not None or args.with_desktop
    if args.gui_only and (not gui or platform != "linux" or args.native_packages):
        parser.error("--gui-only requires a Linux desktop binary and creates an archive only")
    if gui:
        version = tomllib.loads((ROOT / "apps/desktop/Cargo.toml").read_text())["package"]["version"]
        if version != args.version:
            parser.error("Desktop manifest version differs from requested release")
    output = licenses.new_directory(args.out_dir)
    epoch = int(os.environ.get("SOURCE_DATE_EPOCH") or run("git", "-C", ROOT, "show", "-s", "--format=%ct", "HEAD"))
    kind = "gui" if gui else "cli"
    name = f"magpie-rs-{'gui-' if gui else ''}{args.version}-{args.target}"
    with contextlib.ExitStack() as stack:
        if args.build:
            build_binaries(args, stack)
        for binary in filter(None, [args.binary, args.desktop_binary]):
            check_binary(binary, platform, arch)
        temp = stack.enter_context(licenses.temporary_directory("magpie-rs-package-"))
        scratch = Path(temp)
        payload = scratch / "payload"
        extension = ".exe" if platform == "windows" else ""
        if args.binary:
            copy(args.binary, payload / f"bin/magpie{extension}", True)
        if gui:
            copy(args.desktop_binary, payload / f"bin/magpie-desktop{extension}", True)
            if platform == "linux":
                copy(ROOT / "packaging/linux/magpie-rs.desktop", payload / "share/applications/magpie-rs.desktop")
                copy(ROOT / "packaging/linux/magpie-rs.svg", payload / "share/icons/hicolor/scalable/apps/magpie-rs.svg")
        copy(ROOT / "LICENSE", payload / "LICENSE")
        copy(ROOT / "packaging/INSTALL.md", payload / "INSTALL.md")
        notices = collect_notices(payload, args, extension)
        if platform == "windows":
            write(payload / "install.ps1", windows_script(ROOT / "packaging/windows/install.ps1", [
                ("'LICENSE',", "'LICENSE', " + ", ".join(f"'{name}'" for name in NOTICE_FILES) + ",", 2),
            ]))
        metadata = {
            "product": "Magpie Rust", "version": args.version, "prerelease": True,
            "target": args.target, "kind": kind, "cli": bool(args.binary), "gui": gui,
            "source_repository": "LIghtJUNction/magpie-rs-source",
            "release_repository": "LIghtJUNction/magpie-rs",
            "source_commit": run("git", "-C", ROOT, "rev-parse", "HEAD"),
            "source_dirty": bool(run("git", "-C", ROOT, "status", "--porcelain")),
            "source_date_epoch": epoch, "rustc": run("rustc", "-Vv"),
            "binaries": {
                binary.name: digest(binary)
                for binary in sorted((payload / "bin").glob("*"))
            },
            "third_party_notices": {
                "inventory": NOTICE_FILES[1], "inventory_sha256": digest(payload / NOTICE_FILES[1]),
                "text": NOTICE_FILES[0], "text_sha256": digest(payload / NOTICE_FILES[0]),
                "package_count": len(notices["packages"]),
                "missing_license_file_count": len(notices["missing_license_files"]),
                "unresolved_source_code_count": len(notices["unresolved_source_code"]),
                "resolved_archive_omission_count": len(notices["resolved_archive_omissions"]),
                "license_supplement_sha256": notices["license_supplements"]["sha256"],
                "incomplete_notices_allowed": args.allow_incomplete_notices,
                "binary_provenance": "Existing binary bytes are hashed; correspondence to this source/lockfile build is not verified.",
            },
            "limitations": "Early preview; not a complete Magpie implementation. See source README.",
        }
        write(payload / "BUILD-METADATA.json", json.dumps(metadata, indent=2, sort_keys=True) + "\n")
        assets = [archive_payload(payload, output, name, platform, epoch)]
        if args.native_packages:
            assets += native_packages(payload, scratch, output, args, platform, arch, native_arch, epoch, gui)
        for asset in assets:
            write(asset.with_name(asset.name + ".sha256"), f"{digest(asset)}  {asset.name}\n")
            print(asset)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(f"Packaging failed: {error}")
