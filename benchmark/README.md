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

## Real forwarding baseline (Linux)

`forwarding.py` measures a synthetic OpenAI Chat Completions Provider directly
and through the CLI's `serve --forward` candidate. It uses Python's standard
library and Linux `/proc`; it neither builds Rust nor needs display/GPU startup.
Supply an already built executable and a **new absolute output path outside both
checkouts**:

```sh
python3 -B benchmark/forwarding.py \
  --binary /home/lightjunction/.cache/magpie-rs-build/cli/release/magpie \
  --output /tmp/magpie-forwarding-full.json \
  --duration-seconds 300 \
  --stream-seconds 5 \
  --long-stream-seconds 60
```

The launch duration applies to **each of 12 matrix cells**, so the default run
takes about an hour plus stream draining, two long-stream rounds and recovery
windows. A bounded smoke command exercises the same paths with fewer samples:

```sh
python3 -B benchmark/forwarding.py \
  --binary /home/lightjunction/.cache/magpie-rs-build/cli/release/magpie \
  --output /tmp/magpie-forwarding-smoke.json \
  --duration-seconds 0.3 --stream-seconds 0.3 \
  --long-stream-seconds 1.2 \
  --sample-interval 0.05 --recovery-seconds 0.2
```

Short smoke results are runner correctness evidence, not the five-minute
performance workload. Duration is restricted to 0.2–600 seconds per cell;
ordinary streams to 0.2–40 seconds. The long stream must be at least twice the
ordinary duration and at most 100 seconds, below the candidate's current
120-second request timeout. Stream durations round up to tenths of a second.
These are harness/candidate bounds, not product budgets.

Optional runner controls use only the Python fixture, corrupt/extra/truncated
responses, a deliberately buffering writer and a silent synthetic child:

```sh
python3 -B benchmark/forwarding_self_check.py
```

They check actual direct delivery/cancellation, byte/event/terminal validation,
rejection accounting and capped startup diagnostics without waiting for pipe
EOF. They do not attest any Magpie executable.

The controls also exercise auxiliary-command output limits and a descendant
that keeps a pipe open after its parent exits. Auxiliary commands (including
the supplied executable's `--version`) are read incrementally, with at most
8 KiB of stdout and 1 KiB of stderr retained. Exceeding either cap terminates
the process group and records `command_output_limit`; a five-second deadline
records `command_deadline`. Captured prefixes survive either failure. Output
is bounded while the process runs, rather than sliced after full buffering.
Repository paths and symlinks into any checkout are refused. The result file
is created exclusively, so concurrent runs cannot overwrite a shared result.

The runner copies and hashes the executable once under `/tmp` and starts that
snapshot with separate synthetic HOME, CODEX_HOME, XDG_CONFIG_HOME and
MAGPIE_CONFIG_DIR directories, an empty PATH, no display variables and no
credentials. Both listeners use ephemeral IPv4 loopback ports. Configuration
and fixtures are generated by the runner; no real Agent configuration, account,
credential store or external network is used. Temporary state is removed,
stdout/stderr capture is capped, startup has a five-second readiness deadline,
and target termination escalates to kill. Existing result files are refused;
failed runs retain JSON diagnostics and exit nonzero.

The workload and accounting are:

- The nonstream response is exactly **65,536 body bytes**, valid Chat JSON with
  assistant content, a stop reason and fixed synthetic usage. The stream sends
  complete JSON SSE content events of exactly **16,384 bytes** at scheduled
  100 ms intervals, starting immediately. It ends with another 16,384-byte
  content/stop/usage event and `data: [DONE]`. HTTP headers/framing are excluded
  from payload bytes; event hashes and expected complete-body hashes are saved.
- Every accepted response must match the fixed body or every expected SSE
  event, total length and SHA-256. SSE is read incrementally with a bounded
  event buffer. Missing/extra/corrupted events or a missing terminal fail.
  The first complete event must arrive before the Provider starts writing its
  terminal event and before client completion. Header receipt alone is
  insufficient. Each successful request must have actually reached the
  synthetic Provider and its handler must have completed.
- Both payloads run at offered concurrency **1, 4 and 16**, with direct/gateway
  order alternating between neighboring cells. Each round starts clients at a
  barrier, drains them all and starts the next round no sooner than 50 ms after
  the prior round began. There are no hidden retries or replacement requests
  inside a round. This deliberately bounded offered load is at most 20 rounds
  per second (320 attempts/s at concurrency 16), not an unpaced capacity test.
  A cell has an additional 250,000-attempt bound; hitting it invalidates that
  cell rather than claiming a full-duration result.
- Readiness verifies the candidate's declared **four active requests, zero
  waiting requests, sixteen connections**. A separate probe holds four
  validated gateway streams open and sends twelve more requests; all twelve
  must return the specific `503 gateway_concurrency_limit` without reaching
  the Provider. Direct concurrency 16 can use sixteen upstream handlers while
  the gateway accepts at most four. Their throughput distributions therefore
  describe different admitted work under the same offered concurrency; this
  candidate policy is not a global admission contract for all Magpie modes.
- Counts cover successful, rejected, erroneous and cancelled requests, HTTP
  statuses, Provider starts/completions/disconnects and fixture errors.
  Rejections and malformed HTTP 200 responses never count toward valid
  throughput or successful latency. Cells retain bounded error examples.
  Valid req/s and body bytes/s divide correct completions/bytes by actual wall
  time through the last round's drain; the requested launch window and last
  launch time are separate fields.
- First-event and total latency report p50 (median), nearest-rank p95, min,
  max and count in milliseconds, for valid responses only. Total starts
  before TCP setup and ends after body EOF and validation, before waiting for
  the Provider handler to release. First-event time includes validating one
  complete event. Connections are closed per request; there is no inference,
  TLS or warm connection-pool comparison. Provider scheduling lateness and
  Python process CPU time are saved to expose fixture/load-generator effects.

Cancellation is tested once directly and with four streams through the
gateway. After the first event and the admission probe, the clients close
without a terminal event. The Provider must observe actual peer EOF/reset or a
socket write failure before natural completion; a harness timeout/shutdown
does not count. Each cancellation records close-to-Provider-disconnect
milliseconds and its socket error category. Gateway transport counters and
post-cancel idle resources are also saved. Those counters describe transport,
not billable Usage or proof of client delivery.

The target is sampled every 100 ms by default (`--sample-interval` changes it)
for RSS, PSS, Swap, SwapPss, threads, file descriptors and CPU time. Raw
timestamps, actual sampling-gap distribution and the Python harness's own
resources are included. Every target task's child list is sampled; observed
children invalidate the baseline because target-only memory would omit their
cost. This can still miss short-lived or detached descendants. These are
**sampled peaks**, not a kernel lifetime high-water measurement; brief peaks
between samples can be missed and sampling itself adds load.

Initial idle and every post-cell/post-cancel recovery window default to one
second (`--recovery-seconds`). Reports retain memory/fd/thread medians and
maxima, recovery deltas relative to the initial idle, plus a single subsequent
health/counter snapshot. They do not invent a calibrated recovery threshold.
Two additional single rounds keep four streams open for the configured longer
duration, directly and through the gateway. Four consecutive memory windows,
Provider emitted-byte progress, full raw resource samples and post-round
recovery let a reviewer compare memory as cumulative stream length grows,
including against the ordinary four-stream cell. This is an observation
window, not an automatic proof that buffering or leaks never occur.

RSS/PSS must be read alongside Swap/SwapPss. The report also derives RSS+Swap
and PSS+SwapPss without subtracting any harness/control floor. These quantities
are not total allocations, and low resident readings on a swapping host do not
establish low memory use or actual low-memory-device acceptance.

The JSON records host load, available memory/swap and CPU/memory/I/O pressure
before/after each cell, process names with lifetime CPU percentages, OS/CPU,
local Python/Rust/Cargo versions, invocation, executable/runner/source/fixture
hashes and actual ephemeral configuration hash. A separate configuration
template hash excludes the random port. Source hashes cover root Cargo files
and workspace crate sources; they describe the concurrent checkout, **not
proof that the supplied binary was built from it**. Release flags, allocator
and linkage are explicitly unattested for a supplied executable.

The threaded Python Provider, clients, validation and sampler share a process
and GIL; Python CPU/scheduling, host contention and pacing can limit the run.
The direct path provides context for that overhead but does not calibrate it
away. There is no profiled limiter, cache dropping, independent five-round
original/replacement A/B, absolute performance pass, cross-platform execution,
or complete REQ-031/032/033/low-memory acceptance claim.
