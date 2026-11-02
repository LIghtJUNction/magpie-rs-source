#!/usr/bin/env python3
"""Additive Gateway identity/capability inspection over real loopback HTTP/CLI.

The facade supplies malformed/drifting local responses; it never uses an account.
"""
import argparse
import copy
import hashlib
import http.server
import json
from pathlib import Path
import shutil
import subprocess
import threading
import time
import unittest

import provider_loop as fixture
from first_loop import CandidateProcess


FOUR_PROTOCOL = False


class Facade(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        self.server.requests.append(self.path)
        value = self.server.reply(self.path)
        body = value if isinstance(value, bytes) else fixture.encoded(value)
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass


class InspectionTests(fixture.ProviderTests):
    def inspect_command(self, address=None, store=None, expected_error=None):
        result = subprocess.run([str(fixture.BINARY), "gateway", "provider-inspect",
            "--store", str(store or self.store), "--listen", address or self.address, "--json"],
            env=self.env, capture_output=True, timeout=7)
        for secret in fixture.SECRETS.values():
            self.assertNotIn(secret.encode(), result.stdout + result.stderr)
        if expected_error:
            self.assertEqual(result.returncode, 2)
            self.assertEqual(result.stdout, b"")
            self.assertEqual(json.loads(result.stderr)["error"]["code"], expected_error)
            return None
        self.assertEqual(result.returncode, 0, result.stderr.decode("utf-8", "replace"))
        value = json.loads(result.stdout)
        self.assertEqual(set(value), {"version", "provider_gateway"})
        self.assertEqual(value["version"], "0.2.0")
        self.assertEqual(set(value["provider_gateway"]),
            {"address", "mode", "store_identity", "revision", "token", "provider_health", "models"})
        return value["provider_gateway"]

    def metadata(self):
        status, _, body = self.request("GET", "/health")
        self.assertEqual(status, 200)
        health = json.loads(body)
        status, _, body = self.request("GET", "/v1/models")
        self.assertEqual(status, 200)
        return health, json.loads(body)

    def facade(self, reply):
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Facade)
        server.daemon_threads = True
        server.requests = []
        server.reply = reply
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        return f"127.0.0.1:{server.server_port}"

    def test_live_protocol_metadata_and_stable_store_identity(self):
        for protocol in ("chat", "responses", "messages", "gemini"):
            self.add(provider=protocol, protocol=protocol, models=["model@2026"])
        self.start_gateway()
        observed = self.inspect_command()
        self.assertEqual(observed["token"], self.snapshot["token"])
        self.assertEqual(observed["revision"], self.snapshot["revision"])
        self.assertEqual(observed["mode"], "provider_store")
        self.assertEqual(observed["provider_health"], "not_probed")
        self.assertRegex(observed["store_identity"], r"^provider-store-v1:[0-9a-f]{64}$")
        for row in observed["models"]:
            protocol = row["id"].split("/", 1)[0]
            native = [protocol] if FOUR_PROTOCOL or protocol in ("chat", "responses") else []
            self.assertEqual(row["native_endpoints"], native)
            self.assertEqual(row["client_endpoints"], ["chat", "responses", "messages", "gemini"] if FOUR_PROTOCOL else native)
        identity = observed["store_identity"]
        self.write("edit", "chat", {"name": "Renamed without changing identity"})
        fresh = self.inspect_command()
        self.assertEqual(fresh["store_identity"], identity)
        self.assertEqual(fresh["token"], self.snapshot["token"])
        self.assertNotEqual(fresh["token"], observed["token"])
        self.assertEqual(self.upstream.records, [])

    def test_same_bytes_and_revision_at_other_store_is_mismatch(self):
        self.add(models=["model@2026"])
        self.start_gateway()
        other = self.root / "other" / "providers.toml"
        other.parent.mkdir(mode=0o700)
        shutil.copyfile(self.store, other)
        other.chmod(0o600)
        self.assertEqual(other.read_bytes(), self.store.read_bytes())
        self.inspect_command(store=other, expected_error="provider_store_mismatch")

    def test_catalog_change_during_inspection_is_rejected(self):
        self.add(models=["model@2026"])
        self.start_gateway()
        health, catalog = self.metadata()
        def reply(path):
            if path == "/health":
                return health
            self.write("edit", "fixture", {"name": "Changed after inspection read"})
            return catalog
        address = self.facade(reply)
        self.inspect_command(address=address, expected_error="provider_catalog_changed")

    def test_confirmed_legacy_forward_is_the_only_unmanaged_response(self):
        config = self.root / "legacy.json"
        config.write_text(json.dumps({"version": 1, "provider_id": "legacy", "base_url": self.base_url,
                                      "models": ["plain"]}))
        process = CandidateProcess([str(fixture.BINARY), "serve", "--listen", "127.0.0.1:0",
                                    "--forward", str(config)], self.env)
        self.addCleanup(process.close)
        address = process.wait_for_bind()
        self.inspect_command(address=address, expected_error="provider_gateway_unmanaged")
        result = subprocess.run([str(fixture.BINARY), "gateway", "inspect", "--listen", address, "--json"],
                                env=self.env, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(json.loads(result.stdout)["gateway"]["models"],
                         [{"id": "legacy/plain", "source": "configured"}])

    def test_partial_unknown_and_duplicate_health_never_fall_back(self):
        self.add()
        self.start_gateway()
        health, catalog = self.metadata()
        variants = []
        for change in ({"state_source": "unknown"}, {"state_source": None}, {"mode": "unknown"},
                       {"limits": {}}, {"store_identity": "not-a-path-hash"}):
            modified = copy.deepcopy(health)
            modified.update(change)
            variants.append(modified)
        partial = copy.deepcopy(health)
        del partial["state_source"]
        variants.append(partial)
        raw = fixture.encoded(health)
        variants.append(raw[:-1] + b',"state_sourc\\u0065":"provider_store"}')
        for health_response in variants:
            with self.subTest(variant=variants.index(health_response)):
                address = self.facade(lambda path, h=health_response: h if path == "/health" else catalog)
                self.inspect_command(address=address, expected_error="provider_gateway_invalid_response")

    def test_inconsistent_catalog_rows_and_capabilities_are_rejected(self):
        self.add(models=["model@2026", "plain"])
        self.start_gateway()
        health, original = self.metadata()
        variants = []
        duplicate = copy.deepcopy(original)
        duplicate["data"][1] = copy.deepcopy(duplicate["data"][0])
        variants.append(duplicate)
        for field, value in [("native_endpoints", ["responses"]), ("client_endpoints", ["invented"]),
                             ("client_endpoints", ["chat", "chat"]), ("client_endpoints", [])]:
            changed = copy.deepcopy(original)
            changed["data"][0]["magpie"][field] = value
            variants.append(changed)
        changed = copy.deepcopy(original)
        changed["data"][0]["id"] = "fixture/not-configured@2026"
        variants.append(changed)
        for catalog in variants:
            address = self.facade(lambda path, c=catalog: health if path == "/health" else c)
            self.inspect_command(address=address, expected_error="provider_gateway_invalid_response")

    def test_empty_eligible_catalog_and_bounded_response(self):
        self.start_gateway()
        self.assertEqual(self.inspect_command()["models"], [])
        self.assertFalse(self.store.exists())
        health, catalog = self.metadata()
        address = self.facade(lambda path: b" " * (64 * 1024 + 1) if path == "/health" else catalog)
        self.inspect_command(address=address, expected_error="provider_gateway_invalid_response")
        address = self.facade(lambda path: health if path == "/health" else b" " * (2 * 1024 * 1024 + 1))
        self.inspect_command(address=address, expected_error="provider_gateway_invalid_response")

    def test_overall_deadline_and_bad_arguments_are_bounded(self):
        self.add()
        self.start_gateway()
        health, catalog = self.metadata()
        def reply(path):
            time.sleep(2.65)
            return health if path == "/health" else catalog
        address = self.facade(reply)
        start = time.monotonic()
        self.inspect_command(address=address, expected_error="provider_gateway_inspect_timeout")
        self.assertLess(time.monotonic() - start, 6.3)
        for address in ("127.0.0.1:0", "192.0.2.1:3333"):
            self.inspect_command(address=address, expected_error="provider_inspect_argument_invalid")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--four-protocol", action="store_true")
    args = parser.parse_args()
    fixture.BINARY = args.binary.resolve()
    FOUR_PROTOCOL = args.four_protocol
    names = [name for name in InspectionTests.__dict__ if name.startswith("test_")]
    start = time.monotonic()
    result = unittest.TextTestRunner(verbosity=2).run(unittest.TestSuite(InspectionTests(name) for name in names))
    report = {"scope": "additive_typed_gateway_provider_inspect_real_cli_loopback",
              "binary_sha256": hashlib.sha256(fixture.BINARY.read_bytes()).hexdigest(),
              "four_protocol_merge": FOUR_PROTOCOL, "tests_run": result.testsRun,
              "failures": len(result.failures), "errors": len(result.errors), "skipped": len(result.skipped),
              "passed": result.wasSuccessful(), "duration_seconds": round(time.monotonic() - start, 3),
              "external_accounts": "not_run", "gui_acceptance": "not_run"}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(report, ensure_ascii=False))
    raise SystemExit(0 if result.wasSuccessful() else 1)
