#!/usr/bin/env python3
"""Synthetic regressions for native Gemini errors and local Provider failures."""
import argparse
from contextlib import contextmanager, nullcontext
import fcntl
import hashlib
import http.client
import json
from pathlib import Path
import threading
import time
import unittest

import provider_protocol_loop as protocol_fixture
import provider_loop as fixture


SECRET = b"synthetic-gemini-error-secret-never-forward"
OBSERVATIONS = {}
NATIVE_SUCCESS = (
    b'data: {"responseId":"fixture","modelVersion":"fixture-model",'
    b'"candidates":[{"index":0,"content":{"role":"model","parts":[{"text":"safe"}]},'
    b'"finishReason":"STOP"}]}\r\n\r\n'
    b'data: {"usageMetadata":{"promptTokenCount":2,"candidatesTokenCount":1,'
    b'"totalTokenCount":3}}\r\n\r\n'
)
ERROR_FRAMES = [
    b'data: {"type":"error","message":"synthetic-gemini-error-secret-never-forward"}\n\n',
    b'event: error\ndata: {"message":"synthetic-gemini-error-secret-never-forward"}\n\n',
    b'data: {"type":"response.failed","response":{"error":{"message":'
    b'"synthetic-gemini-error-secret-never-forward"}}}\n\n',
    b'data: {"error":{"message":"synthetic-gemini-error-secret-never-forward"}}\n\n',
    b'data: {"message":"synthetic-gemini-error-secret-never-forward"}\n\n',
    b'data: {"status":"UNAVAILABLE","code":503,"message":'
    b'"synthetic-gemini-error-secret-never-forward"}\n\n',
    b'data: {"type":"error","candidates":[],"message":'
    b'"synthetic-gemini-error-secret-never-forward"}\n\n',
    b'data: {"candidates":[],"failure":{"message":"synthetic-gemini-error-secret-never-forward"}}\n\n',
    b'data: {"usageMetadata":{"message":"synthetic-gemini-error-secret-never-forward"}}\n\n',
    b'data: {"candidates":["synthetic-gemini-error-secret-never-forward"]}\n\n',
]


class RegressionUpstream(protocol_fixture.ProtocolUpstream):
    def do_POST(self):
        raw = getattr(self.server, "override_sse", None)
        if raw is None or not self.path.startswith("/v1beta/models/"):
            return super().do_POST()

        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
        self.record(body)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Transfer-Encoding", "chunked")
        self.send_header("Connection", "close")
        self.end_headers()
        chunk_size = self.server.override_chunk_size
        try:
            for offset in range(0, len(raw), chunk_size):
                chunk = raw[offset : offset + chunk_size]
                self.wfile.write(f"{len(chunk):x}\r\n".encode() + chunk + b"\r\n")
                self.wfile.flush()
            self.wfile.write(b"0\r\n\r\n")
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass
        self.close_connection = True


class ProviderErrorRegressions(protocol_fixture.ProtocolTests):
    """Reuses only the isolated persisted-store and loopback fixture helpers."""

    def setUp(self):
        super().setUp()
        self.upstream.RequestHandlerClass = RegressionUpstream
        self.upstream.override_sse = None
        self.upstream.override_chunk_size = 1
        self.upstream.tcp_connections = 0
        self.upstream.tcp_lock = threading.Lock()
        original_get_request = self.upstream.get_request

        def count_connection():
            connection = original_get_request()
            with self.upstream.tcp_lock:
                self.upstream.tcp_connections += 1
            return connection

        self.upstream.get_request = count_connection

    def send_native_sse(self, raw, chunk_size=1):
        self.upstream.override_sse = raw
        self.upstream.override_chunk_size = chunk_size
        public = "gemini/" + protocol_fixture.MODEL
        body = protocol_fixture.request_body("gemini", public, True)
        connection = http.client.HTTPConnection(self.host, self.port, timeout=14)
        try:
            connection.request("POST", protocol_fixture.entry("gemini", public, True),
                               fixture.encoded(body), {"Content-Type": "application/json"})
            response = connection.getresponse()
            status = response.status
            headers = dict(response.getheaders())
            try:
                content = response.read()
                read_result = "complete"
            except http.client.IncompleteRead as error:
                content = error.partial
                read_result = "incomplete_body"
            return status, headers, content, read_result
        finally:
            connection.close()

    def health_counters(self):
        status, _, body = self.request("GET", "/health")
        self.assertEqual(status, 200)
        return json.loads(body)["transport_counters"]

    @staticmethod
    def counter_delta(after, before):
        return {key: after[key] - before[key] for key in after}

    def test_native_gemini_typed_error_never_escapes_or_completes(self):
        self.start_all()
        seen = []
        for prefix in (b"", NATIVE_SUCCESS):
            for index, error in enumerate(ERROR_FRAMES):
                with self.subTest(after_candidate=bool(prefix), error_frame=index):
                    before_records = len(self.upstream.records)
                    before_connections = self.upstream.tcp_connections
                    before_counters = self.health_counters()
                    status, _, body, read_result = self.send_native_sse(
                        prefix + error, 1 if index == 0 else 7
                    )
                    counters = self.counter_delta(self.health_counters(), before_counters)
                    observation = {
                        "after_candidate": bool(prefix),
                        "error_frame": index,
                        "http_status": status,
                        "downstream_body_read": read_result,
                        "secret_escaped": SECRET in body,
                        "sse_error_frame_escaped": error.split(b"\n", 1)[-1].strip() in body,
                        "upstream_records": len(self.upstream.records) - before_records,
                        "upstream_tcp_connections": self.upstream.tcp_connections - before_connections,
                        "transport_counters": counters,
                    }
                    seen.append(observation)
                    self.assertEqual(status, 200, observation)
                    self.assertNotIn(SECRET, body, observation)
                    self.assertNotIn(b"response.completed", body, observation)
                    self.assertNotIn(b"[DONE]", body, observation)
                    self.assertNotIn(
                        b'"finishReason":"STOP"', body,
                        "hold Gemini's terminal candidate until upstream EOF confirms success",
                    )
                    self.assertEqual(observation["upstream_records"], 1, observation)
                    self.assertEqual(observation["upstream_tcp_connections"], 1, observation)
                    self.assertEqual((counters["active"], counters["completed"], counters["failed"]),
                                     (0, 0, 1), observation)
        OBSERVATIONS["gemini_error_observations"] = seen

    def test_native_gemini_success_keeps_fragmented_candidate_and_usage_tail(self):
        self.start_all()
        before_counters = self.health_counters()
        status, _, body, read_result = self.send_native_sse(NATIVE_SUCCESS, 1)
        counters = self.counter_delta(self.health_counters(), before_counters)
        self.assertEqual(status, 200)
        self.assertEqual(read_result, "complete")
        self.assertIn(b'"safe"', body)
        self.assertIn(b'"usageMetadata"', body)
        self.assertEqual(body, NATIVE_SUCCESS)
        self.assertEqual((counters["completed"], counters["failed"]), (1, 0))
        OBSERVATIONS["gemini_success"] = {
            "http_status": status,
            "raw_native_bytes_preserved": body == NATIVE_SUCCESS,
            "fragment_size": 1,
            "downstream_body_read": read_result,
            "transport_counters": counters,
        }

    def test_native_gemini_preserves_order_when_another_candidate_starts_later(self):
        self.start_all()
        raw = (
            b'data: {"candidates":[{"index":0,"finishReason":"STOP"}]}\n\n'
            b'data: {"candidates":[{"index":1,"content":{"parts":[{"text":"later"}]}}]}\n\n'
            b'data: {"candidates":[{"index":1,"finishReason":"STOP"}]}\n\n'
        )
        before = self.health_counters()
        status, _, body, read_result = self.send_native_sse(raw, 7)
        counters = self.counter_delta(self.health_counters(), before)
        self.assertEqual(status, 200)
        self.assertEqual(read_result, "complete")
        self.assertEqual(body, raw)
        self.assertEqual((counters["completed"], counters["failed"]), (1, 0))
        OBSERVATIONS["gemini_late_candidate_order"] = {
            "raw_native_bytes_preserved": body == raw,
            "transport_counters": counters,
        }

    def test_native_gemini_accepts_official_metadata_tail(self):
        self.start_all()
        raw = (
            b'data: {"candidates":[{"index":0,"finishReason":"STOP"}]}\n\n'
            b'data: {"modelStatus":{"modelStage":"STABLE"},'
            b'"createTime":"2026-10-29T00:00:00Z",'
            b'"usageMetadata":{"totalTokenCount":3}}\n\n'
        )
        status, _, body, read_result = self.send_native_sse(raw, 7)
        self.assertEqual(status, 200)
        self.assertEqual(read_result, "complete")
        self.assertEqual(body, raw)
        OBSERVATIONS["gemini_official_metadata"] = {"raw_native_bytes_preserved": body == raw}

    @contextmanager
    def held_store_lock(self):
        lock_path = Path(str(self.store) + ".lock.v2")
        with lock_path.open("rb") as lock_file:
            fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            try:
                yield
            finally:
                fcntl.flock(lock_file.fileno(), fcntl.LOCK_UN)

    @contextmanager
    def pending_recovery(self):
        journal = Path(str(self.store) + ".transaction")
        journal.write_text("synthetic pending transaction\n")
        try:
            yield
        finally:
            journal.unlink(missing_ok=True)

    def test_local_provider_failures_keep_four_protocol_error_shapes_and_make_zero_requests(self):
        self.env["MAGPIE_FIXTURE_INVALID"] = "fixture key with space"
        self.start_all()
        codes = {
            "busy": "provider_busy",
            "recovery": "provider_recovery_required",
            "credential": "provider_credential_unavailable",
            "credential_invalid": "provider_credential_invalid",
        }
        observations = []
        for kind in codes:
            self.write("add", kind, {
                "name": f"fixture-{kind}",
                "base_url": self.base_url,
                "protocol": "chat",
                "models": [protocol_fixture.MODEL],
                "keys": [{
                    "id": "key-first",
                    "name": "First",
                    "env": {
                        "credential": "MAGPIE_FIXTURE_MISSING",
                        "credential_invalid": "MAGPIE_FIXTURE_INVALID",
                    }.get(kind, "MAGPIE_FIXTURE_FIRST"),
                }],
                "auto_include": True,
            })
        for kind, local_code in codes.items():
            for client in protocol_fixture.PROTOCOLS:
                for streaming in (False, True):
                    with self.subTest(
                        local_error=kind,
                        client_protocol=client,
                        streaming=streaming,
                    ):
                        if kind == "busy":
                            guard = self.held_store_lock()
                        elif kind == "recovery":
                            guard = self.pending_recovery()
                        else:
                            guard = nullcontext()
                        before_records = len(self.upstream.records)
                        before_connections = self.upstream.tcp_connections
                        before_counters = self.health_counters()
                        with guard:
                            status, _, body = self.send(client, kind, streaming)
                        counters = self.counter_delta(self.health_counters(), before_counters)
                        parsed = json.loads(body)
                        observation = {
                            "local_error": kind,
                            "client_protocol": client,
                            "streaming_request": streaming,
                            "http_status": status,
                            "local_code_present": local_code.encode() in body,
                            "upstream_records": len(self.upstream.records) - before_records,
                            "upstream_tcp_connections": (
                                self.upstream.tcp_connections - before_connections
                            ),
                            "transport_counters": counters,
                        }
                        observations.append(observation)
                        self.assertEqual(status, 503, observation)
                        self.assertEqual(observation["upstream_records"], 0, observation)
                        self.assertEqual(
                            observation["upstream_tcp_connections"], 0, observation
                        )
                        self.assertEqual(
                            (counters["active"], counters["completed"], counters["failed"]),
                            (0, 0, 0),
                            observation,
                        )
                        self.assertIn(local_code, body.decode(), observation)
                        if client in ("chat", "responses"):
                            self.assertEqual(parsed["error"]["code"], local_code, observation)
                            self.assertEqual(parsed["error"]["type"], "server_error", observation)
                        elif client == "messages":
                            self.assertEqual(parsed["type"], "error", observation)
                            self.assertEqual(parsed["error"]["type"], "api_error", observation)
                            self.assertEqual(parsed["error"]["message"], local_code, observation)
                        else:
                            self.assertEqual(parsed["error"]["code"], 503, observation)
                            self.assertEqual(parsed["error"]["status"], "UNAVAILABLE", observation)
                            self.assertEqual(parsed["error"]["message"], local_code, observation)
        OBSERVATIONS["local_provider_error_observations"] = observations


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    fixture.BINARY = args.binary.resolve()
    names = [name for name in ProviderErrorRegressions.__dict__ if name.startswith("test_")]
    suite = unittest.TestSuite(ProviderErrorRegressions(name) for name in names)
    started = time.monotonic()
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    case_count = (
        len(OBSERVATIONS.get("gemini_error_observations", []))
        + len(OBSERVATIONS.get("local_provider_error_observations", []))
        + int("gemini_success" in OBSERVATIONS)
        + int("gemini_late_candidate_order" in OBSERVATIONS)
        + int("gemini_official_metadata" in OBSERVATIONS)
    )
    report = {
        "scope": "synthetic_loopback_gemini_native_errors_and_local_provider_failures",
        "binary_sha256": hashlib.sha256(fixture.BINARY.read_bytes()).hexdigest(),
        "tests_run": result.testsRun,
        "synthetic_http_cases": case_count,
        "failures": len(result.failures),
        "errors": len(result.errors),
        "passed": result.wasSuccessful(),
        "duration_seconds": round(time.monotonic() - started, 3),
        "external_accounts": "not_used",
        **OBSERVATIONS,
    }
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(report, ensure_ascii=False))
    raise SystemExit(0 if result.wasSuccessful() else 1)


if __name__ == "__main__":
    main()
