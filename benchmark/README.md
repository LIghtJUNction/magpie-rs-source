# Performance benchmarks

These runners measure the implemented CLI and local gateway model catalog.
They currently require Linux: CLI fork/exec peak RSS uses `wait4`, while gateway RSS/PSS uses
`/proc/<pid>/smaps_rollup`. Results are host-specific and are not a substitute
for provider forwarding, inference, streaming, concurrency, or cold-cache
measurements. CLI `wait4` RSS includes the Python child before `exec`; treat it as an upper bound rather than target-only CLI memory.

## Reproduce

Run from the repository root. Build artifacts go under `/tmp` so the checkout
stays free of a `target/` directory:

```sh
CARGO_TARGET_DIR=/tmp/magpie-rs-bench-target cargo build --release --locked -p magpie-cli
python3 benchmark/cli_startup.py \
  --binary /tmp/magpie-rs-bench-target/release/magpie \
  --output /tmp/magpie-cli-startup.json
python3 benchmark/gateway_catalog.py \
  --binary /tmp/magpie-rs-bench-target/release/magpie \
  --output /tmp/magpie-gateway-catalog.json
python3 benchmark/gateway_catalog.py \
  --binary /tmp/magpie-rs-bench-target/release/magpie \
  --catalog benchmark/fixtures/catalog.toml \
  --output /tmp/magpie-gateway-catalog-configured.json
```

The CLI runner performs five warmups and 30 measured process launches for each
case. It checks output and exit status, isolates configuration paths, and uses
only a synthetic Codex executable plus a credential-free 4 KiB TOML fixture.
The gateway runner performs three warmup starts, 30 measured starts, 100
loopback catalog requests, and a 60-second idle memory sample. Its catalog is
empty by default; the configured fixture exercises three providers and sixty
models and validates every returned model ID. Raw samples, binary/source/runner hashes, toolchain and host
details are stored in the JSON output. p95 uses nearest-rank selection.
New results identify their source hash scope: workspace Cargo manifests/lock
and crates Rust/TOML sources. The independently built GPUI application is
excluded. Historical results without `source_scope` used the wider repository
Rust/TOML tree and should not be compared by source hash alone.

The checked-in `results/` files are dated baselines. The earliest CLI result is
the pre-gateway baseline; later CLI and gateway results measure the current
implementation. Do not compare runs as an optimization win unless the host,
binary, workload, and measurement method are held constant.
