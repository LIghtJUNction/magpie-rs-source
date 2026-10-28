# Performance benchmarks

These runners measure the implemented CLI and the empty local gateway catalog.
They currently require Linux: CLI RSS uses `wait4`, while gateway RSS/PSS uses
`/proc/<pid>/smaps_rollup`. Results are host-specific and are not a substitute
for provider forwarding, inference, streaming, concurrency, or cold-cache
measurements.

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
```

The CLI runner performs five warmups and 30 measured process launches for each
case. It checks output and exit status, isolates configuration paths, and uses
only a synthetic Codex executable plus a credential-free 4 KiB TOML fixture.
The gateway runner performs three warmup starts, 30 measured starts, 100
loopback catalog requests, and a 60-second idle memory sample. Its catalog is
empty by design. Raw samples, binary/source/runner hashes, toolchain and host
details are stored in the JSON output. p95 uses nearest-rank selection.

The checked-in `results/` files are dated baselines. The earliest CLI result is
the pre-gateway baseline; later CLI and gateway results measure the current
implementation. Do not compare runs as an optimization win unless the host,
binary, workload, and measurement method are held constant.
