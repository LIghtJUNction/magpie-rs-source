# Performance benchmarks

These runners measure the implemented CLI and local gateway model catalog.
They currently require Linux: corrected CLI peak RSS uses GNU `time`, while
gateway RSS/PSS uses `/proc/<pid>/smaps_rollup`. Results are host-specific and are not a substitute
for provider forwarding, inference, streaming, concurrency, or cold-cache
measurements. Historical schema 1 CLI RSS includes the Python child before
`exec`; treat it as a contaminated upper bound. Schema 2 separates RSS from
latency and removes that Python memory floor.

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

GNU `time` defaults to `/usr/bin/time`. Use `--time-binary /absolute/path/to/time`
if it is elsewhere. An official distribution package can be extracted outside
the checkout without a system install; `--time-provenance /path/to/provenance.json`
can record its download URL and archive hash. The runner also records the tool's
version and executable hash. No production or Python package dependency is added.
An existing output file is refused so reruns cannot overwrite raw history.

The CLI runner performs **two passes**, each with five warmups and 30 measured
launches per case: one direct latency pass, then one RSS pass through GNU `time`.
That is 60 measured launches and 10 warmups per case, not 30 paired latency/RSS
observations. Cases run serially in rotating order within each round to reduce
consistent order bias. Every launch, including warmups and both passes, must
exit successfully, produce no stderr, and match the semantic output checks and
the same stdout hash. A failure aborts the run without publishing a successful
result. Version output and the expected synthetic JSON are saved with the result.
The executable is copied into the temporary fixture directory once; all launches
use those same bytes even if another build replaces the original path.

The fixture contains only a synthetic Codex executable marker and a
credential-free 4 KiB TOML configuration. HOME, CODEX_HOME, MAGPIE_CONFIG_DIR and
PATH point into temporary directories; LC_ALL is C and display/GPU variables are
absent. The marker is discovered, not an actual Codex client being executed.
The discovery case validates the configured model and paths; status is only a
configuration/status command, not gateway readiness.

### Corrected CLI measurement boundaries

Method `cli-startup-v2-gnu-time-separate-rss`, JSON schema 2:

- **Latency (ms):** Python parent immediately before `fork` through stdout/stderr
  EOF and `waitpid` exit. It includes Python fork, scheduling, loader, CLI and
  pipe collection costs, and excludes output validation. GNU `time` is absent
  from these launches. The boundary preserves the old latency definition, but
  host load, case ordering and harness memory still prevent automatic comparison.
- **Peak RSS (bytes):** GNU `time --quiet --format=magpie_cli_peak_rss_kib=%M`
  writes a separate report file; Linux child peak RSS in KiB is multiplied by
  1024. GNU `time` has already replaced the forked Python image before it creates
  the measured child. Python's RSS and the monitoring `time` process are excluded.
  The reported lifetime still includes the small C child's pre-exec high-water
  floor, the executable loader and shared resident mappings. This is a
  CLI-lifetime resident-memory measurement with that explicit floor, not an exact
  allocation/private-memory/PSS or aggregate descendant-tree measurement.
- **Wrapper controls:** `/usr/bin/true` runs directly and through the wrapper in
  alternating pairs (five warmups, 30 pairs by default). Both elapsed-time
  series, paired deltas and the wrapped empty-command RSS series are saved.
  This measures empirical wrapper/loader overhead and noise; it is not a value
  to subtract from CLI RSS or latency. A tiny process may report zero under Linux
  RSS accounting; control zeros are retained and never interpreted as zero
  physical memory. A zero CLI RSS sample aborts instead of becoming a low-memory
  claim. If CLI and empty-control RSS overlap, this method cannot separate the
  CLI footprint from the control floor.
- **Runner correctness:** before the measured passes, the runner rejects
  malformed RSS reports, checks KiB conversion, drains 128 KiB on each output
  pipe, preserves exit 7 and signal 15 exits directly and through the wrapper,
  and rejects failed/incorrect command output. It touches 64 MiB in the Python
  parent and repeats five empty controls before/after that allocation to detect
  inherited Python RSS. A separate target touches 16 MiB and must report at
  least that much peak RSS. These controls and parent RSS readings are saved;
  they are excluded from CLI statistics.

RSS is a kernel high-water measurement rather than polling, so there is no
sampling interval that can miss a brief peak. It still measures resident pages,
not total allocated memory, swap usage or memory under a constrained device.
The runner saves host load, available RAM, swap, CPU/memory/I/O pressure and
process names/RSS before and after the passes. Process CPU percentages are
lifetime averages, not an instantaneous profile. The OS page cache is not
dropped; these are warmed synthetic command paths. The four cases do not cover
help, 0/10/100 Agent scales, real accounts, GUI, forwarding or long-running work.
No original Magpie binary is compared and no startup limiter has been profiled.
These measurements cannot establish a speedup, memory savings or overall
superiority over the original application.

The gateway runner performs three warmup starts, 30 measured starts, 100
loopback catalog requests, and a 60-second idle memory sample. Its catalog is
empty by default; the configured fixture exercises three providers and sixty
models and validates every returned model ID. Raw samples, binary/source/runner hashes, toolchain and host
details are stored in the JSON output. p95 uses nearest-rank selection.
New results identify their source hash scope: workspace Cargo manifests/lock
and crates Rust/TOML sources. The independently built GPUI application is
excluded. Historical results without `source_scope` used the wider repository
Rust/TOML tree and should not be compared by source hash alone.
The CLI source hash and Git HEAD describe the checkout when the runner started.
For a supplied prebuilt binary they are context, not proof that this exact
checkout built it. The executable hash identifies the bytes actually measured;
recorded local toolchain versions and the declared release profile do not attest
to its build provenance.

The checked-in `results/` files are dated baselines. The earliest CLI result is
the pre-gateway baseline; later CLI and gateway results measure the current
implementation at their recorded dates. In particular,
`cli-baseline-2026-10-28.json`, `cli-2026-10-28.json` and
`cli-0.2.0-2026-10-28.json` retain their original samples and hashes. Their
roughly 14 MiB RSS values include Python's pre-exec image, even where old metadata
described them as CLI-only. New `cli-*-corrected-rss-v2.json` results correct the
measurement; a lower number is not a code optimization.
Do not compare runs as an optimization win unless the host,
binary, workload, and measurement method are held constant.

### Corrected 0.2.0 observation, 2026-10-28

Raw evidence: `results/cli-0.2.0-2026-10-28-corrected-rss-v2.json`, recorded at
12:15:22 UTC (20:15:22 Asia/Taipei). The supplied executable is 647,616 bytes,
prints `magpie 0.2.0`, and has SHA-256
`62a94f575cb0df69c9d2186c51ec2ccb04a58a3d7a8b6e340d0659e3f5115a7b`.
The checkout HEAD captured at start is
`4a967ec2eecb165630a7b246546bd3bd6362fc05`. No rebuild was performed.

| Case | Latency p50 / p95 (ms) | Latency min-max (ms) | Peak RSS median (MiB) | Peak RSS min-max (MiB) |
| --- | ---: | ---: | ---: | ---: |
| version | 1.512 / 1.822 | 1.134-2.037 | 2.564 | 2.215-2.773 |
| Agent discovery, 4 KiB TOML | 1.568 / 1.777 | 1.221-2.061 | 2.533 | 2.332-2.812 |
| status JSON | 1.488 / 1.881 | 1.121-1.893 | 2.523 | 2.301-2.688 |
| config path JSON | 1.516 / 1.828 | 1.092-1.854 | 2.566 | 2.293-2.641 |

Each cell summarizes 30 launches for its pass. All 280 CLI starts (240 measured
plus 40 warmups) passed exit, stderr, semantic output and cross-pass stdout-hash
checks. All runner controls passed. Increasing the Python parent from
26,017,792 to 93,130,752 resident bytes with a touched 64 MiB allocation left
the five empty-command controls at a maximum of 1,826,816 bytes on both sides.
The separate target touching 16 MiB reported 25,219,072 bytes of peak RSS.

GNU time was version 1.10, extracted from an official Arch package into
`/tmp/magpie-cli-gnu-time-6upjadxd/`; it was not installed system-wide.
The 30 paired empty-command controls had a median wrapped-minus-direct elapsed
delta of 0.471 ms (range -0.047 to 0.963 ms). The negative sample demonstrates
scheduler/measurement noise, not negative wrapper cost. Their RSS median was
1.732 MiB (range 1.582-1.742 MiB); this empirical floor was **not subtracted**.
Exact tool/package hashes, acquisition URLs and all control samples are in JSON.

The host was an i9-12900HX, 24 logical CPUs, 16,091,208 KiB RAM. Load averages
were 6.460 / 7.838 / 10.017 at both endpoints; available RAM was about 3.9 GiB,
and about 30.8 GiB of system swap was in use. The initial 10-second memory
pressure averages were 4.43% (`some`) / 4.36% (`full`), and I/O pressure was
8.35% / 7.61%. Desktop applications, the original Magpie and other development
work remained active; `rustc` appears in the final process snapshot. Swap and
pressure describe the host, not the short-lived CLI's own swapped bytes.

These are repeated observations from one roughly one-second collection window
on a loaded desktop, not independent sessions on a quiet or memory-limited
device. No cold-cache, original-application A/B or actual low-memory-device
acceptance was performed. The approximately 14 MiB historical readings remain
invalid as CLI-only RSS; the corrected values establish a method-specific CLI
baseline and do not prove a code optimization or whole-product superiority.
