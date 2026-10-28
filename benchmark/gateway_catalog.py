#!/usr/bin/env python3
"""Measure loopback startup and model-catalog requests on Linux."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import math
import os
import platform
import socket
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import tomllib
from pathlib import Path


MODELS_BODY = b'{"object":"list","data":[]}'


def digest(data: bytes) -> str:
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
    result = hashlib.sha256()
    for path in files:
        result.update(path.relative_to(root).as_posix().encode())
        result.update(b"\0")
        result.update(path.read_bytes())
        result.update(b"\0")
    return result.hexdigest()


def percentile(values: list[float], quantile: float) -> float:
    ordered = sorted(values)
    return ordered[max(0, math.ceil(quantile * len(ordered)) - 1)]


def parse_address(address: str) -> tuple[str, int]:
    host, port = address.rsplit(":", 1)
    return host.strip("[]"), int(port)


def request(address: tuple[str, int], path: str) -> tuple[int, dict[str, str], bytes, float]:
    started = time.perf_counter_ns()
    with socket.create_connection(address, timeout=2) as connection:
        connection.settimeout(2)
        connection.sendall(
            f"GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n".encode()
        )
        response = bytearray()
        while True:
            part = connection.recv(65536)
            if not part:
                break
            response.extend(part)
    elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
    header_end = response.find(b"\r\n\r\n")
    if header_end < 0:
        raise RuntimeError(f"{path}: response has no HTTP header terminator")
    lines = bytes(response[:header_end]).decode("ascii").split("\r\n")
    status = int(lines[0].split()[1])
    headers = {}
    for line in lines[1:]:
        name, value = line.split(":", 1)
        headers[name.lower()] = value.strip()
    body = bytes(response[header_end + 4 :])
    if int(headers.get("content-length", "-1")) != len(body):
        raise RuntimeError(f"{path}: content length does not match response bytes")
    return status, headers, body, elapsed_ms


def request_until(
    address: tuple[str, int], path: str, deadline: float
) -> tuple[int, dict[str, str], bytes, float]:
    while True:
        try:
            return request(address, path)
        except (OSError, RuntimeError):
            if time.monotonic() >= deadline:
                raise
            time.sleep(0.001)


def check_health(response: tuple[int, dict[str, str], bytes, float]) -> None:
    status, _, body, _ = response
    value = json.loads(body)
    if status != 200 or value.get("status") != "ok":
        raise RuntimeError(f"health check failed: status={status}, body={body!r}")


def check_models(response: tuple[int, dict[str, str], bytes, float], expected_ids: tuple[str, ...] = ()) -> None:
    status, _, body, _ = response
    value = json.loads(body)
    data = value.get("data", [])
    actual_ids = sorted(item.get("id") for item in data)
    if status != 200 or value.get("object") != "list" or actual_ids != sorted(expected_ids):
        raise RuntimeError(f"model catalog failed: status={status}, body={body!r}")


def start_server(binary: Path, env: dict[str, str], catalog: Path | None = None, expected_ids: tuple[str, ...] = ()) -> tuple[subprocess.Popen[str], tuple[str, int], int, float, float]:
    started_ns = time.perf_counter_ns()
    command = [str(binary), "serve", "--listen", "127.0.0.1:0"]
    if catalog is not None:
        command.extend(["--catalog", str(catalog)])
    process = subprocess.Popen(
        command,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=env,
    )
    line = process.stdout.readline()
    bound_ns = time.perf_counter_ns()
    if not line.startswith("Gateway listening at "):
        if process.poll() is None:
            process.terminate()
        _, stderr = process.communicate(timeout=3)
        raise RuntimeError(f"Gateway did not bind: stdout={line!r}, stderr={stderr!r}")
    address_text = line.removeprefix("Gateway listening at ").strip()
    address = parse_address(address_text)
    bound_ms = (bound_ns - started_ns) / 1_000_000
    deadline = time.monotonic() + 5
    health_response = request_until(address, "/health", deadline)
    health_ms = (time.perf_counter_ns() - started_ns) / 1_000_000
    check_health(health_response)
    models_response = request_until(address, "/v1/models", deadline)
    models_ms = (time.perf_counter_ns() - started_ns) / 1_000_000
    check_models(models_response, expected_ids)
    return process, address, bound_ms, health_ms, models_ms


def stop_server(process: subprocess.Popen[str]) -> None:
    if process.poll() is None:
        process.terminate()
    process.wait(timeout=3)
    if process.stdout:
        process.stdout.close()
    if process.stderr:
        process.stderr.close()


def read_proc_metrics(pid: int) -> dict[str, int]:
    values: dict[str, int] = {}
    for line in (Path("/proc") / str(pid) / "status").read_text().splitlines():
        if line.startswith("VmRSS:"):
            values["rss_kib"] = int(line.split()[1])
        elif line.startswith("Threads:"):
            values["threads"] = int(line.split()[1])
    for line in (Path("/proc") / str(pid) / "smaps_rollup").read_text().splitlines():
        if line.startswith("Pss:"):
            values["pss_kib"] = int(line.split()[1])
            break
    values["file_descriptors"] = len(list((Path("/proc") / str(pid) / "fd").iterdir()))
    return values


def summarize(values: list[float]) -> dict[str, float | list[float]]:
    return {
        "p50": statistics.median(values),
        "p95_nearest_rank": percentile(values, 0.95),
        "min": min(values),
        "max": max(values),
        "samples": values,
    }


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
    return {
        "os": platform.platform(),
        "kernel": platform.release(),
        "architecture": platform.machine(),
        "cpu": cpu,
        "logical_cpus": os.cpu_count(),
        "memory_kib": memory_kib,
        "load_average": os.getloadavg() if hasattr(os, "getloadavg") else None,
    }


def source_root() -> Path:
    for candidate in Path(__file__).resolve().parents:
        if (candidate / "Cargo.toml").is_file() and (candidate / "crates").is_dir():
            return candidate
    raise RuntimeError("cannot locate the Rust workspace root")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--runs", default=30, type=int)
    parser.add_argument("--warmup-starts", default=3, type=int)
    parser.add_argument("--requests", default=100, type=int)
    parser.add_argument("--idle-seconds", default=60, type=int)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--catalog", type=Path, help="synthetic catalog fixture; never use real account data")
    options = parser.parse_args()
    if not sys.platform.startswith("linux") or not Path("/proc/self/smaps_rollup").exists():
        parser.error("this runner currently requires Linux /proc RSS and PSS accounting")
    if options.runs < 5 or options.warmup_starts < 1 or options.requests < 30:
        parser.error("use at least 5 startup runs, 1 warmup, and 30 requests")

    root = source_root()
    binary = options.binary.resolve(strict=True)
    catalog = options.catalog.resolve(strict=True) if options.catalog else None
    fixture_bytes = catalog.read_bytes() if catalog else b"providers=[]\nmodels=[]\n"
    providers = tomllib.loads(fixture_bytes.decode()).get("providers", []) if catalog else []
    expected_ids = tuple(f"{provider['id']}/{model}" for provider in providers for model in provider['models'])
    if not os.access(binary, os.X_OK):
        parser.error(f"not executable: {binary}")
    startup_bind: list[float] = []
    startup_health: list[float] = []
    startup_models: list[float] = []
    startup_errors = 0
    machine = machine_info()
    rustc_version = subprocess.run(
        ["rustc", "--version"], text=True, capture_output=True, check=True
    ).stdout.strip()
    cargo_version = subprocess.run(
        ["cargo", "--version"], text=True, capture_output=True, check=True
    ).stdout.strip()

    with tempfile.TemporaryDirectory(prefix="magpie-gateway-bench-") as temporary:
        temp = Path(temporary)
        config_dir = temp / "magpie"
        home_dir = temp / "home"
        config_dir.mkdir()
        home_dir.mkdir()
        env = {
            "PATH": "/usr/bin:/bin",
            "HOME": str(home_dir),
            "MAGPIE_CONFIG_DIR": str(config_dir),
        }
        for index in range(options.warmup_starts):
            process, _, _, _, _ = start_server(binary, env, catalog, expected_ids)
            stop_server(process)
        for _ in range(options.runs):
            process, _, bind_ms, health_ms, models_ms = start_server(binary, env, catalog, expected_ids)
            startup_bind.append(bind_ms)
            startup_health.append(health_ms)
            startup_models.append(models_ms)
            stop_server(process)

        process, address, _, _, _ = start_server(binary, env, catalog, expected_ids)
        try:
            for _ in range(5):
                check_models(request(address, "/v1/models"), expected_ids)
            idle_samples: list[dict[str, int]] = []
            idle_end = time.monotonic() + options.idle_seconds
            while time.monotonic() < idle_end:
                idle_samples.append(read_proc_metrics(process.pid))
                time.sleep(0.1)

            sample_stop = threading.Event()
            request_samples: list[dict[str, int]] = []

            def sample_memory() -> None:
                while not sample_stop.is_set():
                    try:
                        request_samples.append(read_proc_metrics(process.pid))
                    except (FileNotFoundError, ProcessLookupError):
                        return
                    sample_stop.wait(0.01)

            sampler = threading.Thread(target=sample_memory, daemon=True)
            sampler.start()
            for _ in range(5):
                check_models(request(address, "/v1/models"), expected_ids)
            request_ms: list[float] = []
            response_hashes: list[str] = []
            request_errors = 0
            for _ in range(options.requests):
                try:
                    response = request(address, "/v1/models")
                    check_models(response, expected_ids)
                    request_ms.append(response[3])
                    response_hashes.append(digest(response[2]))
                except (OSError, ValueError, RuntimeError):
                    request_errors += 1
            sample_stop.set()
            sampler.join(timeout=1)
            if request_errors or len(set(response_hashes)) != 1:
                raise RuntimeError(
                    f"catalog requests were not stable: errors={request_errors}, "
                    f"responses={len(set(response_hashes))}"
                )
            if not idle_samples or not request_samples:
                raise RuntimeError("service memory sampler collected no samples")
        finally:
            stop_server(process)

    def memory_summary(samples: list[dict[str, int]]) -> dict[str, object]:
        fields = ("rss_kib", "pss_kib", "threads", "file_descriptors")
        return {
            key: {
                "median": statistics.median([sample[key] for sample in samples]),
                "max": max(sample[key] for sample in samples),
                "samples": [sample[key] for sample in samples],
            }
            for key in fields
        }

    machine["load_average_after"] = os.getloadavg() if hasattr(os, "getloadavg") else None
    result = {
        "schema_version": 1,
        "timestamp_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "git_head": subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=root, text=True, capture_output=True
        ).stdout.strip(),
        "source_tree_sha256": source_digest(root),
        "source_scope": "workspace Cargo manifests/lock and crates Rust/TOML files; excludes independent GPUI app",
        "binary": str(binary),
        "binary_size_bytes": binary.stat().st_size,
        "binary_sha256": digest(binary.read_bytes()),
        "runner_sha256": digest(Path(__file__).read_bytes()),
        "toolchain": {"rustc": rustc_version, "cargo": cargo_version},
        "machine": machine,
        "workload": {
            "provider_count": len(providers),
            "model_count": len(expected_ids),
            "fixture_sha256": digest(fixture_bytes),
            "endpoint": "GET /v1/models; validated configured catalog" if catalog else "GET /v1/models; valid empty catalog",
            "request_concurrency": 1,
            "measured_requests": options.requests,
            "correctness_errors": request_errors,
            "idle_window_seconds": options.idle_seconds,
            "idle_sample_interval_seconds": 0.1,
            "request_memory_sample_interval_seconds": 0.01,
        },
        "startup": {
            "runs": options.runs,
            "warmup_starts": options.warmup_starts,
            "errors": startup_errors,
            "to_bind_ms": summarize(startup_bind),
            "to_health_ms": summarize(startup_health),
            "to_models_ready_ms": summarize(startup_models),
        },
        "catalog_request_latency_ms": summarize(request_ms),
        "catalog_response_sha256": response_hashes[0],
        "idle_service_memory": memory_summary(idle_samples),
        "service_memory_during_catalog_requests": memory_summary(request_samples),
        "limitations": [
            "This measures the loopback HTTP/catalog path with a static catalog and no upstream calls.",
            "It does not measure model inference, upstream forwarding, streaming, or concurrent load.",
            "Cold page-cache startup was not measured.",
        ],
    }
    options.output.parent.mkdir(parents=True, exist_ok=True)
    options.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({
        "output": str(options.output),
        "binary_size_bytes": result["binary_size_bytes"],
        "startup_to_models_p50_ms": result["startup"]["to_models_ready_ms"]["p50"],
        "startup_to_models_p95_ms": result["startup"]["to_models_ready_ms"]["p95_nearest_rank"],
        "catalog_request_p50_ms": result["catalog_request_latency_ms"]["p50"],
        "catalog_request_p95_ms": result["catalog_request_latency_ms"]["p95_nearest_rank"],
        "idle_rss_kib_median": result["idle_service_memory"]["rss_kib"]["median"],
        "idle_pss_kib_median": result["idle_service_memory"]["pss_kib"]["median"],
        "catalog_request_errors": request_errors,
    }, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
