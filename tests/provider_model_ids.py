#!/usr/bin/env python3
"""Real Provider/catalog/legacy-inspect regression for the shared model grammar.

All state, credentials and upstream responses are synthetic and temporary.
The historical --forward configuration grammar remains intentionally unchanged.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import time
import unittest

import provider_loop as fixture


class ModelIdTests(fixture.ProviderTests):
    def inspect(self):
        result = subprocess.run(
            [str(fixture.BINARY), "gateway", "inspect", "--listen", self.address, "--json"],
            env=self.env, capture_output=True, timeout=5,
        )
        self.assertEqual(result.returncode, 0, result.stderr.decode("utf-8", "replace"))
        value = json.loads(result.stdout)
        self.assertEqual(set(value), {"version", "gateway"})
        self.assertEqual(set(value["gateway"]), {"address", "mode", "provider_health", "models"})
        self.assertTrue(all(set(row) == {"id", "source"} for row in value["gateway"]["models"]))
        return [row["id"] for row in value["gateway"]["models"]]

    def test_mixed_manual_and_discovered_ids_round_trip_live(self):
        models = ["plain", "model@2026", "family/model:v1", "M_1.2-3@2026/sub:id", "m" * 256]
        self.add(models=models)
        self.start_gateway()
        expected = ["fixture/" + model for model in models]
        self.assertEqual(self.inspect(), expected)
        status, _, body = self.request("GET", "/v1/models")
        self.assertEqual(status, 200)
        self.assertEqual([row["id"] for row in json.loads(body)["data"]], expected)
        for model in models:
            status, _, body = self.generate("fixture/" + model)
            self.assertEqual(status, 200, body)
            self.assertEqual(self.upstream.records[-1]["body"]["model"], model)
        self.upstream.catalog = ["still-plain", "fresh@2026", "nested/vendor:model@1"]
        refreshed = self.write("refresh")
        self.assertTrue(refreshed["operation"]["ok"])
        self.assertEqual(self.inspect(), ["fixture/" + model for model in self.upstream.catalog])
        long_provider = "p" * 128
        self.add(provider=long_provider, models=["model@2026"])
        self.assertIn(long_provider + "/model@2026", self.inspect())

    def test_invalid_ids_rejected_without_changing_live_catalog(self):
        self.add(models=["plain", "model@2026"])
        self.start_gateway()
        before = self.snapshot["token"]
        for invalid in ["@start", "a//b", "a/./b", "a/../b", "a?query", "a#fragment", "a b", "m" * 257]:
            result = self.invoke("models", "fixture", data={"models": ["plain", invalid],
                "selected_models": [], "auto_include": True}, expect=before, ok=False)
            self.assertEqual(json.loads(result.stderr)["error"]["code"], "provider_invalid_or_duplicate_model")
            self.assertEqual(self.cli("list")["token"], before)
            self.assertEqual(self.inspect(), ["fixture/plain", "fixture/model@2026"])

    def test_legacy_forward_config_keeps_its_original_grammar(self):
        path = self.root / "legacy-forward.json"
        path.write_text(json.dumps({"version": 1, "provider_id": "legacy", "base_url": self.base_url,
                                    "models": ["model@2026"]}))
        result = subprocess.run([str(fixture.BINARY), "serve", "--forward", str(path),
                                 "--listen", "127.0.0.1:0"], env=self.env, capture_output=True, timeout=5)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"forward_config_invalid_model", result.stderr)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--report", required=True, type=Path)
    args = parser.parse_args()
    fixture.BINARY = args.binary.resolve()
    names = ["test_mixed_manual_and_discovered_ids_round_trip_live",
             "test_invalid_ids_rejected_without_changing_live_catalog",
             "test_legacy_forward_config_keeps_its_original_grammar"]
    start = time.monotonic()
    result = unittest.TextTestRunner(verbosity=2).run(unittest.TestSuite(ModelIdTests(name) for name in names))
    report = {"scope": "synthetic_real_cli_core_live_tcp_shared_model_grammar",
              "binary_sha256": hashlib.sha256(fixture.BINARY.read_bytes()).hexdigest(),
              "tests_run": result.testsRun, "failures": len(result.failures), "errors": len(result.errors),
              "skipped": len(result.skipped), "passed": result.wasSuccessful(),
              "duration_seconds": round(time.monotonic() - start, 3),
              "legacy_inspect_envelope_unchanged": True, "external_provider": "not_run"}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(report, ensure_ascii=False))
    raise SystemExit(0 if result.wasSuccessful() else 1)
