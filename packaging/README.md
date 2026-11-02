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
22.04 and install `libdbus-1-dev` plus `pkg-config` for the shared core's Linux
Secret Service backend. Both CLI and GUI link libdbus. Linux GUI archives built
on the parent's Arch host require its
glibc 2.44 runtime and GPUI graphics dependencies; they are not compatible
with Ubuntu 22.04. The AUR binary template declares `glibc>=2.44`. An ARM
GUI is not promised. macOS and Windows GUI jobs are build attempts, not
evidence of native graphical interaction acceptance.

Earlier portable static Linux CLI archives targeted x86_64, AArch64,
ARMv7 hard-float, and RISC-V64 GC using musl. Their x86_64 binary was run on the
build host and ARM/RISC-V binaries under QEMU user emulation. Those results
predate the Linux Secret Service dependency and do not validate this revision's
musl builds or native NixOS/hardware compatibility.

The Linux keyring backend now also requires target-compatible libdbus headers,
libraries and `dbus-1.pc` metadata. For a static musl archive, supply a musl-built
static libdbus and its native dependency closure, with the cross C toolchain and
target-specific pkg-config search/sysroot configured. Host glibc development
libraries cannot satisfy this requirement. `RUSTFLAGS='-C linker=rust-lld'`
alone is no longer a complete build recipe. RISC-V additionally needs
`-C target-feature=+crt-static -C link-self-contained=yes` because its musl
target defaults to dynamic linking. Rebuild with the locked workspace, inspect
the actual ELF interpreter and shared-library requirements, and execute the
result before labeling a new archive static. No updated musl build is claimed
by this dependency correction. Package verified musl binaries with
`scripts/package-release.py` without `--native-packages`.

Earlier Android CLI archives cover ARM64, ARMv7, x86_64 and i686 at API23;
ARM versions were run on an API34 device. Android does not enable the Linux
Secret Service dependency; its current Provider credential source is an explicit
`--key-env NAME` reference. Direct key storage is unavailable there and does not
silently fall back to a file or an environment variable.

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
Linux source builds require Debian/Ubuntu `libdbus-1-dev` and `pkg-config`,
or Arch `dbus` and `pkgconf`; this applies to CLI-only builds as well. Arch's
[`dbus` package](https://archlinux.org/packages/core/x86_64/dbus/files/)
contains both the runtime library and development headers/metadata, so its
runtime `depends` entry also supplies the source package's build dependency.
Native Linux system packaging additionally requires Debian/Ubuntu `dpkg-dev`,
`libarchive-tools`, `zstd`; use makepkg for a native Arch desktop package.
The Deb packager uses `dpkg-shlibdeps` to calculate ELF dependencies, including
`libdbus-1-3` and its required version. Arch release packages and both AUR
variants explicitly depend on `dbus`. Neither package selects or starts a
Secret Service implementation: saving a key requires the user's available,
unlocked service; headless use can explicitly select `--key-env NAME`.
macOS packaging requires Apple's `pkgbuild`; Windows requires NSIS
(`makensis` on PATH). All build/output paths must be absolute and outside every
Git checkout, including the parent recipe repository. Existing output directories
are refused, even when empty; use a fresh path for every invocation. Paths are
checked before and after resolving symlinks, and directories are reserved with
an exclusive `mkdir`.

`build-release.sh` delegates to `package-release.py --build`. It reserves the
output before preparation or compilation and uses a separate temporary Cargo
target and intermediate directory for each invocation. An explicit
`MAGPIE_RS_TARGET_DIR` must also be a new absolute external directory. It is
owned by that invocation and removed on completion or failure; existing build
work is never reused or deleted. The dependency source cache is still Cargo's
normal cache; it is distinct from these exclusive build/output directories.

The preparation stage runs `cargo fetch --locked` without `--target`, once per
selected manifest. This can download dependencies for all platforms and can
exceed 100 MB: check disk space before actually running it. `--filter-platform`
only filters metadata's resolved graph, so a prior build for one platform is
insufficient preparation for offline metadata. To prepare without building:

```sh
python3 -B scripts/collect-licenses.py --prepare-cache --cli --desktop
```

This explicit preparation command may access the network. Ordinary collection
and packaging of existing binaries remain offline and do not fetch on failure.
The checked-in workflow is unchanged: until its owner adds this preparation
step, a clean runner may stop at offline notice collection.

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

## Dependency licenses and notices

Every archive includes `THIRD-PARTY-NOTICES.txt` and
`THIRD-PARTY-LICENSES.json` alongside the project's `LICENSE`.
The inventory preserves each Cargo license expression verbatim, including
dual licenses and `WITH` exceptions. It records the target, root manifests,
manifest and lockfile hashes, package sources, enabled features, dependency roles and
hashes of the actual packaged binaries. The notice file retains source text
bytes unchanged; the inventory gives each text's source path, hash and byte
offset. Neither file contains local Cargo cache paths.
Binary hashes identify the bytes being packaged. Reusing an existing binary
does not prove it was built from the current dirty source, manifests or
lockfiles; this limitation is recorded in the package metadata.

During collection, `scripts/collect-licenses.py` runs
`cargo metadata --format-version 1 --locked --offline --filter-platform TARGET`.
It walks from `magpie-cli` for CLI packages and `magpie-desktop` for desktop
packages, using their separate lockfiles. A combined installer includes both
closures. Dev dependencies are excluded; build dependencies and proc macros
are included conservatively and marked as build dependencies. This is a
Cargo dependency inventory, not proof of precisely which objects the linker
retained. Metadata may also combine features differently from individual
host/target compilation units.

Registry notice texts come from cached `.crate` archives whose SHA-256 must
match the appropriate lockfile. Git notice texts come from Git objects at
the resolved lockfile revision, never from another crate with a similar
license. Monorepo packages without a license file may inherit the nearest
ancestor's license within that same source repository. Local Magpie packages
use tracked files within this checkout, including the root MIT text and
desktop asset notice. Named nested license, notice and copyright files are
retained, including bundled native code. README license sections are retained
as supplemental evidence when standalone license files are absent.

`packaging/license-supplements.json` supplies original upstream texts for exact
published archives that omit those files. Its 48 entries bind the package name,
version, registry source, unmodified license expression, archive checksum and
`.cargo_vcs_info.json`. It deduplicates 22 original text bodies by SHA-256 and
records upstream URLs, revisions, manifest evidence and checksummed byte ranges
for 86 original source license/copyright headers. No implementation source
blobs are stored in the supplement or added to the payload.

The original 21 resolutions are:

- 18 packages use license files from their own upstream repository at the
  archive's recorded revision, usually the workspace ancestor directory.
- `seahash@4.1.0` and `simd_helpers@0.1.0` use their upstream's explicit
  license-only corrections immediately after their published revisions.
  The supplement records the parent/child relation and change evidence.
  For simd_helpers, adding only the exact LICENSE blob to the pinned parent
  tree reproduces the correction commit's tree hash.
- `pathfinder_geometry@0.5.1` has an archive-recorded revision
  `a5e98fac00f433fe2ff4b2135383d82491b8bfc5` that could not be retrieved from
  its declared upstream or original author's repository. Its published
  source files contain explicit Apache/MIT grants and original copyright
  notices. The collector retains those exact headers and the Apache
  Software Foundation's full license text at the URL they reference.
  It does not claim to have recovered that commit's root license files
  or borrow another revision's MIT copyright notice.

The additional 27 resolutions cover the current Windows/macOS desktop
closures: `accesskit_windows@0.34.0`, `accesskit_macos@0.26.3`, and 25
Objective-C ecosystem packages. Each package's `Cargo.toml.orig` was compared
byte-for-byte to its own archive-recorded upstream commit. AccessKit and older
objc2 releases use the original root license at that same commit. Newer objc2
commits provide a licensing declaration with official links rather than a
standalone full license. Their original declaration is retained alongside the
applicable common MIT/Apache/Zlib texts from the SPDX License List at fixed
commit `617f1a78824394ada7525017d98c6fb215aad09b`. Original copyright placeholders
in those standard texts are preserved; they do not supply an invented holder
or year. Package expressions remain unchanged and no license option is chosen.
The objc2 declaration's question about Apple SDK derived works remains in the
notices and resolution limitations. This is an upstream caveat, not a resolved
legal conclusion.

Every inventory entry now records corresponding source retrieval information.
crates.io entries give the official versioned
`https://crates.io/api/v1/crates/NAME/VERSION/download` endpoint and the exact
archive SHA-256 from the lockfile. GitHub dependencies give the locked commit's
source tree URL. Local packages identify the current checkout and its dirty
state; they are not described as verified published source. These URLs identify
the source whose notices were inspected, including MPL components such as
`option-ext`; they do not attest which source produced an existing binary.
Unestablished remote source URLs stop normal packaging, and the diagnostic
override records their unresolved list and count.

When an upstream Apache notice contains only the short grant, the supplement
also includes the original Apache Software Foundation common-license text.
All original expressions and notices remain intact; no copyright names,
dates, license choices or permissions are invented. The schema 2 inventory
distinguishes `license_file_in_published_crate: false`, a resolved archive
omission, an unresolved known license and an unknown license. An archive
omission alone does not mean the software has no license.

The collector needs cached registry archives and Git objects for the
requested target. It does not download missing sources, build anything,
choose a side of a dual license or infer legal permission from a package
name. System libraries, Rust's standard libraries, SDKs, external runtime
assets and source comments outside the specifically reviewed supplemental
notice ranges are outside this inventory.
Collecting notice files does not establish that every obligation
of every license expression has been fulfilled.

Unresolved license texts stop normal packaging. The collector still writes an
inventory listing them. A supplement cannot silently match a new version,
changed archive checksum, license expression or VCS identity. Missing entries
need new source evidence. For diagnostic work only, a local audit may
explicitly retain unresolved-text warnings:

```sh
python scripts/collect-licenses.py --target x86_64-unknown-linux-gnu \
  --cli --desktop --allow-incomplete-notices \
  --out-dir /tmp/magpie-license-audit
```

`package-release.py` accepts the same `--allow-incomplete-notices` switch
for inspecting existing binaries locally. Such packages retain the unresolved
file list and an explicit flag in `BUILD-METADATA.json`; they are not final
release candidates. The normal workflow and AUR source package do not use
this override. The reviewed supplement resolves the original 21 omissions
without this switch. New resolutions must preserve the exact primary upstream
text and its relationship to the checksum-bound archive; do not copy a sibling
crate's license without source evidence.

Native Linux packages install both files under
`/usr/share/licenses/magpie-rs/`. macOS system packages use
`/usr/local/share/licenses/magpie-rs/` and desktop applications also carry
copies inside `Magpie Rust.app/Contents/Resources/`. The packager generates
Windows install/uninstall scripts from the existing templates with both
notice files added; the checked-in Windows templates are not standalone
notice-aware installers. Windows execution remains to be checked on Windows.

### Local audits on 2026-10-28 and 2026-10-29

The first pass after the HTTP/TLS workspace additions, with the unchanged
desktop lockfile, found 121 Linux x86_64 CLI packages, 479 desktop
packages and 530 distinct packages in the combined inventory. All CLI
packages had standalone license files. The initial filename-based desktop
check found 21 omissions. Primary upstream review resolved those omissions
through the supplement described above. Current collection always reads the
current manifests and lockfiles; these historical counts are not pinned
inputs or expected counts for future backend revisions.

Using snapshots of the existing Linux CLI and desktop binaries, two runs
produced identical CLI tar.gz, deb and Arch packages and identical desktop
audit tarballs. The audit checked 1,052 recorded text segments against
cached source files, binary and notice hashes, native package metadata,
and notice installation into temporary AUR package directories. The Arch
mtree root timestamp is normalized along with its children. No source
build, host installation or publication was performed for this audit.

Offline metadata collection also succeeded for Windows x86_64 CLI
(123 packages) and Android AArch64 CLI (120 packages). These are metadata
checks only. macOS notice staging was inspected on Linux; macOS package
execution and Windows installer execution were not performed. Existing
binary snapshots were not attested as builds of the current working tree.

The 2026-10-29 follow-up used ordinary packaging with no incomplete-notice
override: all 21 reviewed resolutions applied and zero license texts remained
unresolved. Two runs produced identical CLI tar.gz, deb and Arch packages and
identical desktop tarballs. The audit checked 1,162 text segments, all 45
supplemental primary-source file references, package hashes and metadata,
and successful notice installation into temporary directories for both AUR
templates. The reviewed supplement is about 123 KiB, including provenance.
Default collection still rejects a deliberately removed resolution; altered
archive/VCS identities, source ranges and text hashes are also rejected.
No Cargo build, host installation, publication or commit was performed.

### Packaging and benchmark fixes checked on 2026-10-29

Small synthetic controls use temporary directories and mocked Cargo/native
tools. They cover repository/parent/symlink path rejection, exclusive directory
reservation under concurrency, preserving existing build work, fetching both
manifests without a target filter, separate build directories, complete
collector output with synthetic metadata, and AUR rejection of missing,
altered or diagnostic notices. The generated AUR source functions are exercised
with a fake Cargo executable; they prepare both manifests and reject repeated
preparation/build state even when the shell does not use errexit.

```sh
python3 -B scripts/packaging_self_check.py
python3 -B benchmark/forwarding_self_check.py
bash -n scripts/build-release.sh
```

After those controls, offline collection from the existing cache succeeded on
Linux for these target-filtered inventories:

The checks were repeated after the concurrent dependency/core updates at source
commit `4545f18e4617b4d5124004b16d81deea15f7684e`: 13 packaging controls,
8 forwarding controls, and the shell syntax check passed. The two lockfiles,
all tracked Cargo manifests, HEAD, and the protected historical forwarding
result were unchanged throughout this repeat. Lockfile SHA-256 values were:

- `Cargo.lock`: `bbbcfa2bb7363ccd190bb913fb32b7fb45c2d98e0057d36bf295475c1fa1279c`
- `apps/desktop/Cargo.lock`: `0ca78a7d9000dfb395274b3739a34642bc1fcbf880a9a52ec8ceb672c069035d`

After the concurrent service commit advanced HEAD to
`cc22e7dacc5794d1b20765d5c1c383a8a734a5b1`, all three inventories and all
21 synthetic controls were repeated successfully with the same counts below.
Both lockfiles and the dependency manifests retained the same hashes.
The rendered AUR checks also assert the provided command names and conflicts
with the other variants and the older split CLI package; actual Arch package
installation transactions remain untested.

| Target | CLI closure | Desktop closure | Combined | Verified text segments |
| --- | ---: | ---: | ---: | ---: |
| x86_64-unknown-linux-gnu | 124 | 479 | 530 | 1,035 |
| x86_64-pc-windows-msvc | 123 | 321 | 376 | 686 |
| aarch64-apple-darwin | 122 | 362 | 417 | 823 |

All three recorded zero unresolved license texts and zero unestablished remote
source URLs. Every inventory text range was checked against its notice-file
hash, and registry inputs were checksum-verified by the collector. Only small
upstream license/declaration/manifests and standard license texts were retrieved
for provenance review. No dependency fetch, Rust build, real-binary packaging,
host install or publication was run for these fixes. These inventories do not
prove Windows/macOS installer construction, target-device execution, exact
linker contents, binary/source correspondence or complete legal compliance.
The original AUR templates and workflow remain unchanged; use the renderer's
augmented recipes and arrange the workflow's preparation step separately.

## AUR handoff

Put the final CLI and GUI tarballs in one directory, then render real
checksums and `.SRCINFO` files:

```sh
python scripts/render-aur.py --version 0.2.0 \
  --asset-dir /tmp/magpie-final-assets --out-dir /tmp/magpie-aur-ready
```

Before writing either AUR package directory, the renderer checks both archives'
notice declarations, inventory and text hashes, every recorded text range,
binary hashes, source URLs, and zero unresolved counts. Missing/legacy notices,
tampering, duplicate or linked archive members, and diagnostic override flags
are rejected. An override flag is rejected even when its unresolved count is
zero. Output must be a new absolute directory outside all repositories.

`magpie-rs-bin` downloads both archives from the **main** repository's
v0.2.0 release. Missing GUI assets stop rendering or packaging.
`magpie-rs-git` follows the **source** repository's main branch and builds
both binaries. It computes a VCS version from tags/revision count.
Its only `SKIP` checksum is for the Git VCS source.
The renderer augments the source recipe with an explicit fetch preparation
stage and a new external Cargo build directory. Its external makepkg source
directory holds an exclusively created state directory; reuse or a second
build in the same state is rejected. The stored Cargo target path lets
`build()`, `check()` and `package()` find the same invocation's binaries.
Build state and its recorded target are retained for inspection;
remove only that invocation's paths when no longer needed. The raw checked-in
AUR templates are inputs to the renderer, not the final hardened recipes.

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
This describes the application and recipe license, not all bundled
dependencies. `magpie-rs-bin` installs CLI and GUI notice inventories
separately under `/usr/share/licenses/magpie-rs-bin/{cli,gui}/`, so neither
archive overwrites the other. Missing notice files in older archives cause
packaging to fail. `magpie-rs-git` collects the two locked closures for the
Rust host target after building and installs their combined inventory under
`/usr/share/licenses/magpie-rs-git/`. It uses cached sources and refuses missing
license files. Cross-target AUR builds are not supported by this template.

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
