#!/usr/bin/env python3
"""Real desktop ActionsState -> CLI -> gateway -> installed Codex acceptance.

Pass a GUI libtest executable built by the parent with cargo test --no-run.
Runs one ignored worker in each of two private homes, without opening a window.
"""
import argparse
import hashlib
import http.client
import http.server
import importlib.util
import ipaddress
import json
import os
from pathlib import Path
import re
import secrets
import selectors
import signal
import subprocess
import sys
import tempfile
import threading
import time


WORKER = "actions_state::tests::real_cli_native_codex_roundtrip"
SOURCE = Path(__file__).resolve().parents[1]
ORIGINAL = (b'# isolated fixture, preserve bytes\n'
            b'model = "before-model" # user comment\n'
            b'model_reasoning_effort = "low"\n')
SOURCE_INPUTS = (
    "Cargo.toml", "Cargo.lock", "apps/desktop/Cargo.toml",
    "apps/desktop/Cargo.lock", "apps/desktop/src/lib.rs",
    "apps/desktop/src/actions_state.rs", "apps/desktop/src/cli_bridge.rs",
    "apps/desktop/src/config_stamp.rs", "apps/desktop/src/snapshot.rs",
    "crates/magpie-cli/src/main.rs", "crates/magpie-core/src/agents.rs",
    "crates/magpie-core/src/codex_transaction.rs",
    "crates/magpie-service/src/forward.rs", "tests/native_codex.py",
    "tests/desktop_actions.py",
)


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def load_provider_fixture():
    path = SOURCE / "tests/native_codex.py"
    spec = importlib.util.spec_from_file_location("magpie_native_codex_fixture", path)
    module = importlib.util.module_from_spec(spec)
    previous = sys.dont_write_bytecode
    try:
        sys.dont_write_bytecode = True
        spec.loader.exec_module(module)
    finally:
        sys.dont_write_bytecode = previous
    return module


class Child:
    """One owned process with nonblocking, capped pipes and bounded reaping."""

    def __init__(self, command, env, cwd, *, stdout_limit=65536, redactions=()):
        self.process = None
        self.selector = selectors.DefaultSelector()
        self.output = {"stdout": bytearray(), "stderr": bytearray()}
        self.limits = {"stdout": stdout_limit, "stderr": 65536}
        self.redactions = tuple(value.encode() for value in redactions)
        self.tails = {"stdout": b"", "stderr": b""}
        self.secret_leaked = False
        self.overflow = False
        self.closed = False
        try:
            self.process = subprocess.Popen(
                command, env=env, cwd=cwd, stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0,
                start_new_session=True)
            for name in self.output:
                pipe = getattr(self.process, name)
                os.set_blocking(pipe.fileno(), False)
                self.selector.register(pipe, selectors.EVENT_READ, name)
        except BaseException:
            self.close()
            raise

    def pump(self, timeout=0):
        for key, _ in self.selector.select(timeout):
            try:
                chunk = os.read(key.fileobj.fileno(), 16384)
            except BlockingIOError:
                continue
            if not chunk:
                self.selector.unregister(key.fileobj)
                continue
            name = key.data
            scan = self.tails[name] + chunk
            self.secret_leaked |= any(secret in scan for secret in self.redactions)
            overlap = max((len(secret) - 1 for secret in self.redactions), default=0)
            self.tails[name] = scan[-overlap:] if overlap else b""
            remaining = self.limits[name] - len(self.output[name])
            self.overflow |= len(chunk) > remaining
            self.output[name].extend(chunk[:remaining])

    def check_output(self):
        if self.overflow:
            raise AssertionError("child output exceeded its capture limit")
        if self.secret_leaked:
            raise AssertionError("child output leaked the synthetic provider key")

    def diagnostics(self):
        result = "; ".join(
            f"{name}={bytes(data[-2000:]).decode('utf-8', 'replace')!r}"
            for name, data in self.output.items())
        for secret in self.redactions:
            result = result.replace(secret.decode(), "<redacted-fixture-key>")
        return result

    def wait(self, timeout):
        deadline = time.monotonic() + timeout
        while True:
            self.pump(min(.02, max(0, deadline - time.monotonic())))
            self.check_output()
            code = self.process.poll()
            if code is not None:
                # Drain already available bytes, never wait for inherited EOF.
                for _ in range(32):
                    self.pump()
                self.check_output()
                return code
            if time.monotonic() >= deadline:
                raise AssertionError("child completion deadline; " + self.diagnostics())

    def wait_for_bind(self, timeout=15):
        deadline = time.monotonic() + timeout
        while True:
            self.pump(min(.02, max(0, deadline - time.monotonic())))
            self.check_output()
            raw = self.output["stdout"]
            if b"\n" in raw:
                line = bytes(raw).split(b"\n", 1)[0].decode("ascii")
                match = re.search(r"\bbound at (127\.0\.0\.1:[0-9]+)(?:\s|$)", line)
                if not match:
                    raise AssertionError("unexpected gateway startup line")
                address = match[1]
                host, port = address.rsplit(":", 1)
                if not ipaddress.ip_address(host).is_loopback or not 0 < int(port) <= 65535:
                    raise AssertionError("invalid gateway bind address")
                return address
            if self.process.poll() is not None:
                raise AssertionError("gateway exited before bind; " + self.diagnostics())
            if time.monotonic() >= deadline:
                raise AssertionError("gateway startup deadline; " + self.diagnostics())

    def close(self):
        if self.closed:
            return
        try:
            if self.process is not None and self.process.poll() is None:
                try:
                    os.killpg(self.process.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    self.process.wait(timeout=1)
                except subprocess.TimeoutExpired:
                    try:
                        os.killpg(self.process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    self.process.wait(timeout=2)
            if self.process is not None:
                for _ in range(8):
                    self.pump()
        finally:
            self.closed = True
            self.selector.close()
            if self.process is not None:
                self.process.stdout.close()
                self.process.stderr.close()


def command_output(command, env, cwd, *, timeout=15, redactions=()):
    child = Child(command, env, cwd, redactions=redactions)
    try:
        if child.wait(timeout):
            raise AssertionError("command failed; " + child.diagnostics())
        return bytes(child.output["stdout"])
    finally:
        child.close()


def publish_marker(root, token, phase, **fields):
    target = root / phase
    temporary = root / f".{phase}.pending"
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "wb") as stream:
        stream.write(json.dumps({"token": token, "phase": phase, **fields}).encode())
    if target.exists():
        raise AssertionError("fixture marker already exists")
    temporary.rename(target)


def read_marker(root, token, phase):
    path = root / phase
    if not path.exists():
        return None
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 4096:
        raise AssertionError("invalid fixture marker file")
    value = json.loads(path.read_bytes())
    if value.get("token") != token or value.get("phase") != phase:
        raise AssertionError("fixture marker identity mismatch")
    return value


def wait_connected(root, token, worker, gateway, timeout=110):
    deadline = time.monotonic() + timeout
    while True:
        worker.pump(.02)
        gateway.pump()
        worker.check_output()
        gateway.check_output()
        if read_marker(root, token, "connected") is not None:
            return
        if worker.process.poll() is not None:
            raise AssertionError("Rust worker exited before connected; " + worker.diagnostics())
        if gateway.process.poll() is not None:
            raise AssertionError("gateway exited while worker was connecting")
        if time.monotonic() >= deadline:
            raise AssertionError("Rust connected marker deadline; " + worker.diagnostics())


def wait_healthy(address, gateway, timeout=10):
    host, port = address.rsplit(":", 1)
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        gateway.pump()
        gateway.check_output()
        if gateway.process.poll() is not None:
            raise AssertionError("gateway exited during readiness")
        connection = http.client.HTTPConnection(host, int(port), timeout=1)
        try:
            connection.request("GET", "/health")
            response = connection.getresponse()
            data = response.read(4097)
            if response.status == 200 and len(data) <= 4096:
                if json.loads(data).get("mode") == "forward_candidate":
                    return
        except (OSError, http.client.HTTPException):
            pass
        finally:
            connection.close()
        time.sleep(.02)
    raise AssertionError("gateway readiness deadline")


def run_case(binary, test_binary, codex, fixture, case):
    started = time.monotonic()
    result = {"case": case, "passed": False, "cleanup_errors": []}
    with tempfile.TemporaryDirectory(prefix="magpie-desktop-actions-") as directory:
        root = Path(directory).resolve(strict=True)
        root.chmod(0o700)
        for name in ("home", "codex", "work", "xdg", "cache", "tmp", "bin"):
            (root / name).mkdir(mode=0o700)
        (root / "bin/codex").symlink_to(codex)
        token = secrets.token_hex(24)
        # Construct child environments from scratch; credentials, user settings,
        # proxy variables and display/session variables never enter the worker.
        env = {"PATH": str(root / "bin") + os.pathsep + os.environ.get("PATH", os.defpath),
               "HOME": str(root / "home"), "CODEX_HOME": str(root / "codex"),
               "XDG_CONFIG_HOME": str(root / "xdg"), "XDG_CACHE_HOME": str(root / "cache"),
               "TMPDIR": str(root / "tmp"), "LANG": "C.UTF-8",
               "MAGPIE_CLI_PATH": str(binary), "MAGPIE_DESKTOP_ACTIONS_ROOT": str(root),
               "MAGPIE_DESKTOP_ACTIONS_TOKEN": token}
        work = root / "work"
        config = root / "codex/config.toml"
        if case == "existing":
            config.write_bytes(ORIGINAL)
            config.chmod(0o600)
        result["original_config_sha256"] = (
            hashlib.sha256(ORIGINAL).hexdigest() if case == "existing" else None)
        provider = None
        server_thread = None
        gateway = None
        worker = None
        native = None
        native_passed = False
        worker_passed = False
        redactions = (fixture.SECRET,)
        fixture.Provider.records = []
        try:
            version = command_output([str(codex), "--version"], env, work,
                                     redactions=redactions).decode().strip()
            result["codex_version"] = version
            if not re.fullmatch(r"codex-cli 0\.165(?:\.[0-9]+)?", version):
                raise AssertionError("this acceptance requires installed Codex 0.165")
            result["cli_version"] = command_output(
                [str(binary), "--version"], env, work, redactions=redactions).decode().strip()
            result["source_commit"] = command_output(
                ["git", "-C", str(SOURCE), "rev-parse", "HEAD"], env, work).decode().strip()

            class BoundedProvider(fixture.Provider):
                def setup(self):
                    self.request.settimeout(5)
                    super().setup()

            provider = http.server.ThreadingHTTPServer(("127.0.0.1", 0), BoundedProvider)
            provider.daemon_threads = True
            provider.block_on_close = False
            server_thread = threading.Thread(
                target=provider.serve_forever, kwargs={"poll_interval": .05}, daemon=True)
            server_thread.start()
            forward = root / "forward.json"
            forward.write_bytes(fixture.encoded({
                "version": 1, "provider_id": "local",
                "base_url": f"http://127.0.0.1:{provider.server_port}/v1",
                "models": ["native-fixture"], "api_key_env": "MAGPIE_NATIVE_FIXTURE_KEY"}))
            forward.chmod(0o600)
            result["forward_fixture_sha256"] = sha256(forward)
            gateway = Child(
                [str(binary), "serve", "--listen", "127.0.0.1:0", "--forward", str(forward)],
                {**env, "MAGPIE_NATIVE_FIXTURE_KEY": fixture.SECRET}, work,
                stdout_limit=8192, redactions=redactions)
            address = gateway.wait_for_bind()
            wait_healthy(address, gateway)
            publish_marker(root, token, "fixture", case=case, gateway=address)
            worker = Child(
                [str(test_binary), "--exact", WORKER, "--ignored", "--nocapture",
                 "--test-threads=1"], env, work, redactions=redactions)
            wait_connected(root, token, worker, gateway)
            if fixture.Provider.records:
                raise AssertionError("inspect/preview/apply unexpectedly invoked the provider")
            connected_bytes = config.read_bytes()
            result["connected_config_sha256"] = hashlib.sha256(connected_bytes).hexdigest()
            native = Child(
                [str(codex), "exec", "--ephemeral", "--ignore-rules", "--skip-git-repo-check",
                 "--sandbox", "read-only", "--color", "never", "--json", "-C", str(work),
                 "Reply with the supplied fixture response. Do not use any tools."],
                env, work, stdout_limit=2 * 1024 * 1024, redactions=redactions)
            code = native.wait(60)
            events = [json.loads(line) for line in bytes(native.output["stdout"]).splitlines()
                      if line.strip()]
            messages = [event["item"].get("text") for event in events
                        if event.get("type") == "item.completed"
                        and event.get("item", {}).get("type") == "agent_message"]
            result.update({"codex_exit_code": code, "marker_received": fixture.MARKER in messages,
                           "event_types": [event.get("type") for event in events],
                           "provider_requests": list(fixture.Provider.records)})
            if code or fixture.MARKER not in messages:
                raise AssertionError("native Codex did not return the UTF-8 marker; "
                                     + native.diagnostics())
            expected = [{"path": "/v1/responses", "model": "native-fixture", "stream": True,
                         "provider_key_correct": True}]
            if fixture.Provider.records != expected:
                raise AssertionError("expected one Responses POST with rewritten model and fixture auth")
            if config.read_bytes() != connected_bytes:
                raise AssertionError("native Codex changed the GUI-applied config")
            native_passed = True
        except Exception as error:
            result["error"] = str(error)[-4096:]
        finally:
            for name, child in (("codex", native), ("gateway", gateway)):
                if child is not None:
                    try:
                        child.close()
                        child.check_output()
                    except Exception as error:
                        result["cleanup_errors"].append(f"{name}: {error}")
            if worker is not None and gateway is not None and gateway.process.poll() is not None:
                try:
                    # Also release a worker whose connected marker was published
                    # just as the coordinator failed. Give its bridge time to
                    # finish cleanup before terminating the worker itself.
                    publish_marker(root, token, "gateway-stopped")
                    result["gateway_stopped_before_disconnect"] = True
                    code = worker.wait(50)
                    result["worker_exit_code"] = code
                    if (code or read_marker(root, token, "connected") is None
                            or read_marker(root, token, "restored") is None):
                        raise AssertionError("Rust offline restoration failed; " + worker.diagnostics())
                    worker_passed = True
                except Exception as error:
                    result["cleanup_errors"].append(str(error)[-4096:])
            if worker is not None:
                try:
                    worker.close()
                    result["worker_exit_code"] = worker.process.returncode
                except Exception as error:
                    result["cleanup_errors"].append(f"worker: {error}")
            if provider is not None:
                if server_thread is not None and server_thread.is_alive():
                    shutdown = threading.Thread(target=provider.shutdown, daemon=True)
                    shutdown.start()
                    shutdown.join(timeout=2)
                    server_thread.join(timeout=2)
                    if shutdown.is_alive() or server_thread.is_alive():
                        result["cleanup_errors"].append("provider shutdown deadline")
                provider.server_close()
            result["provider_requests"] = list(fixture.Provider.records)
            restored = (config.read_bytes() == ORIGINAL if case == "existing" and config.is_file()
                        else not config.exists() if case == "missing" else False)
            result["original_bytes_or_absence_restored"] = restored
            result["worker_roundtrip_passed"] = worker_passed
            result["native_codex_passed"] = native_passed
            result["passed"] = (native_passed and worker_passed and restored
                                and "error" not in result and not result["cleanup_errors"])
            result["duration_seconds"] = round(time.monotonic() - started, 3)
    return result


def self_check():
    """Exercise hanging/partial/overflow startup and marker cleanup, without Rust."""
    with tempfile.TemporaryDirectory(prefix="magpie-desktop-actions-") as directory:
        root = Path(directory)
        env = {"PATH": os.defpath, "HOME": str(root), "LANG": "C.UTF-8"}
        for source in (
                "import time; time.sleep(60)",
                "import os,time; os.write(1,b'partial'); time.sleep(60)",
                "import os,time; os.write(2,b'x'*262144); time.sleep(60)",
                "raise SystemExit(3)"):
            child = Child([sys.executable, "-I", "-c", source], env, root)
            started = time.monotonic()
            try:
                try:
                    child.wait_for_bind(timeout=.15)
                except AssertionError:
                    pass
                else:
                    raise AssertionError("startup self-check unexpectedly succeeded")
            finally:
                child.close()
            assert child.process.returncode is not None, "startup child was not reaped"
            assert time.monotonic() - started < 4, "startup cleanup was unbounded"
        child = Child([sys.executable, "-I", "-c", "import time; time.sleep(60)"], env, root)
        try:
            try:
                wait_connected(root, "self-check", child, child, timeout=.1)
            except AssertionError as error:
                assert "marker deadline" in str(error)
            else:
                raise AssertionError("missing marker self-check unexpectedly succeeded")
        finally:
            child.close()
        assert child.process.returncode is not None, "marker child was not reaped"
        publish_marker(root, "self-check", "connected")
        assert read_marker(root, "self-check", "connected") is not None
        try:
            read_marker(root, "wrong-token", "connected")
        except AssertionError:
            pass
        else:
            raise AssertionError("marker identity self-check unexpectedly succeeded")
        child = Child(
            [sys.executable, "-I", "-c",
             "import time; print('candidate bound at 127.0.0.1:12345',flush=True); time.sleep(60)"],
            env, root)
        try:
            assert child.wait_for_bind(timeout=2) == "127.0.0.1:12345"
        finally:
            child.close()
        assert child.process.returncode is not None, "bound child was not reaped"
    print(json.dumps({"startup_and_marker_self_checks": 8, "passed": True}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, help="absolute parent-built root CLI")
    parser.add_argument("--test-binary", type=Path, help="absolute GUI library libtest executable")
    parser.add_argument("--codex", type=Path, help="absolute installed Codex 0.165 executable")
    parser.add_argument("--report", type=Path)
    parser.add_argument("--source-context", help="parent's source/build provenance label")
    parser.add_argument("--self-check", action="store_true", help="only run Python startup/cleanup checks")
    args = parser.parse_args()
    if os.name != "posix":
        parser.error("this host acceptance currently requires Unix")
    if args.self_check:
        self_check()
        return 0
    for option in ("binary", "test_binary", "codex"):
        path = getattr(args, option)
        if path is None or not path.is_absolute() or not path.is_file() or not os.access(path, os.X_OK):
            parser.error(f"--{option.replace('_', '-')} must be an absolute executable file")
        setattr(args, option, path.resolve(strict=True))
    started = time.monotonic()
    result = {
        "scope": "window_independent_desktop_actions_real_cli_gateway_installed_codex",
        "acceptance": "T01_continuous_synthetic_provider_roundtrip",
        "worker_test": WORKER, "passed": False,
        "native_gui_window": "not_run", "native_ime": "not_run", "external_provider": "not_run",
        "binary": {"path": str(args.binary), "sha256": sha256(args.binary)},
        "test_binary": {"path": str(args.test_binary), "sha256": sha256(args.test_binary)},
        "codex": {"path": str(args.codex), "sha256": sha256(args.codex)},
        "source_input_context": {
            "source_root": str(SOURCE), "parent_label": args.source_context,
            "observed_inputs_sha256": {path: sha256(SOURCE / path) for path in SOURCE_INPUTS},
            "binary_source_match": "parent_build_provenance_required"},
        "cases": [],
    }
    try:
        fixture = load_provider_fixture()
        result["fixture_module_sha256"] = sha256(SOURCE / "tests/native_codex.py")
        result["marker_sha256"] = hashlib.sha256(fixture.MARKER.encode()).hexdigest()
        for case in ("existing", "missing"):
            result["cases"].append(run_case(
                args.binary, args.test_binary, args.codex, fixture, case))
        result["passed"] = all(case["passed"] for case in result["cases"])
    finally:
        result["duration_seconds"] = round(time.monotonic() - started, 3)
        text = json.dumps(result, ensure_ascii=False, indent=2) + "\n"
        if args.report:
            args.report.write_text(text)
        print(text, end="")
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
