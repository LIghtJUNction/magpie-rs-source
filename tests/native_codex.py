#!/usr/bin/env python3
"""Run an installed Codex through Magpie using only an isolated synthetic provider.

No login, real account, real Agent config, or external model is used. This is a
native-client protocol/configuration acceptance, not a real-model quality test.
"""
import argparse
import hashlib
import http.client
import http.server
import json
import os
from pathlib import Path
import select
import subprocess
import tempfile
import threading
import time

MARKER = "MAGPIE_真实闭环_OK"
SECRET = "magpie-native-fixture-not-a-real-key"


def encoded(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()


class Provider(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    records = []

    def log_message(self, *_):
        pass

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        if not 0 < length <= 1024 * 1024:
            self.send_error(413)
            return
        request = json.loads(self.rfile.read(length))
        self.records.append({"path": self.path, "model": request.get("model"),
                             "stream": request.get("stream"),
                             "provider_key_correct": self.headers.get("Authorization") == "Bearer " + SECRET})
        response = {"id": "resp_native_fixture", "object": "response", "created_at": 1700000000,
                    "model": "native-fixture", "status": "in_progress", "error": None, "output": []}
        item = {"id": "msg_native_fixture", "type": "message", "role": "assistant",
                "status": "in_progress", "content": []}
        part = {"type": "output_text", "text": MARKER, "annotations": []}
        completed_item = {**item, "status": "completed", "content": [part]}
        completed = {**response, "status": "completed", "output": [completed_item],
                     "usage": {"input_tokens": 10, "output_tokens": 7, "total_tokens": 17,
                               "input_tokens_details": {"cached_tokens": 0},
                               "output_tokens_details": {"reasoning_tokens": 0}}}
        events = [
            {"type": "response.created", "response": response},
            {"type": "response.output_item.added", "output_index": 0, "item": item},
            {"type": "response.content_part.added", "item_id": item["id"], "output_index": 0,
             "content_index": 0, "part": {**part, "text": ""}},
            {"type": "response.output_text.delta", "item_id": item["id"], "output_index": 0,
             "content_index": 0, "delta": MARKER},
            {"type": "response.output_text.done", "item_id": item["id"], "output_index": 0,
             "content_index": 0, "text": MARKER},
            {"type": "response.content_part.done", "item_id": item["id"], "output_index": 0,
             "content_index": 0, "part": part},
            {"type": "response.output_item.done", "output_index": 0, "item": completed_item},
            {"type": "response.completed", "response": completed},
        ]
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Connection", "close")
        self.end_headers()
        self.close_connection = True
        try:
            for sequence, event in enumerate(events):
                event["sequence_number"] = sequence
                self.wfile.write(b"event: " + event["type"].encode() + b"\ndata: " + encoded(event) + b"\n\n")
                self.wfile.flush()
        except (ConnectionError, OSError):
            pass


def run(binary, codex, report):
    started = time.monotonic()
    result = {"scope": "installed_codex_isolated_config_synthetic_local_provider",
              "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "native_gpui": "not_run", "external_provider": "not_run",
              "original_magpie_ab": "not_run", "passed": False}
    with tempfile.TemporaryDirectory(prefix="magpie-native-codex-") as directory:
        root = Path(directory)
        for name in ("home", "codex", "work", "xdg", "cache"):
            (root / name).mkdir(mode=0o700)
        # Child-only environments: no real auth, proxy, configuration or telemetry
        # variables are inherited. The running parent process is never modified.
        child_env = {"PATH": os.environ.get("PATH", os.defpath), "HOME": str(root / "home"),
                     "CODEX_HOME": str(root / "codex"), "XDG_CONFIG_HOME": str(root / "xdg"),
                     "XDG_CACHE_HOME": str(root / "cache"), "LANG": "C.UTF-8"}
        config = root / "codex" / "config.toml"
        original = b'# isolated fixture, preserve bytes\nmodel = "before-model" # user comment\nmodel_reasoning_effort = "low"\n'
        config.write_bytes(original)
        config.chmod(0o600)
        provider = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
        provider.daemon_threads = True
        Provider.records = []
        threading.Thread(target=provider.serve_forever, daemon=True).start()
        forward = root / "forward.json"
        forward.write_bytes(encoded({"version": 1, "provider_id": "local",
                                     "base_url": f"http://127.0.0.1:{provider.server_port}/v1",
                                     "models": ["native-fixture"], "api_key_env": "MAGPIE_NATIVE_FIXTURE_KEY"}))
        service_env = {**child_env, "MAGPIE_NATIVE_FIXTURE_KEY": SECRET}
        gateway = subprocess.Popen([str(binary), "serve", "--listen", "127.0.0.1:0", "--forward", str(forward)],
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=service_env)
        connected = False
        try:
            if not select.select([gateway.stdout], [], [], 15)[0]:
                raise AssertionError("gateway bind timed out")
            line = gateway.stdout.readline().decode()
            if "bound at " not in line:
                raise AssertionError("gateway did not bind")
            address = line.split("bound at ", 1)[1].split()[0]
            host, port = address.rsplit(":", 1)
            for attempt in range(100):
                try:
                    connection = http.client.HTTPConnection(host, int(port), timeout=2)
                    connection.request("GET", "/health")
                    response = connection.getresponse()
                    response.read()
                    connection.close()
                    if response.status == 200:
                        break
                except (OSError, http.client.HTTPException):
                    pass
                time.sleep(.02)
            else:
                raise AssertionError("gateway readiness timed out")

            def magpie(*args):
                return subprocess.run([str(binary), *args], env=child_env, cwd=root / "work",
                                      capture_output=True, text=True, timeout=15, check=True)

            connect = ["agents", "connect", "codex", "--config", str(config), "--model",
                       "local/native-fixture", "--gateway", address, "--json"]
            magpie(*connect, "--dry-run")
            assert config.read_bytes() == original, "preview changed config"
            magpie(*connect, "--apply")
            connected = True
            result["codex_version"] = subprocess.run([str(codex), "--version"], env=child_env,
                                                     capture_output=True, text=True, check=True, timeout=15).stdout.strip()
            completed = subprocess.run([str(codex), "exec", "--ephemeral", "--ignore-rules",
                                        "--skip-git-repo-check", "--sandbox", "read-only", "--color", "never",
                                        "--json", "-C", str(root / "work"),
                                        "Reply with the supplied fixture response. Do not use any tools."],
                                       cwd=root / "work", env=child_env, capture_output=True, text=True, timeout=60)
            events = [json.loads(line) for line in completed.stdout.splitlines() if line.strip()]
            messages = [event["item"].get("text") for event in events
                        if event.get("type") == "item.completed" and event.get("item", {}).get("type") == "agent_message"]
            result.update({"codex_exit_code": completed.returncode,
                           "provider_requests": Provider.records,
                           "marker_received": MARKER in messages,
                           "event_types": [event.get("type") for event in events]})
            if completed.returncode or MARKER not in messages:
                # This child uses exclusively the isolated synthetic test state.
                raise AssertionError("native Codex did not complete: " + completed.stderr[-4000:] + completed.stdout[-4000:])
            assert len(Provider.records) == 1, "unexpected retry/request count"
            assert Provider.records[0] == {"path": "/v1/responses", "model": "native-fixture",
                                           "stream": True, "provider_key_correct": True}
            selected = json.loads(magpie("agents", "status", "codex", "--json").stdout)["agent"]
            assert (selected["model"], selected["model_provider"], selected["reasoning_effort"]) == ("local/native-fixture", "magpie", "low")
            magpie("agents", "disconnect", "codex", "--config", str(config), "--json")
            connected = False
            result["config_restored_exactly"] = config.read_bytes() == original
            assert result["config_restored_exactly"], "original config bytes were not restored"
            result["passed"] = True
        finally:
            if connected:
                try:
                    subprocess.run([str(binary), "agents", "disconnect", "codex", "--config", str(config), "--json"],
                                   env=child_env, capture_output=True, timeout=15)
                except (OSError, subprocess.TimeoutExpired):
                    pass  # Always reap the gateway, including failed test cleanup.
            gateway.terminate()
            try:
                gateway.wait(timeout=5)
            except subprocess.TimeoutExpired:
                gateway.kill()
                gateway.wait()
            provider.shutdown()
            provider.server_close()
            result["duration_seconds"] = round(time.monotonic() - started, 3)
            if report:
                report.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
            print(json.dumps(result, ensure_ascii=False))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--codex", type=Path, required=True)
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    run(args.binary.resolve(strict=True), args.codex.resolve(strict=True), args.report)
