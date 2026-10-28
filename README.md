# Magpie Rust

This repository is the Rust implementation used by the `magpie-rs` Recipkg
project. It is an early, deliberately small reconstruction; it is not yet a
feature-complete replacement for Magpie.

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
connection previews, status output, and a loopback-only gateway process. The
gateway currently reports an empty model catalog. Provider sign-in, model
forwarding, streaming, configuration writes, and most of the original Magpie
surface are not implemented yet. Consult the parent project's recipe for the
full requirements and acceptance status.

## Performance measurements

Reproducible Linux benchmark runners and their raw JSON results are in
[`benchmark/`](benchmark/). The 2026-10-28 x86_64 run measured the release CLI
at 577,896 bytes, warm CLI process-start p50 at 1.37–1.85 ms, empty gateway
catalog request p50 at 0.090 ms, and idle gateway RSS/PSS medians at 2,976/779 KiB. These are
host-specific baselines for the implemented paths, not cross-platform or
upstream-inference measurements. Method and limitations are documented in
[`benchmark/README.md`](benchmark/README.md).
