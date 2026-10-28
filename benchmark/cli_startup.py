#!/usr/bin/env python3
"""Measure CLI latency and, separately, GNU time peak RSS on synthetic data."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import math
import os
import platform
import selectors
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path


CASES = {
    "version": ["--version"],
    "agent_discovery_4k_toml": ["agents", "--json"],
    "status_json": ["status", "--json"],
    "config_path_json": ["config", "path", "--json"],
}
CONFIG_PREFIX = (
    "# Synthetic benchmark fixture; contains no user credentials.\n"
    'model = "fixture/provider-model"\n'
    "temperature = 0.2\n"
    "\n[model_providers.fixture]\n"
    'name = "fixture"\n'
    'base_url = "http://127.0.0.1:9/v1"\n'
).encode()


def make_config_fixture() -> bytes:
    result = bytearray(CONFIG_PREFIX)
    while len(result) < 4096:
        remaining = 4096 - len(result)
        line_size = min(80, remaining)
        if remaining - line_size == 1:
            line_size -= 1
        if line_size == 1:
            result.extend(b"\n")
        else:
            result.extend(b"#" + b"x" * (line_size - 2) + b"\n")
    return bytes(result)


CONFIG = make_config_fixture()
METHOD_VERSION = "cli-startup-v2-gnu-time-separate-rss"
RSS_PREFIX = "magpie_cli_peak_rss_kib="


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def source_digest(root: Path) -> str:
    files = sorted(
        path
        for path in root.rglob("*")
        if path.is_file()
        and (path.parent == root or path.relative_to(root).parts[0] == "crates")
        and (path.suffix in {".rs", ".toml"} or path.name == "Cargo.lock")
        and "target" not in path.parts
    )
    digest = hashlib.sha256()
    for path in files:
        digest.update(path.relative_to(root).as_posix().encode())
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def percentile(values: list[float], quantile: float) -> float:
    ordered = sorted(values)
    return ordered[max(0, math.ceil(quantile * len(ordered)) - 1)]


def run_child(binary: Path, args: list[str], env: dict[str, str]) -> tuple[int, bytes, bytes, float]:
    out_read, out_write = os.pipe()
    err_read, err_write = os.pipe()
    started = time.perf_counter_ns()
    pid = os.fork()
    if pid == 0:
        try:
            os.close(out_read)
            os.close(err_read)
            os.dup2(out_write, 1)
            os.dup2(err_write, 2)
            os.close(out_write)
            os.close(err_write)
            os.execve(str(binary), [str(binary), *args], env)
        except BaseException as error:
            os.write(2, f"benchmark child exec failed: {error}\n".encode())
            os._exit(127)

    os.close(out_write)
    os.close(err_write)
    selector = selectors.DefaultSelector()
    selector.register(out_read, selectors.EVENT_READ, "stdout")
    selector.register(err_read, selectors.EVENT_READ, "stderr")
    output = {"stdout": bytearray(), "stderr": bytearray()}
    while selector.get_map():
        for key, _ in selector.select():
            try:
                chunk = os.read(key.fd, 65536)
            except BlockingIOError:
                continue
            if chunk:
                output[key.data].extend(chunk)
            else:
                selector.unregister(key.fd)
                os.close(key.fd)

    selector.close()
    _, wait_status = os.waitpid(pid, 0)
    elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
    if os.WIFEXITED(wait_status):
        exit_code = os.WEXITSTATUS(wait_status)
    elif os.WIFSIGNALED(wait_status):
        exit_code = 128 + os.WTERMSIG(wait_status)
    else:
        exit_code = 255
    return exit_code, bytes(output["stdout"]), bytes(output["stderr"]), elapsed_ms


def parse_peak_rss(report: str) -> int:
    line = report.strip()
    if not line.startswith(RSS_PREFIX) or not line[len(RSS_PREFIX):].isascii():
        raise RuntimeError(f"invalid GNU time RSS report: {report!r}")
    value = line[len(RSS_PREFIX):]
    if not value.isdecimal():
        raise RuntimeError(f"invalid GNU time RSS report: {report!r}")
    return int(value) * 1024


def run_rss(
    time_binary: Path, binary: Path, args: list[str], env: dict[str, str], report: Path
) -> tuple[int, bytes, bytes, float, int]:
    # time execs before it forks the measured child, removing Python's RSS floor.
    report.unlink(missing_ok=True)
    code, stdout, stderr, wrapped_ms = run_child(
        time_binary,
        ["--quiet", "--format=" + RSS_PREFIX + "%M", "--output=" + str(report),
         "--", str(binary), *args],
        env,
    )
    peak_rss = parse_peak_rss(report.read_text())
    return code, stdout, stderr, wrapped_ms, peak_rss


def sample_summary(values: list[float] | list[int]) -> dict[str, object]:
    return {
        "median": statistics.median(values),
        "min": min(values),
        "max": max(values),
        "samples": values,
    }


def current_rss_bytes() -> int:
    for line in Path("/proc/self/status").read_text().splitlines():
        if line.startswith("VmRSS:"):
            return int(line.split()[1]) * 1024
    raise RuntimeError("cannot read runner RSS")


def check_runner(time_binary: Path, fixture: Path, env: dict[str, str]) -> dict[str, object]:
    """Exercise the accounting boundary and error/pipe handling, outside timing."""
    report = fixture / "self-check-rss.txt"
    python = Path(sys.executable).resolve()
    bad_reports = ["", RSS_PREFIX, RSS_PREFIX + "-1", RSS_PREFIX + "1.0",
                   RSS_PREFIX + "1\n" + RSS_PREFIX + "2", "other=12", RSS_PREFIX + "12x"]
    for value in bad_reports:
        try:
            parse_peak_rss(value)
        except RuntimeError:
            continue
        raise RuntimeError(f"RSS parser accepted malformed report: {value!r}")
    if parse_peak_rss(RSS_PREFIX + "123\n") != 123 * 1024:
        raise RuntimeError("RSS KiB conversion failed")

    code = (
        "import sys; sys.stdout.buffer.write(b'o'*131072); "
        "sys.stderr.buffer.write(b'e'*131072); sys.exit(7)"
    )
    for wrapped in (False, True):
        sample = (
            run_rss(time_binary, python, ["-I", "-S", "-c", code], env, report)
            if wrapped else run_child(python, ["-I", "-S", "-c", code], env)
        )
        if sample[:3] != (7, b"o" * 131072, b"e" * 131072):
            raise RuntimeError("runner lost output or nonzero exit status")
        signal_args = ["-I", "-S", "-c", "import os; os.kill(os.getpid(), 15)"]
        signalled = (
            run_rss(time_binary, python, signal_args, env, report)
            if wrapped else run_child(python, signal_args, env)
        )
        if signalled[:3] != (143, b"", b""):
            raise RuntimeError("runner lost signal exit status")
    try:
        validate("version", 7, b"magpie fixture\n", b"", fixture)
    except RuntimeError:
        pass
    else:
        raise RuntimeError("validation accepted a failed command")
    try:
        validate("config_path_json", 0, b'{"config_dir":"/wrong"}', b"", fixture)
    except RuntimeError:
        pass
    else:
        raise RuntimeError("validation accepted incorrect work")

    def empty_rss() -> int:
        sample = run_rss(time_binary, Path("/usr/bin/true"), [], env, report)
        if sample[:3] != (0, b"", b""):
            raise RuntimeError("GNU time empty control failed")
        return sample[4]

    before_rss = current_rss_bytes()
    baseline = [empty_rss() for _ in range(5)]
    ballast_bytes = 64 * 1024 * 1024
    ballast = bytearray(b"x") * ballast_bytes
    padded_rss = current_rss_bytes()
    padded = [empty_rss() for _ in range(5)]
    if padded_rss - before_rss < ballast_bytes * 0.9:
        raise RuntimeError("parent-memory contamination check did not touch its ballast")
    if max(padded) >= ballast_bytes // 2 or max(padded) > max(baseline) + 8 * 1024 * 1024:
        raise RuntimeError("GNU time RSS still appears contaminated by Python parent memory")
    probe_args = [
        "-I", "-S", "-c",
        "p=bytearray(16*1024*1024)\n"
        "for i in range(0,len(p),4096): p[i]=1\n"
        "print(sum(p))",
    ]
    probe = run_rss(time_binary, python, probe_args, env, report)
    if probe[:3] != (0, b"4096\n", b"") or probe[4] < 16 * 1024 * 1024:
        raise RuntimeError("RSS positive control failed to account for touched target memory")
    del ballast
    return {
        "passed": True,
        "malformed_reports_rejected": len(bad_reports),
        "kib_to_bytes_checked": True,
        "large_stdout_and_stderr_bytes_each": 131072,
        "nonzero_exit_checked_direct_and_wrapped": 7,
        "signal_exit_checked_direct_and_wrapped": 143,
        "failed_command_and_wrong_output_rejected": True,
        "parent_ballast_bytes": ballast_bytes,
        "runner_rss_before_bytes": before_rss,
        "runner_rss_with_ballast_bytes": padded_rss,
        "empty_control_before_bytes": sample_summary(baseline),
        "empty_control_with_ballast_bytes": sample_summary(padded),
        "positive_control_touched_bytes": 16 * 1024 * 1024,
        "positive_control_reported_peak_rss_bytes": probe[4],
    }


def calibrate_wrapper(
    time_binary: Path, fixture: Path, env: dict[str, str], runs: int, warmup: int
) -> dict[str, object]:
    control = Path("/usr/bin/true")
    report = fixture / "calibration-rss.txt"
    direct_ms: list[float] = []
    wrapped_ms: list[float] = []
    rss_bytes: list[int] = []
    for index in range(warmup + runs):
        pair = {}
        for wrapped in ((False, True) if index % 2 == 0 else (True, False)):
            sample = (
                run_rss(time_binary, control, [], env, report)
                if wrapped else run_child(control, [], env)
            )
            if sample[:3] != (0, b"", b""):
                raise RuntimeError("wrapper calibration failed")
            pair[wrapped] = sample
        if index >= warmup:
            direct_ms.append(pair[False][3])
            wrapped_ms.append(pair[True][3])
            rss_bytes.append(pair[True][4])
    return {
        "command": [str(control)],
        "binary_sha256": sha256(control.read_bytes()),
        "runs_per_side": runs,
        "warmup_per_side": warmup,
        "order": "paired direct/wrapped; first side alternates every pair",
        "direct_latency_ms": sample_summary(direct_ms),
        "wrapped_latency_ms": sample_summary(wrapped_ms),
        "paired_wrapper_delta_ms": sample_summary(
            [wrapped - direct for direct, wrapped in zip(direct_ms, wrapped_ms)]
        ),
        "empty_command_peak_rss_bytes": sample_summary(rss_bytes),
        "interpretation": (
            "Empty control includes true/loader plus any C child pre-exec high-water floor; "
            "it is an empirical control, not an exact additive overhead. Zero is possible "
            "with Linux RSS accounting for a tiny process and does not mean no memory used. "
            "No latency or RSS subtraction is applied."
        ),
    }


def validate(case: str, code: int, stdout: bytes, stderr: bytes, fixture: Path) -> None:
    if code != 0 or stderr:
        raise RuntimeError(f"{case}: exit={code}, stderr={stderr.decode(errors='replace')!r}")
    text = stdout.decode("utf-8", errors="strict").strip()
    if case == "version":
        if not text.startswith("magpie "):
            raise RuntimeError(f"{case}: unexpected output {text!r}")
    elif case == "agent_discovery_4k_toml":
        value = json.loads(text)
        item = value["items"][0]
        if (
            len(value["items"]) != 1
            or item["id"] != "codex"
            or not item["installed"]
            or item["config_state"] != "valid_toml"
            or item["config_path"] != str(fixture / "codex" / "config.toml")
            or item["model"] != "fixture/provider-model"
            or item["model_provider"] is not None
            or item["reasoning_effort"] is not None
        ):
            raise RuntimeError(f"{case}: incorrect discovery result {value!r}")
    elif case == "status_json":
        value = json.loads(text)
        if value["gateway"] != "not_checked" or value["config_dir"] != str(fixture / "magpie"):
            raise RuntimeError(f"{case}: incorrect status result {value!r}")
    elif case == "config_path_json":
        value = json.loads(text)
        if value["config_dir"] != str(fixture / "magpie"):
            raise RuntimeError(f"{case}: incorrect config path {value!r}")


def machine_info() -> dict[str, object]:
    cpu = "unknown"
    try:
        for line in Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                cpu = line.split(":", 1)[1].strip()
                break
    except OSError:
        pass
    memory_kib = None
    try:
        for line in Path("/proc/meminfo").read_text().splitlines():
            if line.startswith("MemTotal:"):
                memory_kib = int(line.split()[1])
                break
    except OSError:
        pass
    load = os.getloadavg() if hasattr(os, "getloadavg") else None
    return {
        "os": platform.platform(),
        "kernel": platform.release(),
        "architecture": platform.machine(),
        "cpu": cpu,
        "logical_cpus": os.cpu_count(),
        "memory_kib": memory_kib,
        "load_average_before": load,
        "python": sys.version.split()[0],
    }


def host_conditions() -> dict[str, object]:
    memory = {}
    for line in Path("/proc/meminfo").read_text().splitlines():
        key, value = line.split(":", 1)
        if key in {"MemAvailable", "SwapTotal", "SwapFree"}:
            memory[key] = int(value.split()[0])
    processes = subprocess.run(
        ["ps", "-eo", "comm,pcpu,rss", "--sort=-pcpu"],
        text=True, capture_output=True, check=True,
    ).stdout.splitlines()[:13]
    return {
        "timestamp_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "load_average": os.getloadavg(),
        "memory_kib": memory,
        "pressure": {
            name: Path(f"/proc/pressure/{name}").read_text().strip()
            for name in ("cpu", "memory", "io")
        },
        "top_processes_by_lifetime_cpu_percent": processes,
        "cpu_affinity": sorted(os.sched_getaffinity(0)),
    }


def git_head(root: Path) -> str | None:
    result = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=root, text=True, capture_output=True, check=False
    )
    return result.stdout.strip() if result.returncode == 0 else None


def source_root() -> Path:
    for candidate in Path(__file__).resolve().parents:
        if (candidate / "Cargo.toml").is_file() and (candidate / "crates").is_dir():
            return candidate
    raise RuntimeError("cannot locate the Rust workspace root")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path, help="release magpie executable")
    parser.add_argument("--runs", default=30, type=int, help="measured process starts per case")
    parser.add_argument("--warmup", default=5, type=int, help="unrecorded starts per case")
    parser.add_argument("--output", required=True, type=Path, help="machine-readable JSON result")
    parser.add_argument("--time-binary", default=Path("/usr/bin/time"), type=Path,
                        help="GNU time executable; may be extracted outside the checkout")
    parser.add_argument("--time-provenance", type=Path,
                        help="optional JSON describing acquisition of the GNU time executable")
    options = parser.parse_args()
    if not sys.platform.startswith("linux") or not hasattr(os, "fork"):
        parser.error("this runner currently requires Linux fork and GNU time")
    if options.runs < 5 or options.warmup < 1:
        parser.error("use at least 5 measured runs and 1 warmup")

    root = source_root()
    binary = options.binary.resolve(strict=True)
    time_binary = options.time_binary.resolve(strict=True)
    if not os.access(binary, os.X_OK):
        parser.error(f"not executable: {binary}")
    if shutil.which("rustc") is None:
        parser.error("rustc must be available to record the toolchain")
    if options.output.exists():
        parser.error("output already exists; use a new filename to preserve raw history")
    time_version = subprocess.run(
        [str(time_binary), "--version"], text=True, capture_output=True, check=True
    ).stdout.splitlines()[0]
    if "GNU" not in time_version:
        parser.error("--time-binary must be GNU time")

    machine = machine_info()
    machine["conditions_before"] = host_conditions()
    captured_head = git_head(root)
    captured_source_digest = source_digest(root)
    binary_bytes = binary.read_bytes()
    binary_digest = sha256(binary_bytes)
    rustc_version = subprocess.run(
        ["rustc", "--version"], text=True, capture_output=True, check=True
    ).stdout.strip()
    cargo_version = subprocess.run(
        ["cargo", "--version"], text=True, capture_output=True, check=True
    ).stdout.strip()

    with tempfile.TemporaryDirectory(prefix="magpie-bench-") as temporary:
        fixture = Path(temporary)
        bin_dir = fixture / "bin"
        codex_dir = fixture / "codex"
        magpie_dir = fixture / "magpie"
        home_dir = fixture / "home"
        for directory in (bin_dir, codex_dir, magpie_dir, home_dir):
            directory.mkdir()
        codex = bin_dir / "codex"
        codex.write_bytes(b"synthetic codex executable marker\n")
        codex.chmod(0o755)
        (codex_dir / "config.toml").write_bytes(CONFIG)
        env = {
            "PATH": str(bin_dir),
            "HOME": str(home_dir),
            "CODEX_HOME": str(codex_dir),
            "MAGPIE_CONFIG_DIR": str(magpie_dir),
            "LC_ALL": "C",
        }
        snapshot_binary = fixture / "magpie-binary"
        snapshot_binary.write_bytes(binary_bytes)
        snapshot_binary.chmod(0o755)
        runner_checks = check_runner(time_binary, fixture, env)
        calibration = calibrate_wrapper(time_binary, fixture, env, options.runs, options.warmup)
        fixture_digest = sha256(
            b"bin/codex\0" + codex.read_bytes() + b"codex/config.toml\0" + CONFIG
        )
        cases = {
            name: {
                "arguments": args,
                "runs": options.runs,
                "runs_per_pass": options.runs,
                "validated_starts_including_warmup": 2 * (options.runs + options.warmup),
                "correctness_failures": 0,
                "latency_ms": {"samples": []},
                "peak_rss_bytes": {"samples": []},
            }
            for name, args in CASES.items()
        }
        pass_conditions = {}
        names = list(CASES)
        for kind in ("latency_ms", "peak_rss_bytes"):
            pass_conditions[kind] = {"before": host_conditions()}
            for index in range(options.warmup + options.runs):
                # Rotate the first case so case order does not consistently favor one path.
                offset = index % len(names)
                for name in names[offset:] + names[:offset]:
                    sample = (
                        run_child(snapshot_binary, CASES[name], env) if kind == "latency_ms"
                        else run_rss(time_binary, snapshot_binary, CASES[name], env,
                                     fixture / "cli-rss.txt")
                    )
                    code, stdout, stderr = sample[:3]
                    validate(name, code, stdout, stderr, fixture)
                    case = cases[name]
                    output_hash = sha256(stdout)
                    if "stdout_sha256" not in case:
                        case["stdout_sha256"] = output_hash
                        case["stdout_utf8"] = stdout.decode()
                    if case["stdout_sha256"] != output_hash:
                        raise RuntimeError(f"{name}: stdout changed across passes/warmups")
                    if kind == "peak_rss_bytes" and sample[4] == 0:
                        raise RuntimeError(f"{name}: zero RSS is unresolved, not a valid CLI memory result")
                    if index >= options.warmup:
                        case[kind]["samples"].append(sample[3 if kind == "latency_ms" else 4])
            pass_conditions[kind]["after"] = host_conditions()
        for case in cases.values():
            for kind in ("latency_ms", "peak_rss_bytes"):
                values = case[kind]["samples"]
                case[kind] = sample_summary(values)
                if kind == "latency_ms":
                    case[kind]["p50"] = statistics.median(values)
                    case[kind]["p95_nearest_rank"] = percentile(values, 0.95)

    machine["load_average_after"] = os.getloadavg() if hasattr(os, "getloadavg") else None
    machine["conditions_after"] = host_conditions()
    result = {
        "schema_version": 2,
        "timestamp_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "git_head": captured_head,
        "source_tree_sha256": captured_source_digest,
        "source_scope": "workspace Cargo manifests/lock and crates Rust/TOML files; excludes independent GPUI app",
        "source_provenance": "checkout captured at runner start; prebuilt binary supplied separately, not rebuilt or build-attested by this runner",
        "binary": str(binary),
        "binary_version": cases["version"]["stdout_utf8"].strip(),
        "binary_size_bytes": len(binary_bytes),
        "binary_sha256": binary_digest,
        "execution": "all CLI starts use one byte-for-byte executable snapshot in the temporary fixture directory",
        "runner_sha256": sha256(Path(__file__).read_bytes()),
        "fixture_sha256": fixture_digest,
        "fixture_config_bytes": len(CONFIG),
        "toolchain": {"rustc": rustc_version, "cargo": cargo_version},
        "profile": "release; opt-level=s; thin LTO; one codegen unit; stripped symbols",
        "machine": machine,
        "measurement": {
            "method_version": METHOD_VERSION,
            "claim": "CLI startup and peak RSS on four synthetic warm-cache cases; no optimization or original-Magpie comparison",
            "latency_boundary": "Python parent fork to stdout/stderr EOF and waitpid child exit; includes Python fork/scheduling/pipe overhead, excludes output validation",
            "rss_source": "GNU time %M, Linux waited-child ru_maxrss in KiB multiplied by 1024; separate process launches from latency",
            "rss_boundary": "timed child lifetime: loader and CLI plus any GNU time C child pre-exec high-water floor; Python parent and the GNU time monitoring process are excluded",
            "rss_limitations": "peak resident pages, not PSS/private/allocated memory or aggregate process-tree memory; shared mappings count; no periodic sampling or overhead subtraction",
            "time_binary": str(time_binary),
            "time_binary_sha256": sha256(time_binary.read_bytes()),
            "time_version": time_version,
            "time_acquisition": (
                json.loads(options.time_provenance.read_text()) if options.time_provenance else None
            ),
            "warmup_runs_per_case_per_pass": options.warmup,
            "measured_runs_per_case_per_pass": options.runs,
            "cold_cache_runs": 0,
            "cache_state": "five default warmups per case per pass; OS page cache not dropped; fixture filesystem cache uncontrolled",
            "pass_order": ["latency_ms", "peak_rss_bytes"],
            "case_order": "serial, one launch per case per round; first case rotates by round",
            "pass_host_conditions": pass_conditions,
            "historical_comparability": "schema v1 RSS includes Python pre-exec memory; corrected RSS is a method correction, not memory savings. Loaded host and changed case order also preclude a latency speedup claim.",
            "limiter": "not profiled; sub-millisecond/millisecond process startup is mixed with fork, loader, scheduling and pipe overhead; loaded-host latency is descriptive only",
        },
        "runner_correctness": runner_checks,
        "wrapper_calibration": calibration,
        "cases": cases,
    }
    options.output.parent.mkdir(parents=True, exist_ok=True)
    with options.output.open("x") as output:
        output.write(json.dumps(result, indent=2) + "\n")
    print(json.dumps({
        "output": str(options.output),
        "binary_size_bytes": result["binary_size_bytes"],
        "cases": {
            name: {
                "p50_ms": value["latency_ms"]["p50"],
                "p95_ms": value["latency_ms"]["p95_nearest_rank"],
                "peak_rss_median_bytes": value["peak_rss_bytes"]["median"],
                "peak_rss_max_bytes": value["peak_rss_bytes"]["max"],
                "errors": value["correctness_failures"],
            }
            for name, value in cases.items()
        },
    }, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
