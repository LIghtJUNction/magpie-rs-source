# Magpie Rust 0.2.0 early preview

This is an incomplete Rust reconstruction of Magpie. Package installation does
not imply that all original Magpie features or provider integrations work.
Product releases are hosted by `LIghtJUNction/magpie-rs`; implementation and
build workflows live in `LIghtJUNction/magpie-rs-source`.

The base `magpie-rs-VERSION-TARGET.tar.gz` / `.zip` archives contain **CLI only**.
Archives with `magpie-rs-gui-` include the GPUI desktop. Check the included
`BUILD-METADATA.json` for the exact capabilities, source commit and binary hashes.
Linux aarch64, Windows ARM64 and Android archives are CLI only. No Android APK
or native Android GUI is supplied. macOS and Windows desktop builds are
attempted separately and are available only when those jobs succeed.

Verify the adjacent `.sha256` file before installing:

```sh
sha256sum -c magpie-rs-0.2.0-x86_64-unknown-linux-gnu.tar.gz.sha256
```

On macOS use `shasum -a 256 -c FILE.sha256`; on Windows use
`Get-FileHash FILE -Algorithm SHA256` and compare to the adjacent checksum.

## Arch Linux

The default AUR `magpie-rs-bin` and `magpie-rs-git` packages include **both**
`/usr/bin/magpie` and `/usr/bin/magpie-desktop`, the **Magpie Rust** desktop
entry and icon. They currently target Linux x86_64 only. They deliberately
conflict with `magpie`, `magpie-git`, `magpie-bin` and each other. The AUR
`magpie` IPv6 proxy is unrelated; this mutual exclusion is intentional.
Review the replacement of an existing `magpie-git` before accepting pacman's
transaction. User data is not migrated or removed by these packages.

The release `.pkg.tar.zst` named `magpie-rs-cli-...` is a **CLI only**
alternative:

```sh
sudo pacman -U magpie-rs-cli-0.2.0-1-x86_64.pkg.tar.zst
sudo pacman -R magpie-rs-cli
```

Use the AUR desktop package instead when the GUI is required. Arch aarch64
packages are intended for compatible Arch Linux ARM systems, not official
Arch Linux x86_64. Both CLI and desktop packages depend on `dbus` for libdbus;
on Arch this package also supplies the headers needed by the source build.

## Debian / Ubuntu

```sh
sudo apt install ./magpie-rs-cli_0.2.0-1_amd64.deb
magpie --help
sudo apt remove magpie-rs-cli
```

These native Linux builds use Ubuntu 22.04; the `.deb` contains calculated
runtime dependencies. GNU/Linux archives require compatible glibc and
libgcc plus `libdbus-1-3`; they are not musl/Alpine binaries. Debian/Ubuntu
source builds additionally need `libdbus-1-dev` and `pkg-config`, including
when building only the CLI. The GUI additionally requires a working display
session and GPUI graphics dependencies.

## Linux portable installation

Extract the CLI archive and install `bin/magpie` into `~/.local/bin` (add this
directory to PATH). For the separate Linux GUI archive, also install
`bin/magpie-desktop` into the same directory, copy
`share/applications/magpie-rs.desktop` into `~/.local/share/applications/`,
and copy `share/icons/hicolor/scalable/apps/magpie-rs.svg` into
`~/.local/share/icons/hicolor/scalable/apps/`.
Run `magpie gui`, `magpie-desktop`, or the **Magpie Rust** desktop entry.
Remove those exact files to uninstall.

Linux direct API-key storage uses an available, unlocked Secret Service on the
user's D-Bus session. The packages do not select or configure a credential
service, and installing libdbus does not unlock one. Headless users can explicitly
choose
`magpie providers add ... --key-env VARIABLE_NAME` and supply that variable to
the Gateway process. A missing or locked saved key is an error, never an
automatic switch to environment credentials. The libdbus runtime library is
still required to load these GNU/Linux binaries even when using environment
credentials. New musl/static builds require target-native libdbus and separate
validation; earlier static-archive results do not cover the new keyring backend.

## macOS (13 or later)

Open the target architecture's `.pkg`, or run:

```sh
sudo installer -pkg magpie-rs-0.2.0-aarch64-apple-darwin-gui.pkg -target /
```

The GUI package installs `/Applications/Magpie Rust.app`, the CLI at
`/usr/local/bin/magpie`, and `/usr/local/bin/magpie-desktop` pointing to the
app's executable, so `magpie gui` works. The `-cli.pkg` installs the CLI
only. Choose one package variant; their CLI paths overlap. These packages
are unsigned and not notarized, so Gatekeeper may require explicit approval.
No Developer ID credentials are embedded in the workflow.

To uninstall, remove the installed binaries and (for GUI packages) the app,
plus `/usr/local/share/doc/magpie-rs` and
`/usr/local/share/licenses/magpie-rs`, then use `pkgutil --forget` with
`io.github.lightjunction.magpie-rs.gui` or `.cli`. Do not remove those paths
while another variant is still installed.

## Windows

Run the `-gui-setup.exe` installer for desktop plus CLI, or `-cli-setup.exe`
for CLI only. These are actual NSIS installers with an uninstaller registered
in Windows Settings. They install per user under
`%LOCALAPPDATA%\Programs\Magpie Rust`; no administrator rights are needed.
The GUI executable and CLI share one directory so `magpie gui` can find it.
Use the Start menu shortcut, or run the installed executable by its full
path. The NSIS installer does not modify PATH. Files are unsigned; no
Authenticode signing certificate is configured.

The `.zip` is portable. Extract it completely, then run its `install.ps1`
in PowerShell to copy binaries into
`%LOCALAPPDATA%\Programs\Magpie Rust Portable` and add that directory to
the user PATH. Restart the terminal afterward. Uninstall that variant with:

```powershell
& "$env:LOCALAPPDATA\Programs\Magpie Rust Portable\install.ps1" -Uninstall
```

Windows ARM64 currently has a CLI ZIP and portable PowerShell installer
only. It is cross compiled on the x64 runner and is not executed there.

## Android / Termux

The parent task builds the `aarch64-linux-android` CLI separately with its
Android NDK and tests it on an arm64 phone. No Android asset is produced by
the native desktop workflow. An Android tarball is a CLI binary, not an APK.
Extract and install it into a Termux executable path only after its device
compatibility has been verified. Desktop GNU/Linux binaries do not run on
Android. The Android target does not link the Linux Secret Service backend.
Use an explicit `--key-env VARIABLE_NAME` Provider credential reference;
`--key-stdin`/system key storage is currently unavailable, with no plaintext
fallback. This packaging update does not establish a new device execution result.
