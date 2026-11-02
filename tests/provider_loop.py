#!/usr/bin/env python3
"""Provider state -> live Gateway -> synthetic upstream, through real TCP/CLI.

Never reads a real Agent home or account. Run against an explicitly selected
Magpie binary. Passing does not establish external-account, TLS, or native UI
acceptance. It validates the first shared-state Provider implementation slice.
"""
import argparse
import hashlib
import http.client
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
import unittest

from first_loop import CandidateProcess


SECRETS = {
    "MAGPIE_FIXTURE_FIRST": "synthetic-first-key-do-not-use-in-production",
    "MAGPIE_FIXTURE_SECOND": "synthetic-second-key-do-not-use-in-production",
    "MAGPIE_FIXTURE_ROTATED": "synthetic-rotated-key-do-not-use-in-production",
}


def encoded(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()


def chat(model):
    return {"id": "fixture-chat", "object": "chat.completion", "model": model,
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "真实中文闭环"},
                         "finish_reason": "stop"}], "usage": {"prompt_tokens": 3, "completion_tokens": 4}}


def responses(model):
    return {"id": "fixture-response", "object": "response", "status": "completed", "model": model,
            "error": None, "output": [{"type": "message", "role": "assistant",
                "content": [{"type": "output_text", "text": "真实中文闭环"}]}]}


STREAM = (b'data: ' + encoded({"id": "fixture-stream", "choices": [{"index": 0,
    "delta": {"content": "中文流式"}, "finish_reason": "stop"}]}) + b'\r\n\r\ndata: [DONE]\r\n\r\n')


class Upstream(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def record(self, body=None):
        with self.server.record_lock:
            self.server.records.append({"method": self.command, "path": self.path,
                "headers": {k.lower(): v for k, v in self.headers.items()}, "body": body})

    def reply(self, status, value, media="application/json"):
        raw = encoded(value) if not isinstance(value, bytes) else value
        self.send_response(status)
        self.send_header("Content-Type", media)
        self.send_header("Content-Length", str(len(raw)))
        self.send_header("Connection", "close")
        self.end_headers()
        try:
            self.wfile.write(raw)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def do_GET(self):
        self.record()
        self.server.catalog_entered.set()
        self.server.catalog_release.wait(12)
        mode = self.server.catalog_mode
        if mode == "timeout":
            time.sleep(11)
        if isinstance(mode, int):
            return self.reply(mode, {"error": {"message": SECRETS["MAGPIE_FIXTURE_FIRST"]}})
        if mode == "invalid":
            return self.reply(200, {"data": [{"id": SECRETS["MAGPIE_FIXTURE_FIRST"] + "?invalid"}]})
        if mode == "reflect-credential":
            return self.reply(200, {"data": [{"id": SECRETS["MAGPIE_FIXTURE_FIRST"]}]})
        if mode == "oversize":
            return self.reply(200, b'{"data":[' + b' ' * (1024 * 1024 + 1) + b']}')
        return self.reply(200, {"object": "list", "data": [{"id": model} for model in self.server.catalog]})

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = json.loads(self.rfile.read(length))
        self.record(body)
        model = body["model"]
        if model.startswith("status-"):
            return self.reply(int(model.split("-")[1]),
                              {"error": {"message": SECRETS["MAGPIE_FIXTURE_FIRST"]}})
        if model == "timeout":
            time.sleep(11)
        if model == "invalid":
            return self.reply(200, {"error": {"message": SECRETS["MAGPIE_FIXTURE_FIRST"]}})
        if body.get("stream"):
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Connection", "close")
            self.end_headers()
            try:
                for index in range(0, len(STREAM), 3):
                    self.wfile.write(STREAM[index:index + 3])
                    self.wfile.flush()
                if model == "hold":
                    self.server.stream_release.wait(12)
            except (BrokenPipeError, ConnectionResetError):
                pass
            return
        return self.reply(200, responses(model) if self.path.endswith("/responses") else chat(model))


class ProviderTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="magpie-provider-loop-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.store = self.root / "state" / "providers.toml"
        self.env = {"PATH": os.defpath, "HOME": str(self.root), "XDG_CONFIG_HOME": str(self.root / "xdg"),
                    "MAGPIE_CONFIG_DIR": str(self.store.parent), **SECRETS}
        self.upstream = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Upstream)
        self.upstream.daemon_threads = True
        self.upstream.records = []
        self.upstream.record_lock = threading.Lock()
        self.upstream.catalog = ["alpha", "beta", "stream"]
        self.upstream.catalog_mode = "ok"
        self.upstream.catalog_entered = threading.Event()
        self.upstream.catalog_release = threading.Event()
        self.upstream.catalog_release.set()
        self.upstream.stream_release = threading.Event()
        self.addCleanup(self.upstream.server_close)
        threading.Thread(target=self.upstream.serve_forever, daemon=True).start()
        self.addCleanup(self.upstream.shutdown)
        self.base_url = f"http://127.0.0.1:{self.upstream.server_port}/v1"
        self.snapshot = self.cli("list")
        self.gateway = None

    def invoke(self, command, provider=None, data=None, expect=None, raw=None, ok=True, timeout=14):
        args = [str(BINARY), "providers", command]
        if provider is not None:
            args.append(provider)
        args.extend(["--store", str(self.store), "--json"])
        if expect is not None:
            args.extend(["--expect", expect])
        if data is not None or raw is not None:
            args.append("--stdin")
        result = subprocess.run(args, input=raw if raw is not None else encoded(data) if data is not None else None,
                                capture_output=True, env=self.env, timeout=timeout)
        for secret in SECRETS.values():
            self.assertNotIn(secret.encode(), result.stdout + result.stderr)
        if ok:
            self.assertEqual(result.returncode, 0, result.stderr.decode("utf-8", "replace"))
        else:
            self.assertNotEqual(result.returncode, 0)
        return result

    def cli(self, command, provider=None, data=None, expect=None, raw=None):
        result = self.invoke(command, provider, data, expect, raw)
        value = json.loads(result.stdout)
        self.assertEqual(value["version"], "0.2.0")
        self.assertEqual(value["schema_version"], 1)
        self.snapshot = value
        return value

    def write(self, command, provider="fixture", data=None):
        return self.cli(command, provider, data, self.snapshot["token"])

    def add(self, provider="fixture", protocol="chat", keys=True, models=None):
        return self.write("add", provider, {"name": "合成供应商", "base_url": self.base_url,
            "protocol": protocol, "models": models or ["alpha", "beta", "stream"],
            "keys": [{"id": "key-first", "name": "First", "env": "MAGPIE_FIXTURE_FIRST"},
                     {"id": "key-second", "name": "Second", "env": "MAGPIE_FIXTURE_SECOND"}] if keys else [],
            "auto_include": True})

    def start_gateway(self):
        self.gateway = CandidateProcess([str(BINARY), "serve", "--listen", "127.0.0.1:0",
                                         "--providers", str(self.store)], self.env)
        self.addCleanup(self.gateway.close)
        self.address = self.gateway.wait_for_bind()
        self.host, port = self.address.rsplit(":", 1)
        self.port = int(port)
        deadline = time.monotonic() + 5
        while True:
            try:
                status, _, _ = self.request("GET", "/health")
                if status == 200:
                    return
            except OSError:
                pass
            self.assertLess(time.monotonic(), deadline, "gateway did not become ready")
            time.sleep(.02)

    def request(self, method, path, data=None, headers=None):
        connection = http.client.HTTPConnection(self.host, self.port, timeout=14)
        try:
            h = {"Content-Type": "application/json"}
            h.update(headers or {})
            connection.request(method, path, encoded(data) if data is not None else None, h)
            response = connection.getresponse()
            return response.status, dict(response.getheaders()), response.read()
        finally:
            connection.close()

    def generate(self, model="fixture/alpha", path="/v1/chat/completions", **extra):
        return self.request("POST", path, {"model": model, "messages": [{"role": "user", "content": "合成输入"}], **extra})

    def test_01_empty_missing_state_read_does_not_write(self):
        self.assertEqual(self.snapshot["revision"], 0)
        self.assertEqual(self.snapshot["providers"], [])
        self.assertFalse(self.store.exists())
        self.start_gateway()
        status, _, body = self.request("GET", "/v1/models")
        self.assertEqual((status, json.loads(body)["data"]), (200, []))
        self.assertFalse(self.store.exists())
        result = subprocess.run([str(BINARY), "gateway", "inspect", "--listen", self.address, "--json"],
                                capture_output=True, env=self.env, timeout=5)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(json.loads(result.stdout)["gateway"]["models"], [])

    def test_02_persist_reload_and_live_gateway_share_revision(self):
        added = self.add()
        self.assertEqual(added, self.cli("list"))
        self.assertEqual(added, self.cli("show", "fixture"))
        self.start_gateway()
        status, _, raw = self.request("GET", "/v1/models")
        catalog = json.loads(raw)
        self.assertEqual(status, 200)
        self.assertEqual(catalog["state_revision"], added["revision"])
        self.assertEqual([item["id"] for item in catalog["data"]], ["fixture/alpha", "fixture/beta", "fixture/stream"])
        self.assertTrue(all(item["magpie"]["provider_health"] == "not_probed" for item in catalog["data"]))
        status, headers, raw = self.generate(extension={"exact": 123456789012345678901234567890})
        self.assertEqual((status, json.loads(raw)["choices"][0]["message"]["content"]), (200, "真实中文闭环"))
        self.assertEqual(headers["x-magpie-state-revision"], str(added["revision"]))
        self.assertEqual(headers["x-magpie-provider-key"], "key-first")
        record = self.upstream.records[-1]
        self.assertEqual(record["body"]["model"], "alpha")
        self.assertEqual(record["headers"]["authorization"], "Bearer " + SECRETS["MAGPIE_FIXTURE_FIRST"])
        self.assertEqual(record["body"]["extension"]["exact"], 123456789012345678901234567890)
        for path in self.store.parent.iterdir():
            if path.is_file():
                for secret in SECRETS.values():
                    self.assertNotIn(secret.encode(), path.read_bytes())

    def test_03_pause_and_rotate_key_changes_next_request(self):
        self.add()
        self.start_gateway()
        self.write("key-put", data={"id": "key-first", "name": "Rotated", "env": "MAGPIE_FIXTURE_ROTATED"})
        self.assertEqual(self.generate()[0], 200)
        self.assertEqual(self.upstream.records[-1]["headers"]["authorization"], "Bearer " + SECRETS["MAGPIE_FIXTURE_ROTATED"])
        self.write("key-put", data={"id": "key-first", "name": "Rotated", "env": "MAGPIE_FIXTURE_ROTATED", "enabled": False})
        self.assertEqual(self.generate()[1]["x-magpie-provider-key"], "key-second")
        self.assertEqual(self.upstream.records[-1]["headers"]["authorization"], "Bearer " + SECRETS["MAGPIE_FIXTURE_SECOND"])
        self.write("key-remove", data={"key_id": "key-second"})
        count = len(self.upstream.records)
        self.assertEqual(self.generate()[0], 503)
        self.assertEqual(len(self.upstream.records), count)
        tombstone = self.snapshot["providers"][0]["keys"][1]
        self.assertEqual((tombstone["id"], tombstone["deleted"]), ("key-second", True))

    def test_04_models_selection_enable_remove_live_reload(self):
        self.add()
        self.start_gateway()
        self.write("models", data={"models": ["alpha", "beta"], "selected_models": ["beta"], "auto_include": False})
        status, _, raw = self.request("GET", "/v1/models")
        self.assertEqual([item["id"] for item in json.loads(raw)["data"]], ["fixture/beta"])
        self.assertEqual(status, 200)
        count = len(self.upstream.records)
        self.assertEqual(self.generate()[0], 404)
        self.assertEqual(len(self.upstream.records), count)
        self.assertEqual(self.generate("fixture/beta")[0], 200)
        self.write("edit", data={"enabled": False})
        self.assertEqual(self.generate("fixture/beta")[0], 503)
        self.write("edit", data={"enabled": True})
        self.write("remove")
        self.assertTrue(self.snapshot["providers"][0]["deleted"])
        self.assertEqual(self.generate("fixture/beta")[0], 404)

    def test_05_cas_conflict_does_not_overwrite_external_edit(self):
        self.add()
        before = self.snapshot["token"]
        self.write("edit", data={"name": "New name"})
        saved = self.store.read_bytes()
        result = self.invoke("edit", "fixture", {"name": "Stale name"}, before, ok=False)
        self.assertEqual(json.loads(result.stderr)["error"]["code"], "provider_revision_conflict")
        self.assertEqual(self.store.read_bytes(), saved)

    def test_06_catalog_refresh_success_and_stale_failure_preserves_data(self):
        self.add()
        self.upstream.catalog = ["alpha", "new-model"]
        refreshed = self.write("refresh")
        self.assertTrue(refreshed["operation"]["ok"])
        catalog = refreshed["providers"][0]
        self.assertEqual(catalog["models"], ["alpha", "new-model"])
        self.start_gateway()
        for mode, code in [(401, "provider_http_401"), (429, "provider_http_429"), (503, "provider_http_5xx"),
                           ("invalid", "provider_catalog_invalid"), ("reflect-credential", "provider_catalog_invalid"),
                           ("oversize", "provider_response_too_large")]:
            with self.subTest(mode=mode):
                self.upstream.catalog_mode = mode
                result = self.write("refresh")
                self.assertFalse(result["operation"]["ok"])
                self.assertEqual(result["operation"]["code"], code)
                self.assertEqual(result["providers"][0]["models"], catalog["models"])
                self.assertEqual(result["providers"][0]["catalog_checked_at"], catalog["catalog_checked_at"])
                self.assertIsNotNone(result["providers"][0]["catalog_error"])
                status, _, raw = self.request("GET", "/v1/models")
                self.assertEqual(status, 200)
                self.assertTrue(json.loads(raw)["data"][0]["magpie"]["catalog_stale"])

    def test_07_catalog_timeout_is_bounded_and_retains_models(self):
        self.add()
        self.upstream.catalog_mode = "timeout"
        start = time.monotonic()
        result = self.write("refresh")
        self.assertLess(time.monotonic() - start, 12)
        self.assertEqual(result["operation"]["code"], "provider_timeout")
        self.assertEqual(result["providers"][0]["models"], ["alpha", "beta", "stream"])

    def test_08_generation_errors_are_sanitized_and_not_retried(self):
        self.add(models=["status-401", "status-429", "status-503", "invalid"])
        self.start_gateway()
        for model, expected in [("status-401", 401), ("status-429", 429), ("status-503", 503), ("invalid", 502)]:
            with self.subTest(model=model):
                count = len(self.upstream.records)
                status, headers, raw = self.generate("fixture/" + model)
                self.assertEqual(status, expected)
                self.assertEqual(len(self.upstream.records), count + 1)
                self.assertEqual(headers["x-magpie-provider-key"], "key-first")
                for secret in SECRETS.values():
                    self.assertNotIn(secret.encode(), raw)

    def test_09_native_chat_stream_and_responses_preserve_payloads(self):
        self.add()
        self.add("native-responses", protocol="responses")
        self.start_gateway()
        status, _, raw = self.generate("fixture/stream", stream=True)
        self.assertEqual((status, raw), (200, STREAM))
        status, headers, raw = self.generate("native-responses/alpha", path="/v1/responses", input="保留输入")
        self.assertEqual((status, json.loads(raw)["status"]), (200, "completed"))
        self.assertEqual(headers["x-magpie-provider"], "native-responses")
        self.assertEqual(self.upstream.records[-1]["body"]["input"], "保留输入")
        count = len(self.upstream.records)
        self.assertEqual(self.generate("fixture/alpha", path="/v1/responses")[0], 501)
        self.assertEqual(len(self.upstream.records), count)

    def test_10_missing_key_and_deleted_keys_never_degrade_to_no_auth(self):
        self.add()
        self.start_gateway()
        self.write("key-put", data={"id": "key-first", "name": "Unavailable", "env": "MAGPIE_FIXTURE_MISSING"})
        self.assertEqual(self.generate()[0], 503)
        self.assertEqual(self.upstream.records, [])
        self.write("key-remove", data={"key_id": "key-first"})
        self.write("key-remove", data={"key_id": "key-second"})
        self.assertEqual(self.generate()[0], 503)
        self.assertEqual(self.upstream.records, [])

    def test_11_no_key_local_provider_is_supported(self):
        self.add(keys=False)
        self.start_gateway()
        self.assertEqual(self.generate()[0], 200)
        self.assertNotIn("authorization", self.upstream.records[-1]["headers"])

    def test_12_corrupt_current_state_fails_closed_without_stale_success(self):
        self.add()
        self.start_gateway()
        before = self.store.read_bytes()
        self.store.write_bytes(b"invalid = [fixture-private-material")
        count = len(self.upstream.records)
        for method, path, body in [("GET", "/health", None), ("GET", "/v1/models", None),
                                   ("POST", "/v1/chat/completions", {"model": "fixture/alpha"})]:
            status, _, raw = self.request(method, path, body)
            self.assertEqual(status, 503)
            self.assertNotIn(b"fixture-private-material", raw)
        self.assertEqual(len(self.upstream.records), count)
        self.store.write_bytes(before)
        self.assertEqual(self.generate()[0], 200)

    def test_13_catalog_result_cannot_overwrite_concurrent_cli_edit(self):
        self.add()
        token = self.snapshot["token"]
        self.upstream.catalog_release.clear()
        process = subprocess.Popen([str(BINARY), "providers", "refresh", "fixture", "--store", str(self.store),
                                    "--expect", token, "--json"], stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, env=self.env)
        self.addCleanup(lambda: process.poll() is None and process.kill())
        self.assertTrue(self.upstream.catalog_entered.wait(3))
        self.write("edit", data={"name": "Concurrent edit"})
        saved = self.store.read_bytes()
        self.upstream.catalog_release.set()
        stdout, stderr = process.communicate(timeout=5)
        self.assertNotEqual(process.returncode, 0)
        self.assertEqual(json.loads(stderr)["error"]["code"], "provider_revision_conflict")
        self.assertEqual(stdout, b"")
        self.assertEqual(self.store.read_bytes(), saved)

    def test_14_explicit_provider_test_has_one_attempt_and_no_persistent_write(self):
        self.add()
        before = self.store.read_bytes()
        count = len(self.upstream.records)
        result = self.write("test")
        self.assertTrue(result["operation"]["ok"])
        self.assertEqual(result["operation"]["kind"], "test")
        self.assertEqual(len(self.upstream.records), count + 1)
        self.assertEqual(self.store.read_bytes(), before)
        self.assertEqual(self.upstream.records[-1]["body"]["max_tokens"], 1)

    def test_15_input_unknown_secrets_duplicate_fields_and_limit_leave_store_unchanged(self):
        self.add()
        before = self.store.read_bytes()
        for raw in [b'{"api_key":"' + SECRETS["MAGPIE_FIXTURE_FIRST"].encode() + b'"}',
                    b'{"name":"a","name":"b"}', b'{"name":"a","n\\u0061me":"b"}',
                    b'{"name":"' + b'x' * (128 * 1024) + b'"}']:
            with self.subTest(length=len(raw)):
                self.invoke("edit", "fixture", expect=self.snapshot["token"], raw=raw, ok=False)
                self.assertEqual(self.store.read_bytes(), before)

    def test_16_host_origin_proxy_and_protocol_guards_precede_upstream(self):
        self.add()
        self.start_gateway()
        for headers, expected in [({"Origin": "null"}, 403), ({"Host": "untrusted.example"}, 403),
                                  ({"X-Forwarded-For": "127.0.0.1"}, 403), ({"Upgrade": "websocket"}, 426)]:
            status, _, _ = self.request("POST", "/v1/chat/completions", {"model": "fixture/alpha"}, headers)
            self.assertEqual(status, expected)
        self.assertEqual(self.upstream.records, [])

    def test_17_paused_or_unsupported_provider_actions_do_not_contact_network(self):
        self.add(protocol="messages")
        report = self.write("refresh")
        self.assertFalse(report["operation"]["ok"])
        self.assertEqual(report["operation"]["code"], "provider_protocol_not_integrated")
        self.assertEqual(report["providers"][0]["catalog_error"], "not_supported")
        self.assertEqual(self.upstream.records, [])
        self.write("edit", data={"enabled": False})
        result = self.invoke("refresh", "fixture", expect=self.snapshot["token"], ok=False)
        self.assertEqual(json.loads(result.stderr)["error"]["code"], "provider_disabled")
        self.assertEqual(self.upstream.records, [])

    @unittest.skipUnless(os.name == "posix", "broken-pipe acceptance currently runs on POSIX")
    def test_18_lost_mutation_stdout_is_not_proof_that_write_failed(self):
        self.add()
        old_revision = self.snapshot["revision"]
        command = [str(BINARY), "providers", "edit", "fixture", "--store", str(self.store),
                   "--expect", self.snapshot["token"], "--stdin", "--json"]
        process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, env=self.env)
        # The caller disappears before output, after supplying an authorized edit.
        # Broken stdout causes a nonzero process result even though the core commit
        # completed. Consumers must reconcile, never assume no write happened.
        process.stdout.close()
        process.stdin.write(encoded({"name": "Committed despite lost reply"}))
        process.stdin.close()
        process.wait(timeout=5)
        stderr = process.stderr.read()
        process.stderr.close()
        self.assertNotEqual(process.returncode, 0)
        for secret in SECRETS.values():
            self.assertNotIn(secret.encode(), stderr)
        current = self.cli("list")
        self.assertEqual(current["revision"], old_revision + 1)
        self.assertEqual(current["providers"][0]["name"], "Committed despite lost reply")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--report", type=Path)
    options = parser.parse_args()
    BINARY = options.binary.resolve()
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(ProviderTests)
    start = time.monotonic()
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    report = {"scope": "real_cli_core_persistence_live_gateway_synthetic_loopback_upstream",
              "binary_sha256": hashlib.sha256(BINARY.read_bytes()).hexdigest(),
              "tests_run": result.testsRun, "failures": len(result.failures), "errors": len(result.errors),
              "skipped": len(result.skipped), "duration_seconds": round(time.monotonic() - start, 3),
              "passed": result.wasSuccessful(), "native_gpui": "not_run", "external_accounts": "not_run",
              "external_tls": "not_run", "four_protocol_conversion": "not_run_host_owned",
              "complete_product_acceptance": False}
    if options.report:
        options.report.parent.mkdir(parents=True, exist_ok=True)
        options.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(report, ensure_ascii=False))
    raise SystemExit(0 if result.wasSuccessful() else 1)
