import argparse, hashlib, http.client, importlib.util, json, time
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument("--binary", type=Path, required=True)
parser.add_argument("--report", type=Path, required=True)
parser.add_argument("--iterations", type=int, default=20)
options = parser.parse_args()
spec = importlib.util.spec_from_file_location("first_loop", Path(__file__).parent / "first_loop.py")
f = importlib.util.module_from_spec(spec)
spec.loader.exec_module(f)
f.BINARY = options.binary.resolve()
fixture = f.FixtureTests
case = fixture("test_06_error_bodies_and_sse_errors_never_leak")
start = time.monotonic()
results = []
try:
    fixture.setUpClass()
    before = json.loads(case.request("GET", "/health")[2])["transport_counters"]
    records_before = len(f.Provider.records)
    for iteration in range(options.iterations):
        for path in ("/v1/chat/completions", "/v1/responses"):
            for model in ("sse-error", "sse-event-error", *f.FRAMED_ERRORS):
                for expect_continue in (False, True):
                    result = {"iteration": iteration, "path": path, "model": model, "expect_continue": expect_continue}
                    connection = http.client.HTTPConnection(fixture.host, fixture.port, timeout=4)
                    stage = "headers"
                    try:
                        payload = f.encoded({"model": "local/"+model, "stream": True, "messages": []})
                        if expect_continue:
                            connection.putrequest("POST", path)
                            connection.putheader("Content-Type", "application/json")
                            connection.putheader("Content-Length", str(len(payload)))
                            connection.putheader("Expect", "100-continue")
                            connection.endheaders()
                            interim = bytearray()
                            while not interim.endswith(b"\r\n\r\n"):
                                byte = connection.sock.recv(1)
                                assert byte and len(interim) < 8192, "incomplete interim response"
                                interim.extend(byte)
                            assert interim.startswith(b"HTTP/1.1 100 Continue\r\n"), "unexpected interim response"
                            connection.send(payload)
                        else:
                            connection.request("POST", path, payload, {"Content-Type": "application/json"})
                        response = connection.getresponse()
                        assert response.status == 200, "wrong response status"
                        assert response.getheader("Content-Type") == "text/event-stream", "wrong content type"
                        assert response.getheader("X-Magpie-Provider") == "local", "wrong attribution"
                        assert response.getheader("X-Magpie-Model") == "local/"+model, "wrong model attribution"
                        stage = "body"
                        try:
                            body = response.read()
                        except http.client.IncompleteRead as error:
                            assert error.partial == b"", "rejected event bytes escaped"
                            result["outcome"] = "complete_headers_incomplete_empty_body"
                        else:
                            raise AssertionError("stream error became clean EOF")
                    except Exception as error:
                        result["outcome"] = "unexpected_"+stage+"_"+type(error).__name__
                    finally:
                        connection.close()
                    results.append(result)
                    case.wait_active(0)
    after = json.loads(case.request("GET", "/health")[2])["transport_counters"]
    deltas = {name: after[name]-before[name] for name in ("completed", "failed", "cancelled", "active")}
    count = len(results)
    expected = "complete_headers_incomplete_empty_body"
    failures = [row for row in results if row["outcome"] != expected]
    report = {"binary_sha256": hashlib.sha256(options.binary.read_bytes()).hexdigest(), "cases": count, "iterations": options.iterations, "unexpected": failures, "counter_deltas": deltas, "upstream_attempts": len(f.Provider.records)-records_before, "passed": not failures and deltas == {"completed": 0, "failed": count, "cancelled": 0, "active": 0} and len(f.Provider.records)-records_before == count, "duration_seconds": round(time.monotonic()-start,3), "external_accounts": "not_used"}
finally:
    fixture.doClassCleanups()
options.report.write_text(json.dumps(report,indent=2)+"\n")
print(json.dumps(report))
raise SystemExit(not report["passed"])
