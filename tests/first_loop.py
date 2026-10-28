#!/usr/bin/env python3
"""Real local TCP + synthetic config acceptance. Never opens a real Agent home.

Run: python3 tests/first_loop.py --binary target/debug/magpie --report report.json
This validates a candidate, not real Codex/vendor or native GPUI compatibility.
"""
import argparse
import hashlib
import http.client
import http.server
import json
import os
from pathlib import Path
import select
import selectors
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import unittest

SECRET = "fixture-provider-secret-NOT-A-REAL-KEY"
STARTUP_TIMEOUT = 5
DIAGNOSTIC_LIMIT = 4096
CHAT = {"id": "chat-fixture", "object": "chat.completion", "model": "fixture-a",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "真实本地转发"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7}}
RESPONSES = {"id": "resp-fixture", "object": "response", "status": "completed", "error": None,
             "output": [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "真实本地转发"}]}],
             "usage": {"input_tokens": 3, "output_tokens": 4, "total_tokens": 7}}
def encoded(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()
CHAT_DELTA = b"data: " + encoded({"id": "chat-fixture", "object": "chat.completion.chunk", "choices": [
    {"index": 0, "delta": {"content": "你好", "tool_calls": [{"index": 0, "function": {"arguments": '{"x":"你'}}]}}]}) + b"\r\n\r\n"
CHAT_END = b"data: " + encoded({"id": "chat-fixture", "object": "chat.completion.chunk", "choices": [
    {"index": 0, "delta": {"tool_calls": [{"index": 0, "function": {"arguments": '好"}'}}]}, "finish_reason": "tool_calls"}]}) + b"\r\n\r\ndata: [DONE]\r\n\r\n"
RESPONSE_DELTA = b"event: response.output_text.delta\ndata: " + encoded({"type": "response.output_text.delta", "delta": "你好"}) + b"\n\n"
RESPONSE_END = b"event: response.completed\ndata: " + encoded({"type": "response.completed", "response": RESPONSES}) + b"\n\n"

def framed_sse(raw, endings, bom=False):
    lines = raw.replace(b"\r\n", b"\n").split(b"\n")
    assert lines[-1] == b""
    return (b"\xef\xbb\xbf" if bom else b"") + b"".join(
        line + endings[index % len(endings)] for index, line in enumerate(lines[:-1]))

ERROR_DATA = b"data: " + encoded({"error": {"message": SECRET}})
ERROR_MESSAGE = b"data: " + encoded({"message": SECRET})
FRAMED_ERRORS = {
    "sse-error-cr": b": ping\r" + ERROR_DATA + b"\r\r\n\n",
    "sse-error-mixed": b": ping\r" + ERROR_DATA + b"\n\r\n\n",
    "sse-error-bom": b"\xef\xbb\xbf" + ERROR_DATA + b"\n\n",
    "sse-event-error-cr": b": ping\revent: error\r" + ERROR_MESSAGE + b"\r\r\n\n",
    "sse-event-error-mixed": b": ping\revent: error\r\n" + ERROR_MESSAGE + b"\n\r\n\n",
    "sse-event-error-bom": b"\xef\xbb\xbfevent: error\n" + ERROR_MESSAGE + b"\n\n",
}
SUCCESS_FRAMING = {
    "framing-cr": ((b"\r",), False),
    "framing-mixed": ((b"\r", b"\r\n", b"\n"), False),
    "framing-bom": ((b"\n",), True),
    "framing-bom-cr": ((b"\r",), True),
    "framing-bom-crlf": ((b"\r\n",), True),
}

class CandidateProcess:
    """Deadline-bound startup with capped diagnostics; never waits for pipe EOF."""

    def __init__(self, command, env):
        self.process = subprocess.Popen(command, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, env=env, bufsize=0)
        self.selector = selectors.DefaultSelector()
        self.output = {"stdout": bytearray(), "stderr": bytearray()}
        self.scan_tail = {"stdout": b"", "stderr": b""}
        self.secret_leaked = False
        self.closed = False
        try:
            for name, pipe in (("stdout", self.process.stdout), ("stderr", self.process.stderr)):
                os.set_blocking(pipe.fileno(), False)
                self.selector.register(pipe, selectors.EVENT_READ, name)
        except BaseException:
            self.close()
            raise

    def _pump(self, timeout):
        events = self.selector.select(timeout)
        for key, _ in events:
            try:
                chunk = os.read(key.fileobj.fileno(), 4096)
            except BlockingIOError:
                continue
            if not chunk:
                self.selector.unregister(key.fileobj)
                continue
            name = key.data
            scan = self.scan_tail[name] + chunk
            self.secret_leaked |= SECRET.encode() in scan
            self.scan_tail[name] = scan[-(len(SECRET) - 1):]
            available = DIAGNOSTIC_LIMIT - len(self.output[name])
            self.output[name].extend(chunk[:available])
        return bool(events)

    def diagnostics(self):
        return "; ".join(f"{name}={bytes(raw).decode('utf-8', 'replace')!r}"
            for name, raw in self.output.items()).replace(SECRET, "<redacted-fixture-secret>")

    def wait_for_bind(self, timeout=STARTUP_TIMEOUT):
        deadline = time.monotonic() + timeout
        try:
            while True:
                raw = self.output["stdout"]
                if b"\n" in raw:
                    line = bytes(raw).split(b"\n", 1)[0].decode("utf-8", "replace")
                    if "bound at " not in line:
                        raise AssertionError("candidate_unexpected_startup_line")
                    try:
                        address = line.split("bound at ", 1)[1].split()[0]
                        host, port = address.rsplit(":", 1)
                        if not host or not 0 < int(port) <= 65535:
                            raise ValueError()
                    except (IndexError, ValueError):
                        raise AssertionError("candidate_invalid_bind_line") from None
                    return address
                if len(raw) == DIAGNOSTIC_LIMIT:
                    raise AssertionError("candidate_startup_line_too_long")
                if self.process.stdout not in [key.fileobj for key in self.selector.get_map().values()]:
                    raise AssertionError("candidate_exited_before_bind")
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise AssertionError("candidate_startup_timeout")
                self._pump(remaining)
        except BaseException as error:
            self.close()
            if isinstance(error, AssertionError):
                raise AssertionError(f"{error}; {self.diagnostics()}") from None
            raise

    def close(self):
        if self.closed:
            return
        try:
            if self.process.poll() is None:
                self.process.terminate()
                try:
                    self.process.wait(timeout=.5)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait(timeout=1.5)
            deadline = time.monotonic() + .2
            while self.selector.get_map() and time.monotonic() < deadline:
                if not self._pump(0):
                    break
        finally:
            self.closed = True
            self.selector.close()
            self.process.stdout.close()
            self.process.stderr.close()

class StartupTests(unittest.TestCase):
    def test_hanging_binary_has_deadline_and_is_reaped(self):
        self.assert_startup_failure(
            "import time; time.sleep(60)", "candidate_startup_timeout")

    def test_partial_line_and_full_stderr_do_not_block(self):
        self.assert_startup_failure(
            "import os,time; os.write(1,b'partial'); "
            "os.write(2,b'x'*262144); time.sleep(60)", "candidate_startup_timeout")

    def test_unexpected_line_does_not_wait_for_stderr_eof(self):
        self.assert_startup_failure(
            "import os,signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); "
            "os.write(1,b'unexpected startup\\n'); time.sleep(60)",
            "candidate_unexpected_startup_line")

    def test_setup_failure_runs_child_cleanup(self):
        with tempfile.TemporaryDirectory(prefix="magpie-startup-cleanup-") as root:
            candidate = CandidateProcess([sys.executable, "-I", "-u", "-c",
                "import os,time; "
                "os.write(1,b'Forward candidate bound at 127.0.0.1:1\\n'); time.sleep(60)"],
                {"HOME": root, "CODEX_HOME": root, "XDG_CONFIG_HOME": root})
            self.addCleanup(candidate.close)

            class FailedSetup(unittest.TestCase):
                @classmethod
                def setUpClass(cls):
                    cls.addClassCleanup(candidate.close)
                    candidate.wait_for_bind(timeout=1)
                    raise AssertionError("fixture_readiness_failed")

                def test_unused(self):
                    self.fail("setup must fail first")

            result = unittest.TestResult()
            unittest.defaultTestLoader.loadTestsFromTestCase(FailedSetup).run(result)
            self.assertEqual(len(result.errors), 1)
            self.assertIn("fixture_readiness_failed", result.errors[0][1])
            self.assertIsNotNone(candidate.process.returncode)
            self.assertTrue(candidate.closed)

    def assert_startup_failure(self, script, reason):
        with tempfile.TemporaryDirectory(prefix="magpie-startup-") as root:
            candidate = CandidateProcess([sys.executable, "-I", "-u", "-c", script],
                {"HOME": root, "CODEX_HOME": root, "XDG_CONFIG_HOME": root})
            self.addCleanup(candidate.close)
            start = time.monotonic()
            with self.assertRaisesRegex(AssertionError, reason) as caught:
                candidate.wait_for_bind(timeout=.3)
            self.assertLess(time.monotonic() - start, 3)
            self.assertIsNotNone(candidate.process.returncode)
            self.assertTrue(candidate.process.stdout.closed)
            self.assertTrue(candidate.process.stderr.closed)
            self.assertLess(len(str(caught.exception)), 4 * DIAGNOSTIC_LIMIT)
            self.assertTrue(all(len(raw) <= DIAGNOSTIC_LIMIT for raw in candidate.output.values()))

class Provider(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    records = []
    lock = threading.Lock()
    release = threading.Event()
    closed = threading.Event()
    live = 0

    def log_message(self, *_):
        pass

    def do_POST(self):
        payload = self.rfile.read(int(self.headers["Content-Length"]))
        request = json.loads(payload)
        with self.lock:
            self.records.append({"path": self.path, "body": request, "headers": dict(self.headers)})
        model = request["model"]
        stream = request.get("stream", False)
        if model == "redirect":
            self.send_response(307)
            self.send_header("Location", "http://127.0.0.1:1/credential-exfiltration-refused")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if model == "http-error":
            raw = encoded({"error": {"message": SECRET, "prompt": "synthetic-private-prompt"}})
            self.send_response(429)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            self.send_header("Set-Cookie", SECRET)
            self.end_headers()
            self.wfile.write(raw)
            return
        if not stream:
            result = {} if model == "empty-json" else (RESPONSES if self.path == "/v1/responses" else CHAT)
            if model == "incomplete":
                result = {**RESPONSES, "status": "incomplete", "incomplete_details": {"reason": "max_output_tokens"}}
            if model == "bad-finish":
                result = {**CHAT, "choices": [{"index": 0, "message": {"content": "x"}, "finish_reason": 17}]}
            raw = encoded(result)
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream; charset=utf-8")
        self.send_header("Connection", "close")
        fragmented = model in FRAMED_ERRORS or model in SUCCESS_FRAMING
        if fragmented:
            self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()
        self.close_connection = True
        delta, terminal = (RESPONSE_DELTA, RESPONSE_END) if self.path == "/v1/responses" else (CHAT_DELTA, CHAT_END)
        if model == "cr-delayed":
            delta = framed_sse(delta, (b"\r",))
            terminal = framed_sse(terminal, (b"\r",))
        try:
            if fragmented:
                if model in FRAMED_ERRORS:
                    raw = FRAMED_ERRORS[model]
                else:
                    endings, bom = SUCCESS_FRAMING[model]
                    raw = framed_sse(delta + terminal, endings, bom)
                # One HTTP body chunk per byte also splits BOM, UTF-8 and CRLF.
                for byte in raw:
                    self.wfile.write(b"1\r\n" + bytes([byte]) + b"\r\n")
                    self.wfile.flush()
                self.wfile.write(b"0\r\n\r\n")
                self.wfile.flush()
                return
            if model == "bad-choices":
                self.wfile.write(b'data: {"choices":[7]}\n\ndata: [DONE]\n\n')
                self.wfile.flush()
                return
            if model in ("sse-error", "sse-event-error"):
                # Split an error across TCP writes; not even its prefix may leak.
                error = (b"event: error\ndata: " + encoded({"message": SECRET}) + b"\n\n") if model == "sse-event-error" else (b"data: " + encoded({"error": {"message": SECRET}}) + b"\n\n")
                for byte in error:
                    self.wfile.write(bytes([byte]))
                    self.wfile.flush()
                return
            if model == "large-event":
                result = dict(RESPONSES)
                result["fixture_padding"] = "x" * (96 * 1024)
                terminal = b"data: " + encoded({"type": "response.completed", "response": result}) + b"\n\n"
            if model == "incomplete":
                result = {**RESPONSES, "status": "incomplete", "incomplete_details": {"reason": "max_output_tokens"}}
                terminal = b"event: response.incomplete\ndata: " + encoded({"type": "response.incomplete", "response": result}) + b"\n\n"
            if model == "oversize-event":
                self.wfile.write(b"data: " + b"x" * (4 * 1024 * 1024 + 1))
                self.wfile.flush()
                return
            if model == "silent":
                self.closed.clear()
                for _ in range(300):
                    readable, _, _ = select.select([self.connection], [], [], .01)
                    if readable and not self.connection.recv(1, socket.MSG_PEEK):
                        self.closed.set()
                        return
                return
            if model == "hold":
                with self.lock:
                    Provider.live += 1
                try:
                    self.wfile.write(delta)
                    self.wfile.flush()
                    for _ in range(300):
                        if self.release.wait(.01):
                            break
                        self.wfile.write(b": heartbeat\n\n")
                        self.wfile.flush()
                finally:
                    with self.lock:
                        Provider.live -= 1
                return
            # Exercise UTF-8 and CRLF split across transport chunks.
            for byte in delta:
                self.wfile.write(bytes([byte]))
                self.wfile.flush()
            if model == "truncated":
                return
            if model in ("delayed", "cr-delayed"):
                self.release.wait(2)
            self.wfile.write(terminal)
            self.wfile.flush()
            if model == "terminal-hold":
                self.release.wait(2)
        except (ConnectionError, OSError):
            self.closed.set()

class FixtureTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(prefix="magpie-first-loop-")
        cls.addClassCleanup(cls.temp.cleanup)
        cls.root = Path(cls.temp.name)
        cls.provider = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
        cls.provider.daemon_threads = True
        cls.addClassCleanup(cls.provider.server_close)
        threading.Thread(target=cls.provider.serve_forever, daemon=True).start()
        cls.addClassCleanup(cls.provider.shutdown)
        cls.env = {
            "HOME": str(cls.root),
            "CODEX_HOME": str(cls.root / "codex"),
            "XDG_CONFIG_HOME": str(cls.root / "xdg"),
            "TMPDIR": str(cls.root),
            "LC_ALL": "C.UTF-8",
            "MAGPIE_FIXTURE_PROVIDER_KEY": SECRET,
        }
        (cls.root / "codex").mkdir(mode=0o700)
        (cls.root / "bin").mkdir(mode=0o700)
        agent = cls.root / "bin" / "codex"
        agent.write_text("#!/bin/sh\nexit 0\n")
        agent.chmod(0o700)
        cls.env["PATH"] = str(cls.root / "bin")
        cls.config = cls.root / "forward.json"
        cls.config.write_bytes(encoded({"version": 1, "provider_id": "local",
            "base_url": f"http://127.0.0.1:{cls.provider.server_port}/v1",
            "models": ["fixture-a", "http-error", "sse-error", "sse-event-error", "empty-json", "redirect", "truncated", "hold", "silent", "delayed", "terminal-hold", "large-event", "oversize-event", "incomplete", "bad-finish", "bad-choices", "cr-delayed",
                *FRAMED_ERRORS, *SUCCESS_FRAMING],
            "api_key_env": "MAGPIE_FIXTURE_PROVIDER_KEY"}))
        cls.candidate = CandidateProcess([str(BINARY), "serve", "--listen", "127.0.0.1:0",
            "--forward", str(cls.config)], cls.env)
        cls.addClassCleanup(cls.stop_candidate)
        deadline = time.monotonic() + STARTUP_TIMEOUT
        cls.address = cls.candidate.wait_for_bind()
        cls.host, port = cls.address.rsplit(":", 1)
        cls.port = int(port)
        while time.monotonic() < deadline:
            try:
                status, _, _ = cls.request("GET", "/health",
                    timeout=min(1, max(.01, deadline - time.monotonic())))
                if status == 200:
                    break
            except (ConnectionError, OSError, http.client.HTTPException):
                pass
            time.sleep(.02)
        else:
            raise AssertionError("candidate did not become ready; " + cls.candidate.diagnostics())

    @classmethod
    def stop_candidate(cls):
        Provider.release.set()
        cls.candidate.close()
        if cls.candidate.secret_leaked:
            raise AssertionError("synthetic credential leaked to server output")

    def setUp(self):
        Provider.release.clear()
        self.wait_active(0)

    @classmethod
    def request(cls, method, path, data=None, headers=None, timeout=4):
        connection = http.client.HTTPConnection(cls.host, cls.port, timeout=timeout)
        try:
            request_headers = {"Content-Type": "application/json"}
            request_headers.update(headers or {})
            connection.request(method, path, body=encoded(data) if data is not None else None, headers=request_headers)
            response = connection.getresponse()
            return response.status, dict(response.getheaders()), response.read()
        finally:
            connection.close()

    def wait_active(self, expected):
        for _ in range(150):
            _, _, body = self.request("GET", "/health")
            if json.loads(body)["transport_counters"]["active"] == expected:
                return
            time.sleep(.02)
        self.fail(f"active requests did not settle to {expected}")

    def cli(self, *args, ok=True):
        result = subprocess.run([str(BINARY), *args], env=self.env, capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode == 0, ok, result.stderr)
        self.assertNotIn(SECRET, result.stdout + result.stderr)
        return result

    def stream(self, model, path="/v1/chat/completions"):
        connection = http.client.HTTPConnection(self.host, self.port, timeout=4)
        connection.request("POST", path, encoded({"model": "local/" + model, "stream": True, "messages": []}), {"Content-Type": "application/json"})
        return connection, connection.getresponse()

    def test_01_catalog_readiness_is_not_provider_health(self):
        status, _, body = self.request("GET", "/v1/models")
        self.assertEqual(status, 200)
        item = json.loads(body)["data"][0]
        self.assertEqual(item["id"], "local/fixture-a")
        self.assertEqual(item["magpie"]["provider_health"], "not_probed")

    def test_02_chat_bytes_unknown_fields_tools_and_provider_auth(self):
        data = {"model": "local/fixture-a", "messages": [{"role": "user", "content": "合成输入"}],
                "tools": [{"type": "function", "function": {"name": "fixture", "parameters": {"enum": ["原样"]}}}],
                "extension": {"large_integer": 123456789012345678901234567890, "text": "a\\b\n你"}}
        status, headers, body = self.request("POST", "/v1/chat/completions", data, {"Authorization": "Bearer CALLER-NOT-PROVIDER", "Cookie": "PRIVATE-CALLER"})
        self.assertEqual((status, body), (200, encoded(CHAT)))
        self.assertEqual(headers["x-magpie-provider"], "local")
        self.assertEqual(headers["x-magpie-model"], "local/fixture-a")
        record = Provider.records[-1]
        self.assertEqual(record["body"], {**data, "model": "fixture-a"})
        lowered = {key.lower(): value for key, value in record["headers"].items()}
        self.assertEqual(lowered["authorization"], "Bearer " + SECRET)
        self.assertNotIn("cookie", lowered)

    def test_03_responses_json_accepts_error_null(self):
        status, _, body = self.request("POST", "/v1/responses", {"model": "local/fixture-a", "input": "hello"})
        self.assertEqual((status, body), (200, encoded(RESPONSES)))

    def test_04_sse_preserves_both_protocols_and_split_utf8(self):
        for path, expected in [("/v1/chat/completions", CHAT_DELTA + CHAT_END), ("/v1/responses", RESPONSE_DELTA + RESPONSE_END)]:
            connection, response = self.stream("fixture-a", path)
            self.assertEqual(response.status, 200)
            self.assertEqual(response.read(), expected)
            connection.close()

    def test_05_stream_delivers_before_provider_finishes(self):
        connection, response = self.stream("delayed")
        try:
            first = response.read1(4096)
            self.assertIn("你好".encode(), first)
            self.assertFalse(Provider.release.is_set())
            Provider.release.set()
            self.assertEqual(first + response.read(), CHAT_DELTA + CHAT_END)
        finally:
            connection.close()

    def test_06_error_bodies_and_sse_errors_never_leak(self):
        status, headers, body = self.request("POST", "/v1/responses", {"model": "local/http-error"})
        self.assertEqual(status, 429)
        self.assertNotIn(SECRET.encode(), body)
        self.assertNotIn("set-cookie", {key.lower() for key in headers})
        for model in ("sse-error", "sse-event-error"):
            connection, response = self.stream(model)
            try:
                with self.assertRaises(http.client.IncompleteRead) as caught:
                    response.read()
                self.assertNotIn(SECRET.encode(), caught.exception.partial)
                self.assertNotIn(b"message", caught.exception.partial)
            finally:
                connection.close()

    def test_07_truncated_sse_is_not_completed(self):
        _, _, before = self.request("GET", "/health")
        connection, response = self.stream("truncated")
        try:
            with self.assertRaises(http.client.IncompleteRead) as caught:
                response.read()
            self.assertNotIn(b"[DONE]", caught.exception.partial)
        finally:
            connection.close()
        self.wait_active(0)
        _, _, after = self.request("GET", "/health")
        self.assertEqual(json.loads(after)["transport_counters"]["completed"], json.loads(before)["transport_counters"]["completed"])

    def test_08_local_origin_host_proxy_and_account_guards(self):
        count = len(Provider.records)
        for headers, expected in [({"Origin": "null"}, 403), ({"Host": "evil.example"}, 403), ({"X-Forwarded-For": "127.0.0.1"}, 403), ({"X-Magpie-Account": "wrong"}, 501), ({"Upgrade": "websocket"}, 426)]:
            status, _, _ = self.request("POST", "/v1/responses", {"model": "local/fixture-a"}, headers)
            self.assertEqual(status, expected)
        self.assertEqual(len(Provider.records), count)

    def test_09_limits_model_endpoint_and_empty_response(self):
        for path, data, expected in [
            ("/v1/responses", {"model": "local/no-such-model"}, 404),
            ("/v1/messages", {"model": "local/fixture-a"}, 501),
            ("/v1/responses", {"model": "local/empty-json"}, 502),
            ("/v1/responses", {"model": "local/redirect"}, 502),
        ]:
            status, _, _ = self.request("POST", path, data)
            self.assertEqual(status, expected)
        # A declared oversize body is rejected before upload. Sending the whole
        # body after an early 413 can race TCP close and give the sender EPIPE.
        connection = http.client.HTTPConnection(self.host, self.port, timeout=4)
        try:
            connection.putrequest("POST", "/v1/responses")
            connection.putheader("Content-Type", "application/json")
            connection.putheader("Content-Length", str(1024 * 1024 + 1))
            connection.endheaders()
            response = connection.getresponse()
            self.assertEqual(response.status, 413)
            response.read()
        finally:
            connection.close()

    def test_10_four_streams_hold_permits_and_cancel_releases(self):
        connections = []
        try:
            for _ in range(4):
                connection, response = self.stream("hold")
                self.assertEqual(response.status, 200)
                self.assertTrue(response.read1(4096))
                connections.append((connection, response))
            self.wait_active(4)
            count = len(Provider.records)
            status, _, _ = self.request("POST", "/v1/responses", {"model": "local/fixture-a"})
            self.assertEqual(status, 503)
            self.assertEqual(len(Provider.records), count)
        finally:
            for connection, response in connections:
                response.close()
                connection.close()
            Provider.release.set()
        self.wait_active(0)
        status, _, _ = self.request("POST", "/v1/responses", {"model": "local/fixture-a"})
        self.assertEqual(status, 200)

    def test_11_cancel_silent_upstream_is_not_idle_timeout(self):
        connection = socket.create_connection((self.host, self.port), timeout=3)
        payload = encoded({"model": "local/silent", "stream": True})
        raw = (f"POST /v1/responses HTTP/1.1\r\nHost: {self.address}\r\nContent-Type: application/json\r\nContent-Length: {len(payload)}\r\n\r\n").encode() + payload
        connection.sendall(raw)
        self.wait_active(1)
        connection.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
        start = time.monotonic()
        connection.close()
        self.wait_active(0)
        self.assertLess(time.monotonic() - start, 3)
        self.assertTrue(Provider.closed.wait(3))

    def test_12_terminal_event_closes_without_waiting_for_upstream_eof(self):
        connection, response = self.stream("terminal-hold")
        start = time.monotonic()
        try:
            self.assertEqual(response.read(), CHAT_DELTA + CHAT_END)
            self.assertLess(time.monotonic() - start, 1.5)
        finally:
            Provider.release.set()
            connection.close()

    def test_13_large_responses_terminal_event_and_event_limit(self):
        connection, response = self.stream("large-event", "/v1/responses")
        self.assertGreater(len(response.read()), 96 * 1024)
        connection.close()
        connection, response = self.stream("oversize-event")
        try:
            with self.assertRaises(http.client.IncompleteRead):
                response.read()
        finally:
            connection.close()

    def test_14_transaction_cli_and_shared_read_state(self):
        path = self.root / "codex" / "config.toml"
        original = b'# user comment\nmodel = "before" # keep this\nmodel_reasoning_effort = "high"\n[custom]\nuntouched = 42\n'
        path.write_bytes(original)
        path.chmod(0o600)
        self.cli("agents", "connect", "codex", "--model", "local/fixture-a", "--config", str(path), "--gateway", self.address, "--dry-run", "--json")
        self.assertEqual(path.read_bytes(), original)
        self.assertEqual(sorted(p.name for p in path.parent.iterdir()), ["config.toml"])
        result = self.cli("agents", "connect", "codex", "--model", "local/fixture-a", "--config", str(path), "--gateway", self.address, "--apply", "--json")
        self.assertEqual(json.loads(result.stdout)["state"], "connected")
        selected = json.loads(self.cli("agents", "status", "codex", "--json").stdout)["agent"]
        self.assertEqual((selected["model"], selected["model_provider"], selected["reasoning_effort"]), ("local/fixture-a", "magpie", "high"))
        status = json.loads(self.cli("agents", "connection-status", "codex", "--config", str(path), "--json").stdout)
        self.assertEqual(status["state"], "connected")
        self.cli("agents", "disconnect", "codex", "--config", str(path), "--json")
        self.assertEqual(path.read_bytes(), original)
        self.assertEqual(json.loads(self.cli("agents", "status", "codex", "--json").stdout)["agent"]["model"], "before")

    def test_15_missing_gateway_blocks_config_commit(self):
        target = self.root / "codex" / "missing-gateway.toml"
        self.cli("agents", "connect", "codex", "--model", "local/fixture-a", "--config", str(target), "--gateway", "127.0.0.1:1", "--apply", "--json", ok=False)
        self.assertFalse(target.exists())

    def test_16_valid_incomplete_response_is_preserved_and_not_success(self):
        _, _, before = self.request("GET", "/health")
        status, _, body = self.request("POST", "/v1/responses", {"model": "local/incomplete"})
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(body)["status"], "incomplete")
        connection, response = self.stream("incomplete", "/v1/responses")
        try:
            body = response.read()
            self.assertIn(b"event: response.incomplete", body)
            self.assertIn(b"max_output_tokens", body)
        finally:
            connection.close()
        self.wait_active(0)
        _, _, after = self.request("GET", "/health")
        self.assertEqual(json.loads(after)["transport_counters"]["completed"], json.loads(before)["transport_counters"]["completed"])

    def test_17_malformed_chat_terminal_is_not_success(self):
        status, _, _ = self.request("POST", "/v1/chat/completions", {"model": "local/bad-finish"})
        self.assertEqual(status, 502)
        connection, response = self.stream("bad-choices")
        try:
            with self.assertRaises(http.client.IncompleteRead):
                response.read()
        finally:
            connection.close()

    def test_18_cr_mixed_and_bom_error_events_never_leak(self):
        for path in ("/v1/chat/completions", "/v1/responses"):
            for model in FRAMED_ERRORS:
                with self.subTest(path=path, model=model):
                    connection, response = self.stream(model, path)
                    try:
                        with self.assertRaises(http.client.IncompleteRead) as caught:
                            response.read()
                        self.assertEqual(caught.exception.partial, b"",
                            "no byte of the rejected event may be released")
                    finally:
                        connection.close()
                    self.wait_active(0)

    def test_19_cr_mixed_and_bom_success_preserves_raw_completion(self):
        _, _, before = self.request("GET", "/health")
        for path, raw in (("/v1/chat/completions", CHAT_DELTA + CHAT_END),
                          ("/v1/responses", RESPONSE_DELTA + RESPONSE_END)):
            for model, (endings, bom) in SUCCESS_FRAMING.items():
                with self.subTest(path=path, model=model):
                    connection, response = self.stream(model, path)
                    try:
                        self.assertEqual(response.status, 200)
                        self.assertEqual(response.read(), framed_sse(raw, endings, bom))
                    finally:
                        connection.close()
                    self.wait_active(0)
        _, _, after = self.request("GET", "/health")
        expected = 2 * len(SUCCESS_FRAMING)
        self.assertEqual(json.loads(after)["transport_counters"]["completed"],
            json.loads(before)["transport_counters"]["completed"] + expected)

    def test_20_gateway_inspect_cli_exact_schema(self):
        count = len(Provider.records)
        result = self.cli("gateway", "inspect", "--listen", self.address, "--json")
        models = json.loads(self.config.read_bytes())["models"]
        version = self.cli("--version").stdout.strip().removeprefix("magpie ")
        self.assertEqual(json.loads(result.stdout), {
            "version": version,
            "gateway": {
                "address": self.address,
                "mode": "forward_candidate",
                "provider_health": "not_probed",
                "models": [{"id": "local/" + model, "source": "configured"} for model in models],
            },
        })
        self.assertEqual(len(Provider.records), count, "inspection must not contact the provider")

    def test_21_gateway_inspect_rejects_invalid_catalog_without_source_echo(self):
        class Catalog(http.server.BaseHTTPRequestHandler):
            mode = "forward_candidate"
            ids = ["local/fixture-a"]

            def log_message(self, *_):
                pass

            def do_GET(self):
                value = ({"mode": self.mode} if self.path == "/health" else
                         {"data": [{"id": model} for model in self.ids]})
                raw = encoded(value)
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Catalog)
        server.daemon_threads = True
        self.addCleanup(server.server_close)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.shutdown)
        address = f"127.0.0.1:{server.server_port}"
        for ids in ([], ["local/fixture-a"] * 2, [f"local/model-{i}" for i in range(129)],
                    ["fixture-private-id"], ["local/fixture-private-id?secret"],
                    ["p" * 65 + "/fixture-private-id"], ["local/" + "x" * 257]):
            with self.subTest(ids=ids[:2]):
                Catalog.ids = ids
                result = self.cli("gateway", "inspect", "--listen", address, "--json", ok=False)
                self.assertNotIn("fixture-private-id", result.stdout + result.stderr)
        Catalog.mode = "fixture-private-id"
        Catalog.ids = ["local/fixture-a"]
        result = self.cli("gateway", "inspect", "--listen", address, "--json", ok=False)
        self.assertNotIn("fixture-private-id", result.stdout + result.stderr)
        Catalog.mode = "forward_candidate"
        Catalog.ids = ["local/vendor/fixture-a"]
        result = self.cli("gateway", "inspect", "--listen", address, "--json")
        self.assertEqual(json.loads(result.stdout)["gateway"]["models"],
            [{"id": "local/vendor/fixture-a", "source": "configured"}])

    def test_22_cr_only_delta_is_delivered_before_next_upstream_byte(self):
        for path, raw in (("/v1/chat/completions", CHAT_DELTA + CHAT_END),
                          ("/v1/responses", RESPONSE_DELTA + RESPONSE_END)):
            with self.subTest(path=path):
                Provider.release.clear()
                connection, response = self.stream("cr-delayed", path)
                start = time.monotonic()
                first = bytearray()
                try:
                    while not first.endswith(b"\r\r"):
                        chunk = response.read1(4096)
                        self.assertTrue(chunk, "CR-only delta did not arrive")
                        first.extend(chunk)
                        self.assertLess(len(first), 4096)
                    self.assertIn("你好".encode(), first)
                    self.assertLess(time.monotonic() - start, 1.5)
                    Provider.release.set()
                    self.assertEqual(bytes(first) + response.read(), framed_sse(raw, (b"\r",)))
                finally:
                    Provider.release.set()
                    response.close()
                    connection.close()
                self.wait_active(0)

if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--report", type=Path)
    parser.add_argument("--startup-tests-only", action="store_true",
        help="run isolated stdlib hanging-process regressions without a Magpie binary")
    options = parser.parse_args()
    if options.binary is None and not options.startup_tests_only:
        parser.error("--binary is required unless --startup-tests-only is selected")
    BINARY = options.binary.resolve() if options.binary else None
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(StartupTests)
    if not options.startup_tests_only:
        suite.addTests(unittest.defaultTestLoader.loadTestsFromTestCase(FixtureTests))
    start = time.monotonic()
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    report = {"scope": ("isolated_stdlib_startup_regressions" if options.startup_tests_only else
                       "synthetic_provider_real_loopback_http_and_isolated_config_candidate"),
              "binary_sha256": hashlib.sha256(BINARY.read_bytes()).hexdigest() if BINARY else None,
              "tests_run": result.testsRun, "failures": len(result.failures), "errors": len(result.errors),
              "skipped": len(result.skipped),
              "duration_seconds": round(time.monotonic() - start, 3),
              "passed": result.wasSuccessful(), "real_codex_execution": "not_run", "native_gpui": "not_run",
              "external_tls": "not_run", "original_magpie_ab": "not_run"}
    if options.report:
        options.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(report, ensure_ascii=False))
    raise SystemExit(0 if result.wasSuccessful() else 1)
