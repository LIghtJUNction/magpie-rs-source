#!/usr/bin/env python3
"""Focused legacy SSE failure boundary acceptance; original suite is unchanged.

Hyper can reject the first invalid event before response headers are flushed,
or after HTTP 200 has been sent. Both must fail without releasing invalid event
bytes, leaking credentials, or incrementing the success counter. This runner
checks those semantic guarantees against the same isolated legacy fixture.
"""
import argparse
import hashlib
import http.client
import json
from pathlib import Path
import time
import unittest

import first_loop as fixture


BOUNDARIES = {"before_headers": 0, "after_headers": 0}


class BoundaryTests(fixture.FixtureTests):
    def assert_failed_first_event(self, model, path):
        _, _, before = self.request("GET", "/health")
        count = len(fixture.Provider.records)
        connection = http.client.HTTPConnection(self.host, self.port, timeout=4)
        payload = fixture.encoded({"model": "local/" + model, "stream": True, "messages": []})
        partial = b""
        try:
            connection.request("POST", path, payload, {"Content-Type": "application/json"})
            try:
                response = connection.getresponse()
            except http.client.RemoteDisconnected:
                BOUNDARIES["before_headers"] += 1
            else:
                self.assertEqual(response.status, 200)
                with self.assertRaises(http.client.IncompleteRead) as caught:
                    response.read()
                partial = caught.exception.partial
                BOUNDARIES["after_headers"] += 1
        finally:
            connection.close()
        self.assertEqual(partial, b"", "an invalid first event must release no payload prefix")
        self.assertNotIn(fixture.SECRET.encode(), partial)
        self.assertNotIn(b"[DONE]", partial)
        self.wait_active(0)
        _, _, after = self.request("GET", "/health")
        before = json.loads(before)["transport_counters"]
        after = json.loads(after)["transport_counters"]
        self.assertEqual(len(fixture.Provider.records), count + 1, "exactly one upstream attempt")
        self.assertEqual(after["completed"], before["completed"], "invalid first event is never success")
        self.assertEqual(after["failed"], before["failed"] + 1, "the parser records the failure")

    def test_malformed_first_event_fails_at_either_header_boundary(self):
        for attempt in range(8):
            with self.subTest(attempt=attempt):
                self.assert_failed_first_event("bad-choices", "/v1/chat/completions")

    def test_error_events_bom_cr_and_split_transport_never_release_secrets(self):
        for path in ("/v1/chat/completions", "/v1/responses"):
            for model in ("sse-error", "sse-event-error", *fixture.FRAMED_ERRORS):
                with self.subTest(path=path, model=model):
                    self.assert_failed_first_event(model, path)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--report", type=Path)
    options = parser.parse_args()
    fixture.BINARY = options.binary.resolve()
    # Select only the focused new tests; inherited legacy test names remain in
    # their original suite, whose timing assumption is reported separately.
    names = ["test_malformed_first_event_fails_at_either_header_boundary",
             "test_error_events_bom_cr_and_split_transport_never_release_secrets"]
    suite = unittest.TestSuite(BoundaryTests(name) for name in names)
    start = time.monotonic()
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    report = {"scope": "legacy_first_sse_event_failure_transport_boundaries",
              "binary_sha256": hashlib.sha256(fixture.BINARY.read_bytes()).hexdigest(),
              "tests_run": result.testsRun, "failure_requests": 24,
              "failures": len(result.failures), "errors": len(result.errors),
              "passed": result.wasSuccessful(), "observed_transport_boundaries": BOUNDARIES,
              "duration_seconds": round(time.monotonic() - start, 3),
              "original_suite_modified": False, "native_gpui": "not_run", "external_provider": "not_run"}
    if options.report:
        options.report.parent.mkdir(parents=True, exist_ok=True)
        options.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(report, ensure_ascii=False))
    raise SystemExit(0 if result.wasSuccessful() else 1)
