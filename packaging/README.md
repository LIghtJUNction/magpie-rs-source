# Release packaging

Version 0.2.0 is an early preview, not a complete Magpie replacement.
See [INSTALL.md](INSTALL.md) for installation and removal.

## Build workflow

The source repository owns `.github/workflows/package-release.yml`. Its only
trigger is `workflow_dispatch`. It resolves `source_ref` once to an immutable
commit and uses that commit for every job. All jobs have `contents: read`.
There is no release creation, tag push or publication step.

After the parent task has committed and pushed the source, dispatch with:

```sh
gh workflow run package-release.yml --repo LIghtJUNction/magpie-rs-source \
  -f source_ref=SOURCE_COMMIT_SHA -f version=0.2.0
```

For the final build, use the source repository's `v0.2.0` tag. The workspace
and desktop manifest versions must both be 0.2.0, and both Cargo lockfiles
must be committed. Every build uses current stable Rust and `--locked`.
Python 3.13 is selected before any script runs. Toolchain details, source
commit, dirty state and binary hashes are recorded in each archive.

The independent jobs build:

| Rust target | CLI artifacts | GUI artifacts |
| --- | --- | --- |
| x86_64-unknown-linux-gnu | tar.gz, .deb, .pkg.tar.zst | Parent builds separate GUI tar.gz locally |
| aarch64-unknown-linux-gnu | tar.gz, .deb, .pkg.tar.zst | None |
| aarch64-apple-darwin | tar.gz, CLI .pkg | Separate GUI tar.gz and .pkg |
| x86_64-apple-darwin | tar.gz, CLI .pkg | Separate GUI tar.gz and .pkg |
| x86_64-pc-windows-msvc | zip, CLI NSIS .exe | Separate GUI zip and NSIS .exe |
| aarch64-pc-windows-msvc | zip + portable install.ps1 | None; cross compiled, not run on x64 |

CLI jobs do not depend on the desktop jobs. Linux CLI runners use Ubuntu
22.04. Linux GUI archives built on the parent's Arch host require its
glibc 2.44 runtime and GPUI graphics dependencies; they are not compatible
with Ubuntu 22.04. The AUR binary template declares `glibc>=2.44`. An ARM
GUI is not promised. macOS and Windows GUI jobs are build attempts, not
evidence of native graphical interaction acceptance.

Portable static Linux CLI archives additionally target x86_64, AArch64,
ARMv7 hard-float, and RISC-V64 GC using musl. These have no glibc interpreter;
they are useful on Linux systems such as NixOS without adding a glibc loader.
The x86_64 binary was run on the build host; ARM and RISC-V binaries were
checked under QEMU user emulation. This does not establish native NixOS or
hardware compatibility. Android CLI archives cover ARM64, ARMv7, x86_64 and
i686 at API23; ARM versions were run on an API34 device.

Build portable CLI targets with `RUSTFLAGS='-C linker=rust-lld'`. RISC-V also
requires `-C target-feature=+crt-static -C link-self-contained=yes` because its
musl target defaults to dynamic linking. Use the same locked workspace and
`cargo build --release --locked -p magpie-cli --target TARGET`, then package
the binary with `scripts/package-release.py` without `--native-packages`.

Download artifacts from the source run; the parent publishes their contents
under `LIghtJUNction/magpie-rs`, as a **pre-release**:

```sh
gh run download RUN_ID --repo LIghtJUNction/magpie-rs-source \
  --dir /tmp/magpie-rs-0.2.0-artifacts
```

Artifact names are `magpie-rs-0.2.0-TARGET-cli` and
`magpie-rs-0.2.0-TARGET-gui`. Every file has an adjacent `.sha256`.
The actual release filenames are:

- CLI: `magpie-rs-0.2.0-TARGET.tar.gz` (Windows uses `.zip`).
- GUI: `magpie-rs-gui-0.2.0-TARGET.tar.gz` (Windows uses `.zip`).
- Linux CLI: `magpie-rs-cli_0.2.0-1_amd64.deb` / `_arm64.deb`,
  `magpie-rs-cli-0.2.0-1-x86_64.pkg.tar.zst` / `-aarch64.pkg.tar.zst`.
- macOS: `magpie-rs-0.2.0-TARGET-cli.pkg` and `-gui.pkg`.
- Windows x64: `magpie-rs-0.2.0-x86_64-pc-windows-msvc-cli-setup.exe`
  and `-gui-setup.exe`.

## Local commands

Requires Python 3.11+, stable Rust, and the target's native build tools.
Native Linux system packaging requires Debian/Ubuntu `dpkg-dev`,
`libarchive-tools`, `zstd`; use makepkg for a native Arch desktop package.
macOS packaging requires Apple's `pkgbuild`; Windows requires NSIS
(`makensis` on PATH). All build/output directories stay outside the source.

```sh
rustup update stable
bash scripts/build-release.sh 0.2.0 x86_64-unknown-linux-gnu \
  /tmp/magpie-rs-cli-dist --native-packages

# On macOS or Windows, build the desktop installer as well:
bash scripts/build-release.sh 0.2.0 TARGET /tmp/magpie-rs-gui-dist \
  --with-desktop --native-packages

# Package the parent's already-built Linux GUI, without rebuilding GPUI:
python scripts/package-release.py --version 0.2.0 \
  --target x86_64-unknown-linux-gnu \
  --desktop-binary /tmp/ACTUAL_TARGET/release/magpie-desktop \
  --gui-only --out-dir /tmp/magpie-rs-gui-dist
```

On Windows use a valid Windows output path instead of `/tmp`. For Android,
the parent supplies the four NDK binaries and packages them with
`--target aarch64-linux-android`, `armv7-linux-androideabi`,
`x86_64-linux-android`, or `i686-linux-android`. Omit `--native-packages`.
Android binaries are not built by this workflow and are not APKs.

The Python packager also accepts `--binary PATH` to reuse an existing
binary, validates its executable architecture, and rejects output inside
the checkout. `SOURCE_DATE_EPOCH` defaults to the source commit timestamp.
Archive order, ownership, modes and timestamps are normalized. Reproducing
the complete executable or signed installer also requires the same Rust
toolchain, native SDK, system libraries and tool versions; mutable stable
and hosted runner images do not provide that guarantee.

## AUR handoff

Put the final CLI and GUI tarballs in one directory, then render real
checksums and `.SRCINFO` files:

```sh
python scripts/render-aur.py --version 0.2.0 \
  --asset-dir /tmp/magpie-final-assets --out-dir /tmp/magpie-aur-ready
```

`magpie-rs-bin` downloads both archives from the **main** repository's
v0.2.0 release. Missing GUI assets stop rendering or packaging.
`magpie-rs-git` follows the **source** repository's main branch and builds
both binaries. It computes a VCS version from tags/revision count.
Its only `SKIP` checksum is for the Git VCS source.

Do not pin `magpie-rs-git` to a fixed release tarball.

Run makepkg from the rendered directory, in a clean Arch build environment,
with the source and GUI changes already available upstream:

```sh
cd /tmp/magpie-aur-ready/magpie-rs-bin
makepkg --verifysource
makepkg
namcap PKGBUILD ./*.pkg.tar.zst
```

The AUR package sources carry a 0BSD license; the application remains MIT.
Regenerate `.SRCINFO` after the actual VCS build updates `pkgver`. Review
both package contents and the desktop before the parent pushes either AUR
repository. These scripts do not push, publish or install packages.

## References checked 2026-10-28

- [AUR submission guidelines](https://wiki.archlinux.org/title/AUR_submission_guidelines)
- [VCS package guidelines](https://wiki.archlinux.org/title/VCS_package_guidelines)
- [PKGBUILD manual](https://man.archlinux.org/man/PKGBUILD.5.en)
- [PKGINFO manual](https://man.archlinux.org/man/PKGINFO.5.en)
- [Debian package metadata](https://www.debian.org/doc/debian-policy/ch-controlfields.html)
- [GitHub hosted runner labels](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
- [NSIS scripting reference](https://nsis.sourceforge.io/Docs/Chapter4.html)

Action release tags and full commit SHAs were resolved via GitHub's
`repos/actions/NAME/releases/latest` and `git/ref/tags/TAG` APIs, followed
by reading each pinned `action.yml`: checkout v7.0.1, setup-python v7.1.0,
upload-artifact v7.0.2. Only these GitHub-owned actions are used.
