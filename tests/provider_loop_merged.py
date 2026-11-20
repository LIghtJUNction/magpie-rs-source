#!/usr/bin/env python3
"""Replay all Provider regressions with the explicit new cross-protocol behavior.

The original provider_loop.py is unchanged. Only its old 'Responses->Chat is
501' assertion is replaced here by a real successful translated request.
"""
import argparse
import hashlib
import json
from pathlib import Path
import time
import unittest

import provider_loop as fixture


class MergedProviderTests(fixture.ProviderTests):
    def test_09_native_chat_stream_and_responses_preserve_payloads(self):
        self.add()
        self.add("native-responses", protocol="responses")
        self.start_gateway()
        status, _, body = self.generate("fixture/stream", stream=True)
        self.assertEqual((status, body), (200, fixture.STREAM))
        status, headers, body = self.generate("native-responses/alpha", path="/v1/responses", input="保留输入")
        self.assertEqual((status, json.loads(body)["status"]), (200, "completed"))
        self.assertEqual(headers["x-magpie-provider"], "native-responses")
        self.assertEqual(self.upstream.records[-1]["body"]["input"], "保留输入")
        before = len(self.upstream.records)
        status, headers, body = self.request("POST", "/v1/responses",
            {"model": "fixture/alpha", "input": "真实转换请求", "stream": False})
        self.assertEqual(status, 200, body)
        self.assertEqual(json.loads(body)["status"], "completed")
        self.assertEqual(headers["x-magpie-provider-key"], "key-first")
        self.assertEqual(len(self.upstream.records), before + 1)
        self.assertEqual(self.upstream.records[-1]["path"], "/v1/chat/completions")
        self.assertEqual(self.upstream.records[-1]["body"]["model"], "alpha")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    fixture.BINARY = args.binary.resolve()
    start = time.monotonic()
    result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(MergedProviderTests))
    report = {"scope": "provider_regressions_with_explicit_host_conversion_integration",
              "binary_sha256": hashlib.sha256(fixture.BINARY.read_bytes()).hexdigest(),
              "tests_run": result.testsRun, "failures": len(result.failures), "errors": len(result.errors),
              "skipped": len(result.skipped), "passed": result.wasSuccessful(),
              "duration_seconds": round(time.monotonic() - start, 3),
              "original_provider_loop_modified": False,
              "intentional_changed_assertion": "test09 Responses->Chat now executes one translated request instead of returning 501",
              "messages_gemini_discovery": "still_not_integrated", "external_accounts": "not_run"}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(report, ensure_ascii=False))
    raise SystemExit(0 if result.wasSuccessful() else 1)
