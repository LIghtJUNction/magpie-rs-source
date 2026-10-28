#!/usr/bin/env python3
"""Small synthetic controls for forwarding.py; no Rust build or accounts."""

import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
import forwarding as bench


class MutatingWriter:
    def __init__(self, writer, mode):
        self.writer, self.mode = writer, mode
        self.pending = bytearray()

    def __getattr__(self, name):
        return getattr(self.writer, name)

    def write(self, raw):
        original_size = len(raw)
        if self.mode == "bad-json" and raw == bench.CHAT:
            raw = raw[:-2] + b"!!"
        elif self.mode == "bad-event" and raw == bench.DELTA:
            raw = raw.replace(b"xxxxx", b"yxxxx", 1)
        elif self.mode == "missing-done" and raw == bench.FINISH + bench.DONE:
            raw = bench.FINISH
        elif self.mode == "extra-event" and raw == bench.FINISH + bench.DONE:
            raw = bench.DELTA + raw
        elif self.mode == "buffered":
            if raw == bench.DELTA:
                self.pending.extend(raw)
                return original_size
            if raw == bench.FINISH + bench.DONE:
                raw = bytes(self.pending) + raw
                self.pending.clear()
        self.writer.write(raw)
        return original_size


def mutated_handler(mode):
    class Mutated(bench.Handler):
        def setup(self):
            super().setup()
            self.wfile = MutatingWriter(self.wfile, mode)
    return Mutated


class Reject(bench.Handler):
    code = "gateway_concurrency_limit"

    def do_POST(self):
        self.connection.settimeout(bench.IO_TIMEOUT)
        self.rfile.read(int(self.headers["Content-Length"]))
        raw = bench.encoded({"error": {"code": self.code}})
        self.send_response(503)
        self.send_header("Content-Length", str(len(raw)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(raw)
        self.close_connection = True


class Controls(unittest.TestCase):
    @contextlib.contextmanager
    def provider(self, handler=bench.Handler):
        server = bench.Provider()
        server.RequestHandlerClass = handler
        server.thread.start()
        try:
            yield server, ("127.0.0.1", server.server_port)
        finally:
            server.stop.set()
            server.shutdown()
            server.server_close()
            server.thread.join(timeout=bench.IO_TIMEOUT)
            self.assertEqual(server.active, 0)

    def test_fixture_and_valid_direct_delivery(self):
        self.assertEqual(len(bench.CHAT), 65536)
        for raw in (bench.DELTA, bench.FINISH):
            self.assertEqual(len(raw), 16384)
            self.assertTrue(json.loads(raw[6:-2])["choices"][0]["delta"]["content"])
        fixture = bench.Fixture(.2)
        with self.provider() as (server, address):
            for stream in (False, True):
                result = bench.exchange(server, address, False, fixture, stream)
                self.assertEqual(result["outcome"], "success", result)
                self.assertTrue(result["provider_started"])
                if stream:
                    self.assertEqual(result["sha256"], fixture.stream_hash)
                    self.assertLess(result["first_event_ms"], result["total_ms"])
            cancelled = bench.cancellation_probe(server, address, False, bench.Fixture(2))
            self.assertEqual(cancelled["cancelled"][0]["outcome"], "cancelled")

    def test_wrong_body_corrupt_event_missing_terminal_extra_and_buffered(self):
        for mode in ("bad-json", "bad-event", "missing-done", "extra-event", "buffered"):
            with self.subTest(mode=mode), self.provider(mutated_handler(mode)) as (server, address):
                result = bench.exchange(server, address, False, bench.Fixture(.2),
                                        mode != "bad-json")
                self.assertEqual(result["outcome"], "error", result)
                if mode == "buffered":
                    self.assertIn("first_event_not_before_provider_terminal", result["error"])

    def test_rejections_never_become_valid_work(self):
        with self.provider(Reject) as (server, address):
            result = bench.run_case(server, address, True, bench.Fixture(.2), False, 4, 0, True)
            self.assertEqual(result["offered"], 4)
            self.assertEqual(result["counts"]["rejected"], 4)
            self.assertEqual(result["counts"]["success"], 0)
            self.assertEqual(result["counts"]["provider_started"], 0)
            self.assertEqual(result["valid_requests_per_second"], 0)
            self.assertEqual(result["valid_payload_bytes_per_second"], 0)
            self.assertIsNone(result["total_ms"]["p50"])
        class Other503(Reject):
            code = "upstream_error"
        with self.provider(Other503) as (server, address):
            result = bench.exchange(server, address, True, bench.Fixture(.2), False)
            self.assertEqual(result["outcome"], "error")

    def test_startup_deadline_and_capped_stderr_without_eof(self):
        with tempfile.TemporaryDirectory(prefix="magpie-forwarding-control-", dir="/tmp") as temporary:
            root = Path(temporary)
            executable = root / "silent-gateway"
            executable.write_text(
                "#!/usr/bin/python3\nimport os,time\n"
                "os.write(2,b'x'*262144)\ntime.sleep(60)\n", encoding="ascii")
            executable.chmod(0o700)
            gateway = bench.Gateway(executable, root / "unused.json", {"HOME": str(root)})
            started = time.monotonic()
            try:
                with self.assertRaisesRegex(RuntimeError, "gateway_startup_deadline"):
                    gateway.start()
            finally:
                cleanup = gateway.close()
            self.assertLess(time.monotonic() - started, 8)
            self.assertIsNotNone(cleanup["returncode"])
            self.assertEqual(len(gateway.output["stderr"]), 4096)
            self.assertTrue(gateway.process.stdout.closed)
            self.assertTrue(gateway.process.stderr.closed)
            with self.assertRaises(ProcessLookupError):
                os.kill(gateway.process.pid, 0)

    def test_auxiliary_command_caps_both_pipes(self):
        for descriptor, limit, key in ((1, 8192, "stdout"), (2, 1024, "stderr")):
            with self.subTest(pipe=key):
                result = bench.command([sys.executable, "-B", "-c",
                                        f"import os;os.write({descriptor},b'x'*32768)"])
                self.assertEqual(result["error"], "command_output_limit")
                self.assertEqual(len(result[key]), limit)

    def test_auxiliary_command_deadline_and_inherited_pipe(self):
        started = time.monotonic()
        result = bench.command([sys.executable, "-B", "-c",
                                "import os,time\nif os.fork()==0: time.sleep(10)"], timeout=.2)
        self.assertEqual(result["error"], "command_deadline")
        self.assertLess(time.monotonic() - started, 2)

    def test_auxiliary_command_preserves_exit_and_small_output(self):
        result = bench.command([sys.executable, "-B", "-c",
                                "import sys;print('ok');sys.stderr.write('detail');sys.exit(7)"])
        self.assertEqual(result, {"exit": 7, "stdout": "ok", "stderr": "detail"})

    def test_result_paths_reject_repositories_and_symlink_aliases(self):
        with tempfile.TemporaryDirectory(prefix="magpie-forwarding-path-control-", dir="/tmp") as directory:
            root = Path(directory)
            repository = root / "recipe"
            repository.mkdir()
            subprocess.run(["git", "init", "--quiet", str(repository)], check=True)
            alias = root / "alias"
            alias.symlink_to(repository, target_is_directory=True)
            outside = root / "outside"
            outside.mkdir()
            (repository / "external").symlink_to(outside, target_is_directory=True)
            for output in (repository / "result.json", alias / "result.json",
                           repository / "external/result.json"):
                args = ["forwarding.py", "--binary", sys.executable, "--output", str(output)]
                with self.subTest(output=output), patch.object(sys, "argv", args), \
                        contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                    bench.parse_options()
            self.assertEqual(list(outside.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
