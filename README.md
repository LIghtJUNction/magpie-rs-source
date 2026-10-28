# Magpie Rust

Rust implementation for the `magpie-rs` Recipkg project. Version 0.2.0 is an
early preview with a standalone CLI, loopback model catalog, and GPUI Agent
status window. The full product contract remains under active implementation. The Pro desktop redesign is integrated and has passed the local Linux build and Clippy checks; user visual acceptance is pending. This source checkpoint is not a desktop release.

## Install

Packaging is prepared but AUR packages and release assets have not been
published yet. After publication, download matching packages and checksums from
[magpie-rs Releases](https://github.com/LIghtJUNction/magpie-rs/releases).
On Arch Linux use `paru -S magpie-rs-bin`, or build from source with
`paru -S magpie-rs-git`. Both packages conflict with the original Magpie
packages and with each other. Desktop availability is listed per release;
Android packages provide the CLI without a GPUI APK.

## Build the CLI

```sh
CARGO_TARGET_DIR=/tmp/magpie-rs-target cargo build --release --locked -p magpie-cli
/tmp/magpie-rs-target/release/magpie --help
```

The workspace contains `magpie-core`, `magpie-service`, and the `magpie` CLI.
The separate GPUI desktop shell is in `apps/desktop` and is excluded from the
default workspace build.

## Current implementation boundary

The CLI provides agent discovery, read-only configuration inspection and
connection previews, status output, a static model catalog, and a loopback-only
gateway. Agent discovery checks installed Codex, Claude Code, Gemini CLI,
OpenCode and Zed executables on PATH. Only Codex configuration is inspected;
its explicit model, provider and reasoning effort are read without credentials.
The GPUI application can refresh the local snapshot. Configuration and sign-in data are never modified by these
operations. Provider sign-in, request forwarding, streaming, configuration
writes, Usage, plugins, and synchronization remain unimplemented.

```sh
magpie agents --json
magpie gui
magpie models --catalog examples/catalog.toml --json
magpie serve --listen 127.0.0.1:3425 --catalog examples/catalog.toml
```

Catalogs contain only public model metadata; unknown fields and credentials
are rejected. Model IDs returned by `/v1/models` include the Provider prefix:

```toml
[[providers]]
id = "local"
name = "Local inference"
models = ["example-model"]
```

Catalog loading is bounded to 1 MiB input and 8 MiB encoded response. The
validated snapshot is shared across Gateway workers and loaded once at startup.
Listing a model does not enable forwarding to an upstream. Consult the parent
recipe for full requirements and acceptance status.

## Performance measurements

Reproducible Linux benchmark runners and their raw JSON results are in
[`benchmark/`](benchmark/). The 0.1.0 baseline from 2026-10-28 on x86_64 measured the release CLI
at 577,896 bytes, warm CLI process-start p50 at 1.37–1.85 ms, empty gateway
catalog request p50 at 0.090 ms, and idle gateway RSS/PSS medians at 2,976/779 KiB. These are
host-specific baselines for the implemented paths, not cross-platform or
upstream-inference measurements. Method and limitations are documented in
[`benchmark/README.md`](benchmark/README.md).

The corrected CLI benchmark (`cli-0.2.0-2026-10-28-corrected-rss-v2.json`)
measured warm process-start p50 at 1.49–1.57 ms and peak-RSS medians at
2.52–2.57 MiB across four synthetic CLI cases. RSS uses a separate GNU time
child, excluding Python parent memory. This corrects the measurement method;
it is not a software memory optimization or a comparison against full Magpie.
