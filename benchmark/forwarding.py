#!/usr/bin/env python3
"""Linux loopback forwarding baseline; standard library only, synthetic data only."""

from __future__ import annotations

import argparse
import concurrent.futures
import contextlib
import datetime as dt
import hashlib
import http.client
import http.server
import json
import math
import os
import platform
import select
import selectors
import shutil
import signal
import socket
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from collections import Counter
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
EVENT_BYTES = 16 * 1024
PERIOD = 0.1
ROUND_INTERVAL = 0.05
ACTIVE_CAP = 4
BODY_LIMIT = 64 * 1024
IO_TIMEOUT = 3.0
MAX_REQUESTS = 250_000
DONE = b"data: [DONE]\n\n"


def encoded(value):
    return json.dumps(value, separators=(",", ":"), sort_keys=True).encode("ascii")


def sha256(raw):
    return hashlib.sha256(raw).hexdigest()


def padded(value, field, size, prefix=b"", suffix=b""):
    target = value
    for key in field[:-1]:
        target = target[key]
    target[field[-1]] = ""
    target[field[-1]] = "x" * (size - len(prefix + encoded(value) + suffix))
    raw = prefix + encoded(value) + suffix
    if len(raw) != size:
        raise ValueError("fixture_size")
    return raw


CHAT = padded({
    "id": "bench-chat", "object": "chat.completion", "model": "bench",
    "choices": [{"index": 0, "message": {"role": "assistant", "content": "fixture"},
                 "finish_reason": "stop"}],
    "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7},
}, ("choices", 0, "message", "content"), BODY_LIMIT)
DELTA = padded({
    "id": "bench-chat", "object": "chat.completion.chunk", "model": "bench",
    "choices": [{"index": 0, "delta": {"content": "fixture"}, "finish_reason": None}],
}, ("choices", 0, "delta", "content"), EVENT_BYTES, b"data: ", b"\n\n")
FINISH = padded({
    "id": "bench-chat", "object": "chat.completion.chunk", "model": "bench",
    "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
    "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7},
}, ("choices", 0, "delta", "content"), EVENT_BYTES, b"data: ", b"\n\n")
CHAT_HASH = sha256(CHAT)


class Fixture:
    def __init__(self, seconds):
        self.events = max(2, math.ceil(seconds / PERIOD))
        self.seconds = self.events * PERIOD
        digest = hashlib.sha256()
        for _ in range(self.events):
            digest.update(DELTA)
        digest.update(FINISH + DONE)
        self.stream_hash = digest.hexdigest()
        self.stream_bytes = self.events * EVENT_BYTES + len(FINISH) + len(DONE)

    def describe(self):
        return {
            "delta_events": self.events, "nominal_seconds": self.seconds,
            "stream_bytes": self.stream_bytes, "stream_sha256": self.stream_hash,
            "delta_event_bytes": len(DELTA), "delta_sha256": sha256(DELTA),
            "finish_event_bytes": len(FINISH), "finish_sha256": sha256(FINISH),
            "done_sha256": sha256(DONE), "nonstream_bytes": len(CHAT),
            "nonstream_sha256": sha256(CHAT), "events_per_second": 10,
        }


def summary(values):
    if not values:
        return {"count": 0, "p50": None, "p95_nearest_rank": None}
    values = sorted(values)
    return {"count": len(values), "p50": statistics.median(values),
            "p95_nearest_rank": values[math.ceil(.95 * len(values)) - 1],
            "min": values[0], "max": values[-1]}


def command(args, env=None, timeout=5):
    limits = {"stdout": 8192, "stderr": 1024}
    output = {name: bytearray() for name in limits}
    process = None
    error = None
    try:
        process = subprocess.Popen(
            args, cwd=ROOT, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, start_new_session=True, bufsize=0)
        with selectors.DefaultSelector() as selector:
            for name, pipe in (("stdout", process.stdout), ("stderr", process.stderr)):
                os.set_blocking(pipe.fileno(), False)
                selector.register(pipe, selectors.EVENT_READ, name)
            deadline = time.monotonic() + timeout
            while selector.get_map() or process.poll() is None:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    error = "command_deadline"
                    break
                for key, _ in selector.select(min(.05, remaining)):
                    try:
                        raw = os.read(key.fd, 4096)
                    except BlockingIOError:
                        continue
                    if not raw:
                        selector.unregister(key.fileobj)
                        continue
                    name = key.data
                    room = limits[name] - len(output[name])
                    output[name].extend(raw[:room])
                    if len(raw) > room:
                        error = "command_output_limit"
                        break
                if error:
                    break
    except OSError as exception:
        error = type(exception).__name__
    finally:
        if process:
            # Also kill descendants retaining a pipe after their parent exits.
            with contextlib.suppress(ProcessLookupError):
                os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=1.5)
            process.stdout.close()
            process.stderr.close()
    result = {"exit": process.returncode if process else None,
              **{name: raw.decode("utf-8", "replace").strip() for name, raw in output.items()}}
    if error:
        result["error"] = error
    return result


def text(path):
    try:
        return Path(path).read_text()[:65536]
    except OSError:
        return None


def host_conditions():
    return {
        "timestamp_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "load_average": os.getloadavg(),
        "cpu_affinity": sorted(os.sched_getaffinity(0)),
        "meminfo": text("/proc/meminfo"),
        "pressure": {name: text(f"/proc/pressure/{name}")
                     for name in ("cpu", "memory", "io")},
        "busy_processes": command(["ps", "-eo", "comm,pcpu,rss", "--sort=-pcpu"]),
    }


def source_identity():
    digest = hashlib.sha256()
    files = sorted([*ROOT.glob("Cargo.*"), *ROOT.glob("crates/**/*.rs"),
                    *ROOT.glob("crates/**/Cargo.toml")])
    manifest = {}
    for path in files:
        name = path.relative_to(ROOT).as_posix()
        raw = path.read_bytes()
        digest.update(name.encode() + b"\0" + raw + b"\0")
        manifest[name] = sha256(raw)
    return {
        "sha256": digest.hexdigest(), "files": manifest,
        "scope": "root Cargo.* and crates Rust sources/Cargo.toml; excludes GPUI",
        "head": command(["git", "rev-parse", "HEAD"]),
        "warning": "checkout context only; does not attest prebuilt binary provenance",
    }


def proc_metrics(pid):
    base = Path("/proc") / str(pid)
    fields = {}
    for line in (base / "smaps_rollup").read_text().splitlines():
        key, _, value = line.partition(":")
        if key in ("Rss", "Pss", "Swap", "SwapPss"):
            fields[{"Rss": "rss_kib", "Pss": "pss_kib", "Swap": "swap_kib",
                    "SwapPss": "swap_pss_kib"}[key]] = int(value.split()[0])
    if len(fields) != 4:
        raise RuntimeError("incomplete_smaps_rollup")
    fields["threads"] = next(int(line.split()[1])
                             for line in (base / "status").read_text().splitlines()
                             if line.startswith("Threads:"))
    fields["fds"] = len(list((base / "fd").iterdir()))
    children = set()
    for task in (base / "task").iterdir():
        try:
            children.update(map(int, (task / "children").read_text().split()))
        except FileNotFoundError:
            pass
    fields["children"] = sorted(children)
    stat = (base / "stat").read_text().rsplit(")", 1)[1].split()
    fields["cpu_seconds"] = (int(stat[11]) + int(stat[12])) / os.sysconf("SC_CLK_TCK")
    fields["resident_plus_swap_kib"] = fields["rss_kib"] + fields["swap_kib"]
    fields["pss_plus_swap_pss_kib"] = fields["pss_kib"] + fields["swap_pss_kib"]
    return fields


def memory_summary(samples):
    if not samples:
        return {"samples": 0}
    return {"samples": len(samples), **{
        key: summary([sample["target"][key] for sample in samples])
        for key in ("rss_kib", "pss_kib", "swap_kib", "swap_pss_kib",
                    "resident_plus_swap_kib", "pss_plus_swap_pss_kib", "threads", "fds")
    }}


class Sampler:
    def __init__(self, pid, interval, provider):
        self.pid, self.interval, self.provider = pid, interval, provider
        self.started = time.monotonic()
        self.phase = "startup"
        self.samples, self.errors = [], []
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)

    def run(self):
        while not self.stop.is_set():
            try:
                self.samples.append({
                    "t": time.monotonic() - self.started, "phase": self.phase,
                    "target": proc_metrics(self.pid), "harness": proc_metrics(os.getpid()),
                    "provider": self.provider.snapshot(),
                })
            except (OSError, ValueError, RuntimeError) as error:
                if len(self.errors) < 20:
                    self.errors.append(type(error).__name__ + ": " + str(error)[:160])
            self.stop.wait(self.interval)

    def window(self, phase, seconds):
        self.phase = phase
        threading.Event().wait(seconds)
        return memory_summary([s for s in self.samples if s["phase"] == phase])

    def close(self):
        self.stop.set()
        self.thread.join(timeout=IO_TIMEOUT)


class Gateway:
    """Bounded startup and continuous capped pipe draining, independent of tests."""

    def __init__(self, binary, config, env):
        self.command = [str(binary), "serve", "--listen", "127.0.0.1:0",
                        "--forward", str(config)]
        self.process = subprocess.Popen(
            self.command, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, bufsize=0, start_new_session=True)
        self.output = {"stdout": bytearray(), "stderr": bytearray()}
        self.selector = selectors.DefaultSelector()
        self.stop = threading.Event()
        self.drainer = None
        for name, pipe in (("stdout", self.process.stdout), ("stderr", self.process.stderr)):
            os.set_blocking(pipe.fileno(), False)
            self.selector.register(pipe, selectors.EVENT_READ, name)

    def pump(self, timeout):
        for key, _ in self.selector.select(timeout):
            try:
                raw = os.read(key.fd, 4096)
            except BlockingIOError:
                continue
            if not raw:
                self.selector.unregister(key.fileobj)
            else:
                output = self.output[key.data]
                output.extend(raw[:max(0, 4096 - len(output))])

    def start(self):
        started = time.monotonic()
        deadline = started + 5
        while b"\n" not in self.output["stdout"]:
            if time.monotonic() >= deadline:
                raise RuntimeError("gateway_startup_deadline")
            if len(self.output["stdout"]) == 4096 or not self.selector.get_map():
                raise RuntimeError("gateway_startup_output")
            self.pump(min(.05, max(0, deadline - time.monotonic())))
        line = bytes(self.output["stdout"]).split(b"\n", 1)[0].decode("ascii")
        prefix = "Forward candidate bound at 127.0.0.1:"
        if not line.startswith(prefix):
            raise RuntimeError("gateway_unexpected_bind_line")
        port = int(line[len(prefix):].split()[0])
        if not 0 < port < 65536:
            raise RuntimeError("gateway_invalid_port")
        self.address = ("127.0.0.1", port)

        def drain():
            while not self.stop.is_set():
                self.pump(.05)
        self.drainer = threading.Thread(target=drain, daemon=True)
        self.drainer.start()
        health = get_json(self.address, "/health", max(.01, deadline - time.monotonic()))
        if health.get("mode") != "forward_candidate" or health.get("status") != "ok":
            raise RuntimeError("gateway_not_forward_candidate")
        limits = health.get("limits", {})
        if limits != {"active_requests": 4, "waiting_requests": 0, "connections": 16}:
            raise RuntimeError("candidate_admission_limits_changed")
        models = get_json(self.address, "/v1/models", max(.01, deadline - time.monotonic()))
        if sorted(item["id"] for item in models.get("data", [])) != ["local/bench"]:
            raise RuntimeError("gateway_catalog_mismatch")
        if time.monotonic() > deadline:
            raise RuntimeError("gateway_readiness_deadline")
        return {"elapsed_ms": (time.monotonic() - started) * 1000,
                "health": health, "models": models}

    def close(self):
        for sig, timeout in ((signal.SIGTERM, .5), (signal.SIGKILL, 1.5)):
            try:
                os.killpg(self.process.pid, sig)
            except ProcessLookupError:
                pass
            try:
                self.process.wait(timeout=timeout)
                break
            except subprocess.TimeoutExpired:
                continue
        # A child in the owned process group may outlive the gateway's main PID.
        with contextlib.suppress(ProcessLookupError):
            os.killpg(self.process.pid, signal.SIGKILL)
        self.stop.set()
        if self.drainer:
            self.drainer.join(timeout=IO_TIMEOUT)
        self.selector.close()
        self.process.stdout.close()
        self.process.stderr.close()
        return {"returncode": self.process.returncode,
                "output_prefix": {key: bytes(raw).decode("utf-8", "replace")
                                  for key, raw in self.output.items()}}


def get_json(address, path, timeout=IO_TIMEOUT):
    with contextlib.closing(http.client.HTTPConnection(*address, timeout=timeout)) as connection:
        connection.request("GET", path, headers={"Connection": "close"})
        response = connection.getresponse()
        raw = response.read(BODY_LIMIT + 1)
        if response.status != 200 or len(raw) > BODY_LIMIT:
            raise RuntimeError("health_or_models_failed")
        return json.loads(raw)


class Provider(http.server.ThreadingHTTPServer):
    daemon_threads = True
    request_queue_size = 32

    def __init__(self):
        super().__init__(("127.0.0.1", 0), Handler)
        self.lock = threading.Lock()
        self.nodes = {}
        self.sequence = 0
        self.counters = Counter()
        self.active = self.peak_active = 0
        self.stop = threading.Event()
        self.thread = threading.Thread(
            target=lambda: self.serve_forever(poll_interval=.05), daemon=True)

    def handle_error(self, request, client_address):
        with self.lock:
            self.counters["fixture_errors"] += 1

    def register(self, fixture, stream):
        with self.lock:
            self.sequence += 1
            key = str(self.sequence)
            node = {"fixture": fixture, "stream": stream, "done": threading.Event(),
                    "started": None, "terminal_started": None, "closed": None,
                    "outcome": None, "events": 0, "bytes": 0,
                    "max_schedule_lateness_ms": 0}
            self.nodes[key] = node
        return key, node

    def snapshot(self, reset_peak=False):
        with self.lock:
            result = {"counts": dict(self.counters), "active": self.active,
                      "peak_active": self.peak_active}
            if reset_peak:
                self.peak_active = self.active
            return result


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def do_POST(self):
        self.connection.settimeout(IO_TIMEOUT)
        self.connection.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        self.close_connection = True
        node = None
        provider = self.server
        try:
            size = int(self.headers.get("Content-Length", "-1"))
            if not 0 < size <= 4096 or self.path != "/v1/chat/completions":
                raise ValueError("fixture_request_path_or_size")
            payload = json.loads(self.rfile.read(size))
            with provider.lock:
                node = provider.nodes.get(payload.get("benchmark_id"))
            if node is None or payload != {
                "model": "bench", "messages": [], "stream": node["stream"],
                "benchmark_id": payload["benchmark_id"],
            }:
                raise ValueError("fixture_request_body")
            with provider.lock:
                node["started"] = time.monotonic()
                provider.counters["started"] += 1
                provider.active += 1
                provider.peak_active = max(provider.peak_active, provider.active)
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream" if node["stream"]
                             else "application/json")
            self.send_header("Connection", "close")
            if not node["stream"]:
                self.send_header("Content-Length", str(len(CHAT)))
            self.end_headers()
            if not node["stream"]:
                self.wfile.write(CHAT)
                node["bytes"] = len(CHAT)
                with provider.lock:
                    provider.counters["body_bytes_written"] += len(CHAT)
            else:
                for index in range(node["fixture"].events + 1):
                    due = node["started"] + index * PERIOD
                    while True:
                        remaining = due - time.monotonic()
                        if provider.stop.is_set():
                            raise RuntimeError("harness_shutdown")
                        readable, _, _ = select.select(
                            [self.connection], [], [], max(0, min(.02, remaining)))
                        if readable:
                            if not self.connection.recv(1, socket.MSG_PEEK):
                                raise ConnectionAbortedError("peer_eof")
                            raise ValueError("unexpected_pipelined_bytes")
                        if remaining <= 0:
                            break
                    terminal = index == node["fixture"].events
                    node["max_schedule_lateness_ms"] = max(
                        node["max_schedule_lateness_ms"], (time.monotonic() - due) * 1000)
                    if terminal:
                        node["terminal_started"] = time.monotonic()
                    raw = FINISH + DONE if terminal else DELTA
                    self.wfile.write(raw)
                    self.wfile.flush()
                    node["events"] += 1
                    node["bytes"] += len(raw)
                    with provider.lock:
                        provider.counters["body_bytes_written"] += len(raw)
                        provider.counters["sse_frames_written"] += 2 if terminal else 1
            node["outcome"] = "completed"
        except (ConnectionError, OSError) as error:
            if node:
                node["outcome"] = "disconnect"
                node["disconnect_reason"] = type(error).__name__
        except (ValueError, RuntimeError) as error:
            if node:
                node["outcome"] = str(error)
            with provider.lock:
                provider.counters["fixture_errors"] += 1
        finally:
            if node and node["started"] is not None:
                with provider.lock:
                    node["closed"] = time.monotonic()
                    provider.counters[node["outcome"] or "unknown"] += 1
                    provider.active -= 1
                node["done"].set()


def disconnect(connection):
    if connection.sock:
        with contextlib.suppress(OSError):
            connection.sock.shutdown(socket.SHUT_RDWR)
    connection.close()


def exchange(provider, address, via_gateway, fixture, stream, hold=None):
    key, node = provider.register(fixture, stream)
    started = time.monotonic()
    connection = http.client.HTTPConnection(*address, timeout=IO_TIMEOUT)
    result = {"status": None, "outcome": "error", "first_event_ms": None,
              "bytes": 0, "events": 0}
    response = None
    try:
        connection.request("POST", "/v1/chat/completions", encoded({
            "model": "local/bench" if via_gateway else "bench", "messages": [],
            "stream": stream, "benchmark_id": key,
        }), {"Content-Type": "application/json", "Connection": "close"})
        response = connection.getresponse()
        result["status"] = response.status
        if response.status != 200:
            raw = response.read(BODY_LIMIT + 1)
            if len(raw) > BODY_LIMIT:
                raise ValueError("error_body_limit")
            if via_gateway and response.status == 503 and json.loads(raw).get(
                    "error", {}).get("code") == "gateway_concurrency_limit":
                result["outcome"] = "rejected"
            else:
                raise ValueError("unexpected_http_status")
        else:
            content_type = response.getheader("Content-Type", "").split(";")[0].strip()
            if content_type != ("text/event-stream" if stream else "application/json"):
                raise ValueError("content_type")
            if via_gateway and (
                response.getheader("X-Magpie-Provider") != "local"
                or response.getheader("X-Magpie-Model") != "local/bench"
            ):
                raise ValueError("response_attribution")
            if not stream:
                raw = response.read(BODY_LIMIT + 1)
                result["bytes"] = len(raw)
                result["sha256"] = sha256(raw)
                if raw != CHAT or result["sha256"] != CHAT_HASH:
                    raise ValueError("nonstream_bytes_or_hash")
            else:
                pending = bytearray()
                digest = hashlib.sha256()
                frame = 0
                deadline = started + fixture.seconds + IO_TIMEOUT
                while True:
                    if time.monotonic() > deadline:
                        raise TimeoutError("stream_total_deadline")
                    raw = response.read1(32768)
                    if not raw:
                        break
                    result["bytes"] += len(raw)
                    if result["bytes"] > fixture.stream_bytes:
                        raise ValueError("stream_body_limit")
                    digest.update(raw)
                    pending.extend(raw)
                    while b"\n\n" in pending:
                        end = pending.index(b"\n\n") + 2
                        event = bytes(pending[:end])
                        del pending[:end]
                        expected = (DELTA if frame < fixture.events else
                                    FINISH if frame == fixture.events else DONE)
                        if event != expected or frame > fixture.events + 1:
                            raise ValueError("sse_event_bytes_or_order")
                        if frame == 0:
                            first_at = time.monotonic()
                            result["first_event_ms"] = (first_at - started) * 1000
                            if node["started"] is None or node["terminal_started"] is not None:
                                raise ValueError("first_event_not_before_provider_terminal")
                            if hold is not None:
                                hold["ready"].set()
                                if not hold["release"].wait(IO_TIMEOUT):
                                    raise TimeoutError("hold_release_deadline")
                                result["client_cancel_at"] = time.monotonic()
                                disconnect(connection)
                                response.close()
                                if not node["done"].wait(IO_TIMEOUT):
                                    raise TimeoutError("provider_cancel_not_observed")
                                result["provider_cancel_ms"] = (
                                    node["closed"] - result["client_cancel_at"]) * 1000
                                result["provider_disconnect_reason"] = node.get("disconnect_reason")
                                if (node["outcome"] != "disconnect" or node["terminal_started"]
                                        or result["provider_cancel_ms"] < 0):
                                    raise ValueError("provider_not_actually_cancelled")
                                result["outcome"] = "cancelled"
                                return result
                        frame += 1
                    if len(pending) > EVENT_BYTES:
                        raise ValueError("sse_event_buffer_limit")
                result["events"] = frame
                result["sha256"] = digest.hexdigest()
                if (pending or frame != fixture.events + 2
                        or result["bytes"] != fixture.stream_bytes
                        or result["sha256"] != fixture.stream_hash):
                    raise ValueError("stream_hash_or_terminal")
                if not result["first_event_ms"] < (time.monotonic() - started) * 1000:
                    raise ValueError("first_event_not_before_completion")
            if node["started"] is None:
                raise ValueError("accepted_response_without_provider_call")
            result["outcome"] = "success"
    except (OSError, ValueError, RuntimeError, http.client.HTTPException) as error:
        result["outcome"] = "error"
        result["error"] = type(error).__name__ + ": " + str(error)[:160]
    finally:
        result["total_ms"] = (time.monotonic() - started) * 1000
        disconnect(connection)
        if response:
            response.close()
        if node["started"] is not None and not node["done"].wait(IO_TIMEOUT):
            result["outcome"] = "error"
            result["error"] = "provider_handler_did_not_release"
        result["provider_started"] = node["started"] is not None
        result["provider_max_schedule_lateness_ms"] = node["max_schedule_lateness_ms"]
        result["provider_outcome"] = node["outcome"]
        if result["outcome"] == "success" and node["outcome"] != "completed":
            result["outcome"] = "error"
            result["error"] = "provider_did_not_complete"
        with provider.lock:
            provider.nodes.pop(key, None)
    return result


def run_case(provider, address, via_gateway, fixture, stream, concurrency, seconds,
             single_round=False):
    before = provider.snapshot(reset_peak=True)
    host_before = host_conditions()
    counts, statuses, errors = Counter(), Counter(), Counter()
    first, totals, rejects, lateness, error_examples = [], [], [], [], []
    good_bytes = offered = 0
    started = time.monotonic()
    cpu_started = time.process_time()
    launch_deadline = started + seconds
    launch_ended = started
    with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as pool:
        while ((single_round and offered == 0) or time.monotonic() < launch_deadline
               ) and offered + concurrency <= MAX_REQUESTS:
            round_start = time.monotonic()
            # A barrier gives each round the declared offered concurrency.
            barrier = threading.Barrier(concurrency)
            def attempt():
                try:
                    barrier.wait(timeout=IO_TIMEOUT)
                    return exchange(provider, address, via_gateway, fixture, stream)
                except Exception as error:
                    return {"outcome": "error", "status": None, "provider_started": False,
                            "error": type(error).__name__ + ": " + str(error)[:160]}
            futures = [pool.submit(attempt) for _ in range(concurrency)]
            offered += concurrency
            launch_ended = time.monotonic()
            for future in futures:
                item = future.result(timeout=fixture.seconds + 3 * IO_TIMEOUT)
                counts[item["outcome"]] += 1
                statuses[str(item["status"])] += 1
                counts["provider_started"] += item["provider_started"]
                if item["outcome"] == "success":
                    lateness.append(item["provider_max_schedule_lateness_ms"])
                    good_bytes += item["bytes"]
                    totals.append(item["total_ms"])
                    if item["first_event_ms"] is not None:
                        first.append(item["first_event_ms"])
                elif item["outcome"] == "rejected":
                    rejects.append(item["total_ms"])
                else:
                    errors[item.get("error", "unknown")] += 1
                    if len(error_examples) < 20:
                        error_examples.append(item)
            if single_round:
                break
            threading.Event().wait(max(
                0, min(ROUND_INTERVAL - (time.monotonic() - round_start),
                       launch_deadline - time.monotonic())))
    elapsed = time.monotonic() - started
    cpu_seconds = time.process_time() - cpu_started
    after = provider.snapshot()
    return {
        "path": "gateway" if via_gateway else "direct", "stream": stream,
        "offered_concurrency": concurrency, "gateway_active_cap": ACTIVE_CAP if via_gateway else None,
        "requested_launch_seconds": seconds, "elapsed_with_drain_seconds": elapsed,
        "single_round": single_round,
        "last_launch_seconds": launch_ended - started, "offered": offered,
        "counts": {key: counts[key] for key in
                   ("success", "rejected", "error", "cancelled", "provider_started")},
        "http_status_counts": dict(statuses), "error_counts": dict(errors),
        "error_examples": error_examples, "first_event_ms": summary(first),
        "total_ms": summary(totals), "rejected_total_ms": summary(rejects),
        "provider_max_schedule_lateness_ms": summary(lateness) if stream else None,
        "valid_requests_per_second": counts["success"] / elapsed,
        "valid_payload_bytes_per_second": good_bytes / elapsed,
        "valid_bytes": good_bytes, "request_cap_reached": offered + concurrency > MAX_REQUESTS,
        "harness_cpu_seconds": cpu_seconds, "harness_cpu_fraction_of_one_core": cpu_seconds / elapsed,
        "provider": {"delta_counts": dict(Counter(after["counts"]) - Counter(before["counts"])),
                     "active_after": after["active"], "peak_active": after["peak_active"]},
        "host_before": host_before, "host_after": host_conditions(),
    }


def cancellation_probe(provider, address, via_gateway, fixture):
    count = ACTIVE_CAP if via_gateway else 1
    holds = [{"ready": threading.Event(), "release": threading.Event()} for _ in range(count)]
    with concurrent.futures.ThreadPoolExecutor(max_workers=count) as pool:
        futures = [pool.submit(exchange, provider, address, via_gateway, fixture, True, hold)
                   for hold in holds]
        try:
            if not all(hold["ready"].wait(IO_TIMEOUT) for hold in holds):
                raise RuntimeError("cancel_probe_streams_not_ready")
            admission = None
            if via_gateway:
                admission = run_case(provider, address, True, fixture, False, 12, 0, True)
        finally:
            for hold in holds:
                hold["release"].set()
        cancelled = [future.result(timeout=3 * IO_TIMEOUT) for future in futures]
    return {"path": "gateway" if via_gateway else "direct", "cancelled": cancelled,
            "admission_while_four_streams_held": admission}


def parse_options():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--duration-seconds", type=float, default=300,
                        help="launch window per matrix cell; default 300 (12 cells)")
    parser.add_argument("--stream-seconds", type=float, default=5)
    parser.add_argument("--long-stream-seconds", type=float,
                        help="single four-stream round per path; default max(60, 4*stream), capped at 100")
    parser.add_argument("--sample-interval", type=float, default=.1)
    parser.add_argument("--recovery-seconds", type=float, default=1)
    options = parser.parse_args()
    if not sys.platform.startswith("linux"):
        parser.error("Linux /proc/smaps_rollup is required")
    for field in ("binary", "output"):
        if not getattr(options, field).is_absolute():
            parser.error(f"--{field} must be absolute")
    if not options.binary.is_file() or not os.access(options.binary, os.X_OK):
        parser.error("--binary must name an executable file")
    options.binary = options.binary.resolve()
    supplied_output = options.output
    options.output = options.output.resolve()
    for candidate in (supplied_output, options.output):
        for parent in (candidate, *candidate.parents):
            if (parent / ".git").exists() or parent == ROOT:
                parser.error("--output must be outside every repository")
    if options.output.exists() or supplied_output.is_symlink():
        parser.error("--output must be a new exclusive path")
    for name, lower, upper in (
        ("duration_seconds", .2, 600), ("stream_seconds", .2, 40),
        ("sample_interval", .02, 1), ("recovery_seconds", .2, 10),
    ):
        value = getattr(options, name)
        if not math.isfinite(value) or not lower <= value <= upper:
            parser.error(f"--{name.replace('_', '-')} must be in [{lower}, {upper}]")
    if options.long_stream_seconds is None:
        options.long_stream_seconds = min(100, max(60, 4 * options.stream_seconds))
    if not (math.isfinite(options.long_stream_seconds)
            and 2 * options.stream_seconds <= options.long_stream_seconds <= 100):
        parser.error("--long-stream-seconds must be >= 2*stream-seconds and <= 100")
    return options


def main():
    options = parse_options()
    fixture, long_fixture = Fixture(options.stream_seconds), Fixture(options.long_stream_seconds)
    result = {
        "schema_version": 1, "method": "loopback-chat-forwarding-v1",
        "timestamp_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "invocation": [sys.executable, *sys.argv],
        "source": source_identity(), "runner_sha256": sha256(Path(__file__).read_bytes()),
        "host": {"platform": platform.platform(), "architecture": platform.machine(),
                 "logical_cpus": os.cpu_count(), "cpuinfo": text("/proc/cpuinfo"),
                 "os_release": text("/etc/os-release"), "before": host_conditions()},
        "toolchain": {"python": sys.version, "rustc": command(["rustc", "--version"]),
                      "cargo": command(["cargo", "--version"])},
        "workload": {"fixture": fixture.describe(), "long_fixture": long_fixture.describe(),
                     "duration_seconds_per_matrix_cell": options.duration_seconds,
                     "round_interval_seconds": ROUND_INTERVAL, "max_requests_per_cell": MAX_REQUESTS,
                     "sample_interval_seconds": options.sample_interval,
                     "io_timeout_seconds": IO_TIMEOUT, "active_cap_required": ACTIVE_CAP,
                     "warmup": "one fully validated request per path and payload kind",
                     "compile_profile_allocator_linkage": "not attested for supplied prebuilt binary"},
        "cases": [], "cancellation": [], "failures": [],
        "limitations": [
            "Baseline only; no original Magpie A/B, absolute performance budget or pass claim.",
            "One sequential matrix, not five interleaved independent A/B repetitions; page cache retained.",
            "Both clients and threaded provider share a Python process/GIL and host with the sampler.",
            "Paced barrier rounds (>=50ms apart) cap offered rate; throughput is not maximum capacity.",
            "Direct concurrency 16 permits 16 provider streams; gateway accepts at most four; rejections differ.",
            "Python fixture CPU, scheduling, loopback, body validation and connection setup affect latency.",
            "Total latency starts before connection setup and ends after EOF plus byte/hash validation.",
            "First event means one complete validated 16KiB SSE frame, not headers or first TCP byte.",
            "No inference, TLS, remote vendor, translated protocol, Usage, GUI or non-Linux acceptance.",
            "RSS/PSS are sampled, may miss brief peaks; swap is separate and not a low-memory-device test.",
            "PSS+SwapPss includes attributed swapped pages; neither it nor RSS+Swap equals allocations.",
            "Child checks sample every target task's children; short-lived/detached descendants can be missed.",
            "Memory-window trends are observations; no calibrated leak/recovery tolerance or profiler limiter.",
        ],
    }
    options.output.parent.mkdir(parents=True, exist_ok=True)
    # Reserve the result before starting, so failed runs also retain bounded evidence.
    with options.output.open("x") as output:
        gateway = sampler = provider = None
        try:
            with tempfile.TemporaryDirectory(prefix="magpie-forwarding-", dir="/tmp") as temporary:
                temp = Path(temporary)
                binary = temp / "magpie"
                shutil.copyfile(options.binary, binary)
                binary.chmod(0o700)
                for name in ("home", "config", "codex", "xdg", "bin"):
                    (temp / name).mkdir()
                env = {"HOME": str(temp / "home"), "CODEX_HOME": str(temp / "codex"),
                       "MAGPIE_CONFIG_DIR": str(temp / "config"),
                       "XDG_CONFIG_HOME": str(temp / "xdg"), "PATH": str(temp / "bin"),
                       "TMPDIR": str(temp), "LC_ALL": "C"}
                result["binary"] = {"supplied_path": str(options.binary),
                                    "sha256": sha256(binary.read_bytes()),
                                    "size_bytes": binary.stat().st_size,
                                    "version": command([str(binary), "--version"], env)}
                provider = Provider()
                provider.thread.start()
                direct = ("127.0.0.1", provider.server_port)
                config = temp / "forward.json"
                config_bytes = encoded({"version": 1, "provider_id": "local",
                                        "base_url": f"http://127.0.0.1:{direct[1]}/v1",
                                        "models": ["bench"]})
                config.write_bytes(config_bytes)
                result["workload"]["config_sha256"] = sha256(config_bytes)
                result["workload"]["config_template_sha256"] = sha256(encoded({
                    "version": 1, "provider_id": "local",
                    "base_url": "http://127.0.0.1:PORT/v1", "models": ["bench"]}))
                gateway = Gateway(binary, config, env)
                result["startup"] = gateway.start()
                sampler = Sampler(gateway.process.pid, options.sample_interval, provider)
                sampler.thread.start()
                result["warmup"] = [
                    exchange(provider, gateway.address if via else direct, via, fixture, stream)
                    for via in (False, True) for stream in (False, True)]
                if any(item["outcome"] != "success" for item in result["warmup"]):
                    raise RuntimeError("warmup_content_or_forwarding_failed")
                result["idle_before"] = sampler.window("idle_before", options.recovery_seconds)
                for index, (stream, concurrency) in enumerate(
                        (s, c) for s in (False, True) for c in (1, 4, 16)):
                    for via in ((False, True) if index % 2 == 0 else (True, False)):
                        phase = f"{'gateway' if via else 'direct'}-{'sse' if stream else 'json'}-{concurrency}"
                        sampler.phase = phase
                        case = run_case(provider, gateway.address if via else direct, via,
                                        fixture, stream, concurrency, options.duration_seconds)
                        case["phase"] = phase
                        case["memory"] = memory_summary(
                            [s for s in sampler.samples if s["phase"] == phase])
                        case["recovery"] = sampler.window(phase + "-recovery", options.recovery_seconds)
                        case["health_after_recovery"] = get_json(gateway.address, "/health")
                        result["cases"].append(case)
                        print(f"{phase}: success={case['counts']['success']} "
                              f"reject={case['counts']['rejected']} error={case['counts']['error']}",
                              file=sys.stderr, flush=True)
                for via in (False, True):
                    phase = "cancel-gateway" if via else "cancel-direct"
                    sampler.phase = phase
                    # Long enough that the provider cannot finish before the explicit close.
                    probe = cancellation_probe(provider, gateway.address if via else direct,
                                               via, long_fixture)
                    probe["recovery"] = sampler.window(phase + "-recovery", options.recovery_seconds)
                    probe["health_after_recovery"] = get_json(gateway.address, "/health")
                    result["cancellation"].append(probe)
                result["long_stream"] = []
                for via in (False, True):
                    phase = "long-gateway" if via else "long-direct"
                    sampler.phase = phase
                    case = run_case(provider, gateway.address if via else direct, via,
                                    long_fixture, True, 4, 0, True)
                    samples = [s for s in sampler.samples if s["phase"] == phase]
                    case["phase"] = phase
                    case["memory"] = memory_summary(samples)
                    case["memory_windows"] = []
                    if samples:
                        start, end = samples[0]["t"], samples[-1]["t"]
                        for part in range(4):
                            window = [s for s in samples if
                                      part / 4 <= (s["t"] - start) / max(.001, end - start)
                                      <= (part + 1) / 4]
                            case["memory_windows"].append({
                                "quarter": part + 1, "memory": memory_summary(window),
                                "start_t": window[0]["t"] if window else None,
                                "end_t": window[-1]["t"] if window else None,
                                "provider_bytes_written_start": window[0]["provider"]["counts"].get(
                                    "body_bytes_written", 0) if window else None,
                                "provider_bytes_written_end": window[-1]["provider"]["counts"].get(
                                    "body_bytes_written", 0) if window else None})
                    case["recovery"] = sampler.window(phase + "-recovery", options.recovery_seconds)
                    case["health_after_recovery"] = get_json(gateway.address, "/health")
                    result["long_stream"].append(case)
                result["provider"] = provider.snapshot()
        except (Exception, KeyboardInterrupt) as error:
            result["failures"].append(type(error).__name__ + ": " + str(error)[:240])
        finally:
            if sampler:
                sampler.close()
                result["resource_samples"] = sampler.samples
                result["sampling_errors"] = sampler.errors
                times = [s["t"] for s in sampler.samples]
                result["actual_sample_gap_seconds"] = summary(
                    [b - a for a, b in zip(times, times[1:])])
                children = sorted({pid for s in sampler.samples for pid in s["target"]["children"]})
                result["sampled_target_children"] = children
                if children or sampler.errors or not sampler.samples:
                    result["failures"].append("resource_sampling_incomplete_or_unexpected_children")
            if gateway:
                result["gateway_cleanup"] = gateway.close()
                if result["gateway_cleanup"]["returncode"] is None:
                    result["failures"].append("gateway_not_reaped")
            if provider:
                provider.stop.set()
                provider.shutdown()
                provider.server_close()
                provider.thread.join(timeout=IO_TIMEOUT)
                result["provider_after_cleanup"] = provider.snapshot()
                if provider.active:
                    result["failures"].append("provider_handlers_still_active")
        baseline = result.get("idle_before", {})
        if not baseline.get("samples"):
            result["failures"].append("initial_idle_memory_not_sampled")
        for entry in result["cases"] + result.get("long_stream", []) + result["cancellation"]:
            entry["recovery_delta_from_initial_idle"] = {
                key: entry["recovery"][key]["p50"] - baseline[key]["p50"]
                for key in ("rss_kib", "pss_kib", "swap_kib", "swap_pss_kib", "threads", "fds")
                if key in baseline and key in entry["recovery"]
            }
        for case in result["cases"] + result.get("long_stream", []):
            if (case["counts"]["error"] or not case["counts"]["success"]
                    or case["request_cap_reached"] or case["provider"]["active_after"]
                    or not case["memory"]["samples"] or not case["recovery"]["samples"]
                    or (case["path"] == "direct" and case["counts"]["rejected"])
                    or (case["path"] == "gateway" and case["provider"]["peak_active"] > ACTIVE_CAP)):
                result["failures"].append("invalid_case: " + case["phase"])
            if "memory_windows" in case and (
                len(case["memory_windows"]) != 4
                or any(not window["memory"]["samples"] for window in case["memory_windows"])
            ):
                result["failures"].append("long_memory_window_incomplete: " + case["phase"])
            health = case.get("health_after_recovery", {})
            if health and health["transport_counters"]["active"]:
                result["failures"].append("gateway_not_released: " + case["phase"])
        for probe in result["cancellation"]:
            if any(item["outcome"] != "cancelled" for item in probe["cancelled"]):
                result["failures"].append("provider_cancellation_not_confirmed: " + probe["path"])
            admission = probe["admission_while_four_streams_held"]
            if admission and (admission["offered"] != 12
                              or admission["counts"]["rejected"] != 12
                              or admission["counts"]["provider_started"] != 0):
                result["failures"].append("four_active_cap_not_confirmed")
            if probe["health_after_recovery"]["transport_counters"]["active"]:
                result["failures"].append("gateway_cancel_did_not_release")
            if not probe["recovery"]["samples"]:
                result["failures"].append("cancellation_recovery_memory_not_sampled")
        aggregate = Counter()
        for case in result["cases"] + result.get("long_stream", []):
            aggregate.update(case["counts"])
        for item in result.get("warmup", []):
            aggregate[item["outcome"]] += 1
            aggregate["provider_started"] += item["provider_started"]
        for probe in result["cancellation"]:
            for item in probe["cancelled"]:
                aggregate[item["outcome"]] += 1
                aggregate["provider_started"] += item["provider_started"]
            if probe["admission_while_four_streams_held"]:
                aggregate.update(probe["admission_while_four_streams_held"]["counts"])
        result["all_post_request_counts"] = dict(aggregate)
        result["control_get_requests_excluded_from_post_counts"] = True
        provider_counts = result.get("provider", {}).get("counts", {})
        if provider_counts and provider_counts.get("started", 0) != aggregate["provider_started"]:
            result["failures"].append("provider_start_count_does_not_match_clients")
        if result.get("provider", {}).get("counts", {}).get("fixture_errors"):
            result["failures"].append("provider_fixture_error")
        result["host"]["after"] = host_conditions()
        result["correctness_valid"] = not result["failures"]
        result["performance_verdict"] = "baseline only; no performance pass or A/B claim"
        json.dump(result, output, indent=2, allow_nan=False)
        output.write("\n")
    print(json.dumps({"output": str(options.output), "binary": result.get("binary"),
                      "correctness_valid": result["correctness_valid"],
                      "failures": result["failures"]}, indent=2))
    return 0 if result["correctness_valid"] else 1


if __name__ == "__main__":
    sys.exit(main())
