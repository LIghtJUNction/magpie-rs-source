# Magpie Rust

Rust implementation for the `magpie-rs` Recipkg project. Version 0.2.0 is an
early preview with a standalone CLI, bounded local-provider forwarding,
recoverable Codex configuration changes, and GPUI desktop actions.
The full product contract remains under active implementation. Both previous Pro visual directions were rejected; a further redesign is
required. Local Linux builds, functional tests and native font shaping checks
do not establish visual acceptance. This checkpoint is not a desktop release.

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

The CLI provides agent discovery, configuration inspection, explicit Codex
connection/restore transactions, status output, and loopback-only gateways.
Agent discovery checks installed Codex, Claude Code, Gemini CLI,
OpenCode and Zed executables on PATH. Only Codex configuration is inspected;
its explicit model, provider and reasoning effort are read without credentials.
The GPUI application reads local snapshots and calls the packaged CLI for model
queries, selection, read-only connection previews, explicit saves, ownership
checks and offline restoration. A preview is invalidated when its inputs or
observed configuration change. Discovery, inspection and previews do not change
configuration or sign-ins; only an explicit save or restore writes the selected
Codex configuration. The desktop requires its matching CLI beside the executable,
or an explicit absolute `MAGPIE_CLI_PATH` for development; it does not search PATH
for another Magpie binary.
Provider sign-in, remote HTTPS forwarding, protocol conversion, full Usage,
plugins and synchronization remain unimplemented.

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

### Actual local-provider forwarding and Codex connection

`--forward` accepts the public JSON shape in `examples/forward-local.json`.
It supports one explicitly configured literal-loopback HTTP provider through
OpenAI Chat Completions and Responses, including bounded SSE, cancellation,
four active requests and immediate overload rejection. Credentials, when needed,
come from a named environment variable; the file contains only that reference.
It does not implement remote providers or the full Gateway contract.
The example expects an already running provider at port 18081 and the named
credential environment variable. Replace its address/models/reference for your
provider, or omit `api_key_env` for an unauthenticated local provider.

```sh
magpie serve --listen 127.0.0.1:3425 --forward examples/forward-local.json
magpie gateway inspect --listen 127.0.0.1:3425 --json
magpie agents connect codex --config /absolute/test-home/config.toml --model local/fixture-a --gateway 127.0.0.1:3425 --dry-run --json
magpie agents connect codex --config /absolute/test-home/config.toml --model local/fixture-a --gateway 127.0.0.1:3425 --apply --json
magpie agents connection-status codex --config /absolute/test-home/config.toml --json
magpie agents disconnect codex --config /absolute/test-home/config.toml --json
```

Run the remaining commands in another terminal while `serve` runs. Supply an
existing private parent directory and a model actually configured in the running
Gateway. Preview writes nothing. Apply verifies the listener/catalog before
committing seven owned fields. Unchanged configurations restore byte for byte;
external edits retain their own values during selective restore. Private recovery
images and a persistent journal are bounded to 128 MiB and 256 entries.
An interrupted operation can require manual recovery of its cooperative lock;
the final precondition-check/rename interval is not a filesystem compare-and-swap.
Mutation currently supports Linux, macOS and Android; Windows returns an explicit
unsupported-platform result. Configuration ownership does not prove Provider
health or a completed Agent request.

Local verification on the current candidate includes 77 workspace tests, 26
real TCP/startup/CLI cases, and an installed Codex 0.165.0 executing against an
isolated synthetic provider. The native-client case receives a Chinese Responses
stream through Magpie and restores the original configuration bytes. It does
not use a real account or external model. Reproduce with:

```sh
python3 -B tests/first_loop.py --binary /path/to/release/magpie
python3 -B tests/native_codex.py --binary /path/to/release/magpie --codex /path/to/codex
```

The window-independent desktop action worker also passed with Codex 0.165.0 in
two isolated homes: an existing configuration and an originally missing file.
Both cases execute the real desktop state/CLI bridge, query and select a model,
preview without writes, save, run Codex through the synthetic Responses stream,
stop the gateway, and restore the exact original bytes or original absence.
Build the desktop library test executable with `cargo test --release --locked
--manifest-path apps/desktop/Cargo.toml --lib --no-run --message-format=json`, then
use its reported executable path:

```sh
python3 -B tests/desktop_actions.py --self-check
python3 -B tests/desktop_actions.py --binary /path/to/release/magpie --test-binary /path/to/magpie_desktop-libtest --codex /path/to/codex --report /path/to/desktop-actions.json
```

Recorded evidence and exact binary/source/fixture hashes are in `tests/results/`.
The two ignored test-worker entry points are executed explicitly by their parent
cases; the desktop adds one ignored worker explicitly run twice by its Python
coordinator. These are not separate completed product requirements. Actual native
window interactions, IME, screen readers, other Agent clients, external TLS and
the full 93 requirements remain open.

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
