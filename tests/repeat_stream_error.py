import argparse, hashlib, importlib.util, json, time, unittest
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument("--binary", type=Path, required=True)
parser.add_argument("--output", type=Path, required=True)
parser.add_argument("--iterations", type=int, default=30)
options = parser.parse_args()
if options.iterations <= 0:
    parser.error("--iterations must be positive")
spec = importlib.util.spec_from_file_location("first_loop", Path(__file__).parent / "first_loop.py")
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)
fixture.BINARY = options.binary.resolve()
suite = unittest.TestSuite(fixture.FixtureTests("test_06_error_bodies_and_sse_errors_never_leak") for _ in range(options.iterations))
start = time.monotonic()
result = unittest.TextTestRunner(verbosity=1).run(suite)
report = {"binary_sha256": hashlib.sha256(options.binary.read_bytes()).hexdigest(), "iterations": options.iterations, "tests_run": result.testsRun, "failures": len(result.failures), "errors": len(result.errors), "duration_seconds": round(time.monotonic()-start,3), "error_kinds": {}, "passed": result.wasSuccessful()}
for _, error in result.errors + result.failures:
    last = error.strip().splitlines()[-1].split(":",1)[0]
    report["error_kinds"][last] = report["error_kinds"].get(last,0)+1
options.output.write_text(json.dumps(report,indent=2)+"\n")
print(json.dumps(report))
raise SystemExit(not result.wasSuccessful())
