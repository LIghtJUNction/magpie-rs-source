#!/usr/bin/env python3
"""Measure release CLI startup on Linux with isolated synthetic Codex data."""

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


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def source_digest(root: Path) -> str:
    files = sorted(
        path
        for path in root.rglob("*")
        if path.is_file()
        and "benchmark" not in path.relative_to(root).parts
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


def run_child(binary: Path, args: list[str], env: dict[str, str]) -> tuple[int, bytes, bytes, float, int]:
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

    _, wait_status, usage = os.wait4(pid, 0)
    elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
    if os.WIFEXITED(wait_status):
        exit_code = os.WEXITSTATUS(wait_status)
    elif os.WIFSIGNALED(wait_status):
        exit_code = 128 + os.WTERMSIG(wait_status)
    else:
        exit_code = 255
    return exit_code, bytes(output["stdout"]), bytes(output["stderr"]), elapsed_ms, usage.ru_maxrss * 1024


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
        ):
            raise RuntimeError(f"{case}: incorrect discovery result {value!r}")
    elif case == "status_json":
        value = json.loads(text)
        if value["gateway"] != "not_started" or not value["config_dir"]:
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
    options = parser.parse_args()
    if not sys.platform.startswith("linux") or not hasattr(os, "wait4"):
        parser.error("this runner currently requires Linux wait4 resource accounting")
    if options.runs < 5 or options.warmup < 1:
        parser.error("use at least 5 measured runs and 1 warmup")

    root = source_root()
    binary = options.binary.resolve(strict=True)
    if not os.access(binary, os.X_OK):
        parser.error(f"not executable: {binary}")
    if shutil.which("rustc") is None:
        parser.error("rustc must be available to record the toolchain")

    machine = machine_info()
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
        }
        fixture_digest = sha256(
            b"bin/codex\0" + codex.read_bytes() + b"codex/config.toml\0" + CONFIG
        )
        cases: dict[str, object] = {}
        for name, args in CASES.items():
            for _ in range(options.warmup):
                sample = run_child(binary, args, env)
                validate(name, sample[0], sample[1], sample[2], fixture)
            times_ms: list[float] = []
            rss_bytes: list[int] = []
            output_hashes: list[str] = []
            for _ in range(options.runs):
                code, stdout, stderr, elapsed_ms, peak_rss_bytes = run_child(binary, args, env)
                validate(name, code, stdout, stderr, fixture)
                times_ms.append(elapsed_ms)
                rss_bytes.append(peak_rss_bytes)
                output_hashes.append(sha256(stdout))
            if len(set(output_hashes)) != 1:
                raise RuntimeError(f"{name}: stdout differed across measured runs")
            cases[name] = {
                "arguments": args,
                "runs": options.runs,
                "correctness_failures": 0,
                "stdout_sha256": output_hashes[0],
                "latency_ms": {
                    "p50": statistics.median(times_ms),
                    "p95_nearest_rank": percentile(times_ms, 0.95),
                    "min": min(times_ms),
                    "max": max(times_ms),
                    "samples": times_ms,
                },
                "peak_rss_bytes": {
                    "median": statistics.median(rss_bytes),
                    "max": max(rss_bytes),
                    "samples": rss_bytes,
                },
            }

    machine["load_average_after"] = os.getloadavg() if hasattr(os, "getloadavg") else None
    result = {
        "schema_version": 1,
        "timestamp_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "git_head": git_head(root),
        "source_tree_sha256": source_digest(root),
        "binary": str(binary),
        "binary_size_bytes": binary.stat().st_size,
        "binary_sha256": sha256(binary.read_bytes()),
        "runner_sha256": sha256(Path(__file__).read_bytes()),
        "fixture_sha256": fixture_digest,
        "fixture_config_bytes": len(CONFIG),
        "toolchain": {"rustc": rustc_version, "cargo": cargo_version},
        "profile": "release; opt-level=s; thin LTO; one codegen unit; stripped symbols",
        "machine": machine,
        "measurement": {
            "boundary": "parent fork to stdout/stderr EOF and wait4 child exit",
            "rss_source": "Linux wait4 ru_maxrss for the CLI process, converted from KiB",
            "warmup_runs_per_case": options.warmup,
            "cold_cache_runs": 0,
            "serial_cases": True,
        },
        "cases": cases,
    }
    options.output.parent.mkdir(parents=True, exist_ok=True)
    options.output.write_text(json.dumps(result, indent=2) + "\n")
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
