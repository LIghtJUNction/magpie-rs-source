#!/usr/bin/env python3
"""Provider persistence -> host four-protocol HTTP/SSE chain, real loopback only."""
import argparse
import copy
import hashlib
import http.client
import json
from pathlib import Path
import socket
import threading
import time
import unittest
from urllib.parse import quote, unquote

import provider_loop as fixture

PROTOCOLS = ("chat", "responses", "messages", "gemini")
MODEL = "namespace/model@2026:trial"
TEXT = "中文🙂"
PAIR_RESULTS = []


def item():
    return {"id": "item_text", "type": "message", "role": "assistant", "status": "completed",
            "content": [{"type": "output_text", "text": TEXT, "annotations": []}]}


def response_body(protocol):
    if protocol == "chat":
        return {"id": "fixture", "object": "chat.completion", "created": 1700000000, "model": MODEL,
                "choices": [{"index": 0, "message": {"role": "assistant", "content": TEXT}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10}}
    if protocol == "responses":
        return {"id": "fixture", "object": "response", "created_at": 1700000000, "model": MODEL,
                "status": "completed", "output": [item()], "error": None, "incomplete_details": None,
                "usage": {"input_tokens": 7, "output_tokens": 3, "total_tokens": 10}}
    if protocol == "messages":
        return {"id": "fixture", "type": "message", "role": "assistant", "model": MODEL,
                "content": [{"type": "text", "text": TEXT}], "stop_reason": "end_turn", "stop_sequence": None,
                "usage": {"input_tokens": 7, "output_tokens": 3}}
    return {"responseId": "fixture", "modelVersion": MODEL,
            "candidates": [{"index": 0, "content": {"role": "model", "parts": [{"text": TEXT}]}, "finishReason": "STOP"}],
            "usageMetadata": {"promptTokenCount": 7, "candidatesTokenCount": 3, "totalTokenCount": 10}}


def event(name, value):
    return (("event: " + name + "\n").encode() if name else b"") + b"data: " + fixture.encoded(value) + b"\n\n"


def stream_body(protocol):
    if protocol == "chat":
        def chunk(delta, finish):
            return event("", {"id": "fixture", "object": "chat.completion.chunk", "created": 1700000000,
                "model": MODEL, "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
        return chunk({"role": "assistant", "content": TEXT}, None) + chunk({}, "stop") + event("", {
            "choices": [], "usage": {"prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10}}) + b"data: [DONE]\n\n"
    if protocol == "gemini":
        return event("", response_body(protocol))
    if protocol == "messages":
        events = [
            ("message_start", {"type": "message_start", "message": {"id": "fixture", "type": "message",
             "role": "assistant", "model": MODEL, "content": [], "stop_reason": None, "stop_sequence": None,
             "usage": {"input_tokens": 7, "output_tokens": 0}}}),
            ("content_block_start", {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
            ("content_block_delta", {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": TEXT}}),
            ("content_block_stop", {"type": "content_block_stop", "index": 0}),
            ("message_delta", {"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": None},
                               "usage": {"output_tokens": 3}}),
            ("message_stop", {"type": "message_stop"}),
        ]
    else:
        shell = response_body(protocol)
        shell.update(status="in_progress", output=[])
        del shell["usage"]
        events = [
            ("response.created", {"response": shell}),
            ("response.output_item.added", {"output_index": 0, "item": {"id": "item_text", "type": "message",
             "role": "assistant", "status": "in_progress", "content": []}}),
            ("response.content_part.added", {"item_id": "item_text", "output_index": 0, "content_index": 0,
             "part": {"type": "output_text", "text": "", "annotations": []}}),
            ("response.output_text.delta", {"item_id": "item_text", "output_index": 0, "content_index": 0, "delta": TEXT}),
            ("response.output_text.done", {"item_id": "item_text", "output_index": 0, "content_index": 0, "text": TEXT}),
            ("response.content_part.done", {"item_id": "item_text", "output_index": 0, "content_index": 0,
             "part": {"type": "output_text", "text": TEXT, "annotations": []}}),
            ("response.output_item.done", {"output_index": 0, "item": item()}),
            ("response.completed", {"response": response_body(protocol)}),
        ]
        events = [(name, {**body, "type": name, "sequence_number": index}) for index, (name, body) in enumerate(events)]
    return b"".join(event(name, body) for name, body in events)


def request_body(protocol, model, stream):
    if protocol == "chat":
        body = {"model": model, "messages": [{"role": "user", "content": TEXT}], "max_completion_tokens": 64}
    elif protocol == "responses":
        body = {"model": model, "input": [{"role": "user", "content": TEXT}], "max_output_tokens": 64}
    elif protocol == "messages":
        body = {"model": model, "messages": [{"role": "user", "content": TEXT}], "max_tokens": 64}
    else:
        return {"contents": [{"role": "user", "parts": [{"text": TEXT}]}], "generationConfig": {"maxOutputTokens": 64}}
    body["stream"] = stream
    return body


def entry(protocol, public_model, stream):
    if protocol == "gemini":
        return "/v1beta/models/" + quote(public_model, safe="") + (":streamGenerateContent?alt=sse" if stream else ":generateContent")
    return {"chat": "/v1/chat/completions", "responses": "/v1/responses", "messages": "/v1/messages"}[protocol]


class ProtocolUpstream(fixture.Upstream):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
        self.record(body)
        if self.path.startswith("/v1beta/models/"):
            protocol = "gemini"
            stream = ":streamGenerateContent?alt=sse" in self.path
        else:
            protocol = {"/v1/chat/completions": "chat", "/v1/responses": "responses", "/v1/messages": "messages"}.get(self.path)
            stream = body.get("stream", False)
        if protocol is None:
            return self.reply(404, {"error": "wrong endpoint"})
        mode = self.server.protocol_mode
        if isinstance(mode, int):
            return self.reply(mode, {"error": {"message": fixture.SECRETS["MAGPIE_FIXTURE_FIRST"]}})
        if mode == "timeout":
            time.sleep(10.8)
        if mode == "error-stream":
            return self.reply(200, event("error", {"error": {"message": fixture.SECRETS["MAGPIE_FIXTURE_FIRST"]}}), "text/event-stream")
        if mode == "hold":
            prefix = stream_body("chat").split(b"\n\n", 1)[0] + b"\n\n"
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Transfer-Encoding", "chunked")
            self.end_headers()
            try:
                self.wfile.write(f"{len(prefix):x}\r\n".encode() + prefix + b"\r\n")
                self.wfile.flush()
                self.server.held_entered.set()
                self.connection.settimeout(.15)
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    try:
                        if not self.connection.recv(1):
                            self.server.held_closed.set()
                            break
                    except socket.timeout:
                        continue
            except (BrokenPipeError, ConnectionResetError):
                self.server.held_closed.set()
            self.close_connection = True
            return
        if stream:
            return self.reply(200, stream_body(protocol), "text/event-stream")
        return self.reply(200, response_body(protocol))


class ProtocolTests(fixture.ProviderTests):
    def setUp(self):
        super().setUp()
        self.upstream.RequestHandlerClass = ProtocolUpstream
        self.upstream.protocol_mode = "normal"
        self.upstream.held_entered = threading.Event()
        self.upstream.held_closed = threading.Event()

    def start_all(self):
        base = self.base_url
        for protocol in PROTOCOLS:
            self.base_url = base[:-3] + "/v1beta" if protocol == "gemini" else base
            self.add(provider=protocol, protocol=protocol, models=[MODEL, "other@2026"])
        self.base_url = base
        self.start_gateway()

    def send(self, client, provider, stream=False, extra=None):
        public = provider + "/" + MODEL
        body = request_body(client, public, stream)
        body.update(extra or {})
        return self.request("POST", entry(client, public, stream), body, {
            "Authorization": "Bearer synthetic-client-key", "X-Api-Key": "synthetic-client-anthropic",
            "X-Goog-Api-Key": "synthetic-client-gemini"})

    def assert_upstream(self, provider, stream, expected_key="MAGPIE_FIXTURE_FIRST"):
        recorded = self.upstream.records[-1]
        headers = recorded["headers"]
        expected = fixture.SECRETS[expected_key]
        if provider in ("chat", "responses"):
            self.assertEqual(headers["authorization"], "Bearer " + expected)
            self.assertNotIn("x-api-key", headers)
            self.assertNotIn("x-goog-api-key", headers)
        elif provider == "messages":
            self.assertEqual(headers["x-api-key"], expected)
            self.assertEqual(headers["anthropic-version"], "2023-06-01")
            self.assertNotIn("authorization", headers)
            self.assertNotIn("x-goog-api-key", headers)
        else:
            self.assertEqual(headers["x-goog-api-key"], expected)
            self.assertNotIn("authorization", headers)
            self.assertNotIn("x-api-key", headers)
        expected_path = entry(provider, MODEL, stream)
        self.assertEqual(recorded["path"], expected_path)
        if provider == "gemini":
            self.assertNotIn("model", recorded["body"])
            self.assertNotIn("stream", recorded["body"])
            self.assertEqual(unquote(recorded["path"].split("/models/", 1)[1].rsplit(":", 1)[0]), MODEL)
        else:
            self.assertEqual(recorded["body"]["model"], MODEL)
            self.assertEqual(recorded["body"]["stream"], stream)

    def test_all_sixteen_json_pairs_use_persisted_route_and_protocol_auth(self):
        self.start_all()
        for client in PROTOCOLS:
            for provider in PROTOCOLS:
                with self.subTest(client=client, provider=provider):
                    before = len(self.upstream.records)
                    status, headers, body = self.send(client, provider)
                    self.assertEqual(status, 200, body)
                    self.assertIn(TEXT, json.dumps(json.loads(body), ensure_ascii=False))
                    self.assertEqual(len(self.upstream.records), before + 1)
                    self.assertEqual(headers["x-magpie-provider"], provider)
                    self.assertEqual(headers["x-magpie-state-revision"], str(self.snapshot["revision"]))
                    self.assertEqual(headers["x-magpie-provider-key"], "key-first")
                    self.assert_upstream(provider, False)
                    PAIR_RESULTS.append({"client": client, "provider": provider, "stream": False})

    def test_all_sixteen_sse_pairs_have_safe_completion_and_native_bytes(self):
        self.start_all()
        for client in PROTOCOLS:
            for provider in PROTOCOLS:
                with self.subTest(client=client, provider=provider):
                    before = len(self.upstream.records)
                    status, _, body = self.send(client, provider, True)
                    self.assertEqual(status, 200, body)
                    self.assertIn(TEXT.encode(), body)
                    self.assertEqual(len(self.upstream.records), before + 1)
                    if client == provider:
                        self.assertEqual(body, stream_body(provider))
                    if client == "responses":
                        values = [json.loads(line[6:]) for line in body.splitlines() if line.startswith(b"data: {")]
                        self.assertEqual(sum(value.get("type") == "response.completed" for value in values), 1)
                        self.assertFalse(any(value.get("type") == "response.failed" for value in values))
                    self.assert_upstream(provider, True)
                    PAIR_RESULTS.append({"client": client, "provider": provider, "stream": True})

    def test_live_key_and_protocol_edits_apply_without_restart(self):
        self.start_all()
        self.write("key-put", "chat", {"id": "key-first", "name": "Rotated", "env": "MAGPIE_FIXTURE_ROTATED"})
        self.assertEqual(self.send("responses", "chat")[0], 200)
        self.assert_upstream("chat", False, "MAGPIE_FIXTURE_ROTATED")
        self.write("edit", "chat", {"protocol": "responses"})
        self.assertEqual(self.send("responses", "chat")[0], 200)
        self.assert_upstream("responses", False, "MAGPIE_FIXTURE_ROTATED")
        self.write("key-remove", "chat", {"key_id": "key-first"})
        self.write("key-remove", "chat", {"key_id": "key-second"})
        before = len(self.upstream.records)
        self.assertEqual(self.send("responses", "chat")[0], 503)
        self.assertEqual(len(self.upstream.records), before)

    def test_translation_rejection_errors_and_timeout_never_retry_or_leak(self):
        self.start_all()
        before = len(self.upstream.records)
        self.assertEqual(self.send("responses", "chat", extra={"previous_response_id": "unsupported-history"})[0], 422)
        self.assertEqual(len(self.upstream.records), before)
        for status_code in (401, 429, 503):
            self.upstream.protocol_mode = status_code
            status, headers, body = self.send("responses", "chat")
            self.assertEqual(status, status_code)
            self.assertEqual(headers["x-magpie-provider-key"], "key-first")
            self.assertNotIn(fixture.SECRETS["MAGPIE_FIXTURE_FIRST"].encode(), body)
        self.assertEqual(len(self.upstream.records), before + 3)
        self.upstream.protocol_mode = "timeout"
        start = time.monotonic()
        status, _, body = self.send("responses", "chat")
        self.assertEqual(status, 504, body)
        self.assertLess(time.monotonic() - start, 12)
        self.assertEqual(len(self.upstream.records), before + 4)

    def test_translated_stream_error_is_not_success_or_secret_release(self):
        self.start_all()
        self.upstream.protocol_mode = "error-stream"
        status, _, body = self.send("responses", "chat", True)
        self.assertEqual(status, 200)
        self.assertNotIn(fixture.SECRETS["MAGPIE_FIXTURE_FIRST"].encode(), body)
        self.assertNotIn(b"response.completed", body)
        self.assertNotIn(b"[DONE]", body)
        _, _, health = self.request("GET", "/health")
        counters = json.loads(health)["transport_counters"]
        self.assertEqual((counters["completed"], counters["failed"]), (0, 1))
        self.assertEqual(len(self.upstream.records), 1)

    def test_downstream_cancellation_closes_managed_upstream(self):
        self.start_all()
        self.upstream.protocol_mode = "hold"
        public = "chat/" + MODEL
        connection = http.client.HTTPConnection(self.host, self.port, timeout=4)
        connection.request("POST", "/v1/responses", fixture.encoded(request_body("responses", public, True)),
                           {"Content-Type": "application/json"})
        response = connection.getresponse()
        self.assertEqual(response.status, 200)
        self.assertTrue(response.read1(128))
        self.assertTrue(self.upstream.held_entered.wait(1))
        response.close()
        connection.close()
        self.assertTrue(self.upstream.held_closed.wait(3), "Gateway must close the real upstream socket")
        deadline = time.monotonic() + 3
        while True:
            _, _, raw = self.request("GET", "/health")
            counters = json.loads(raw)["transport_counters"]
            if counters["active"] == 0:
                break
            self.assertLess(time.monotonic(), deadline)
            time.sleep(.02)
        self.assertEqual((counters["completed"], counters["failed"], counters["cancelled"]), (0, 0, 1))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    fixture.BINARY = args.binary.resolve()
    names = [name for name in ProtocolTests.__dict__ if name.startswith("test_")]
    start = time.monotonic()
    result = unittest.TextTestRunner(verbosity=2).run(unittest.TestSuite(ProtocolTests(name) for name in names))
    report = {"scope": "persisted_provider_store_through_host_four_protocol_real_tcp_sse",
              "binary_sha256": hashlib.sha256(fixture.BINARY.read_bytes()).hexdigest(),
              "tests_run": result.testsRun, "failures": len(result.failures), "errors": len(result.errors),
              "passed": result.wasSuccessful(), "duration_seconds": round(time.monotonic() - start, 3),
              "successful_pair_requests": PAIR_RESULTS, "external_accounts": "not_run",
              "model_discovery_messages_gemini": "still_not_integrated", "complete_product_acceptance": False}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(report, ensure_ascii=False))
    raise SystemExit(0 if result.wasSuccessful() else 1)
