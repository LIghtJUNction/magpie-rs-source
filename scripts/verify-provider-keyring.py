#!/usr/bin/env python3
"""Exercise the real Linux Secret Service with synthetic credentials and HTTP.

Requires dbus-run-session and gnome-keyring-daemon. Everything runs in a private
temporary XDG data/config/runtime directory and a separate D-Bus session. No
real account, external Provider or existing Agent configuration is accessed.
"""

import argparse
import http.server
import json
import os
from pathlib import Path
import selectors
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request


FIRST = "synthetic-magpie-first-key"
SECOND = "synthetic-magpie-rotated-key"


class Upstream(http.server.ThreadingHTTPServer):
    def __init__(self):
        super().__init__(("127.0.0.1", 0), Handler)
        self.observed = []
        self.catalog_headers = []
        self.thread = threading.Thread(target=self.serve_forever, daemon=True)
        self.thread.start()

    def close(self):
        self.shutdown()
        self.server_close()
        self.thread.join(timeout=5)


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        data = json.loads(self.rfile.read(int(self.headers["content-length"])))
        self.server.observed.append((self.headers.get("authorization"), data["model"]))
        self.reply({"id": "fixture-completion", "object": "chat.completion", "created": 1,
                    "model": data["model"], "choices": [{"index": 0,
                    "message": {"role": "assistant", "content": "fixture-ok"},
                    "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5}})

    def do_GET(self):
        self.server.catalog_headers.append(self.headers.get("authorization"))
        self.reply({"object": "list", "data": [{"id": "tiny", "object": "model"}]})

    def reply(self, value):
        body = json.dumps(value).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def run(magpie, arguments, env, key=None, succeeds=True):
    result = subprocess.run([str(magpie), *arguments, "--json"],
                            env=env, input=key, text=True, capture_output=True, timeout=25)
    assert FIRST not in result.stdout + result.stderr, "credential leaked to CLI output"
    assert SECOND not in result.stdout + result.stderr, "rotated credential leaked to output"
    if succeeds:
        assert result.returncode == 0, result.stderr
        return json.loads(result.stdout)
    assert result.returncode != 0, "expected operation to fail"
    return json.loads(result.stderr)


def request(base, method, path, body=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(base + path, data=data, method=method,
                                 headers={"Content-Type": "application/json"})
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        with opener.open(req, timeout=10) as response:
            return response.status, json.load(response)
    except urllib.error.HTTPError as error:
        return error.code, json.load(error)


def stop(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def inside_session(magpie, root):
    env = os.environ.copy()
    for name, directory in (("XDG_DATA_HOME", "data"), ("XDG_CONFIG_HOME", "config"),
                            ("XDG_RUNTIME_DIR", "run"), ("MAGPIE_CONFIG_DIR", "magpie")):
        target = root / directory
        target.mkdir(mode=0o700)
        env[name] = str(target)
    control = root / "run" / "keyring"
    control.mkdir(mode=0o700)
    keyring_log = (root / "keyring.log").open("wb")
    daemon = subprocess.Popen(["gnome-keyring-daemon", "--foreground", "--unlock",
                               "--components=secrets", "--control-directory", str(control)],
                              env=env, stdin=subprocess.PIPE, stdout=keyring_log,
                              stderr=subprocess.STDOUT)
    daemon.stdin.write(b"synthetic-only-vault-password")
    daemon.stdin.close()
    upstream = Upstream()
    gateway = None
    checks = []
    try:
        # Wait for the isolated daemon to register, not for the user's session.
        for _ in range(100):
            ready = subprocess.run(["dbus-send", "--session", "--print-reply", "--reply-timeout=500",
                                    "--dest=org.freedesktop.DBus", "/org/freedesktop/DBus",
                                    "org.freedesktop.DBus.NameHasOwner", "string:org.freedesktop.secrets"],
                                   env=env, capture_output=True, timeout=2)
            if b"boolean true" in ready.stdout:
                break
            assert daemon.poll() is None, "isolated credential daemon exited"
            time.sleep(0.05)
        else:
            raise AssertionError("isolated credential service did not become ready")
        address = f"http://127.0.0.1:{upstream.server_port}/v1"
        added = run(magpie, ["providers", "add", "fixture", "--name", "Synthetic Provider",
                            "--url", address, "--protocol", "openai_chat", "--model", "tiny",
                            "--key-stdin"], env, FIRST + "\n")
        assert added["providers"][0]["credential_source"] == "system_keyring"
        store_path = Path(env["MAGPIE_CONFIG_DIR"]) / "providers.toml"
        assert FIRST not in store_path.read_text()
        checks.append("real_secret_service_save_and_public_config_redaction")
        reloaded = run(magpie, ["providers", "list"], env)
        assert reloaded["providers"][0]["credential_source"] == "system_keyring"
        refreshed = run(magpie, ["providers", "refresh", "fixture", "--if-revision",
                                 str(reloaded["revision"])], env)
        assert refreshed["operation"] == "providers.refresh"
        assert refreshed["provider_id"] == "fixture"
        assert refreshed["revision"] == reloaded["revision"] + 1
        assert upstream.catalog_headers[-1] == "Bearer " + FIRST
        checks.append("explicit_model_refresh_uses_saved_key_and_persists_validated_models")

        gateway = subprocess.Popen([str(magpie), "serve", "--listen", "127.0.0.1:0"],
                                   env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        with selectors.DefaultSelector() as selector:
            selector.register(gateway.stdout, selectors.EVENT_READ)
            assert selector.select(timeout=10), "Gateway did not announce its address"
            line = gateway.stdout.readline()
        assert line.startswith("Gateway listening at "), line
        base = "http://" + line.split()[3]
        status, catalog = request(base, "GET", "/v1/models")
        assert status == 200 and catalog["data"][0]["id"] == "fixture/tiny"
        payload = {"model": "fixture/tiny", "messages": [{"role": "user", "content": "synthetic test"}]}
        status, completion = request(base, "POST", "/v1/chat/completions", payload)
        assert status == 200 and completion["choices"][0]["message"]["content"] == "fixture-ok"
        assert upstream.observed[-1] == ("Bearer " + FIRST, "tiny")
        checks.append("separate_gateway_process_reads_saved_key_and_forwards_request")

        rotated = run(magpie, ["providers", "edit", "fixture", "--key-stdin"], env, SECOND + "\n")
        assert rotated["credential_cleanup_pending"] is False
        assert request(base, "POST", "/v1/chat/completions", payload)[0] == 200
        assert upstream.observed[-1] == ("Bearer " + SECOND, "tiny")
        assert FIRST not in store_path.read_text() and SECOND not in store_path.read_text()
        checks.append("key_rotation_takes_effect_without_gateway_restart")

        run(magpie, ["providers", "disable", "fixture"], env)
        attempts = len(upstream.observed)
        assert request(base, "GET", "/v1/models")[1]["data"] == []
        assert request(base, "POST", "/v1/chat/completions", payload)[0] == 404
        assert len(upstream.observed) == attempts
        run(magpie, ["providers", "enable", "fixture"], env)
        checks.append("disable_hides_models_and_stops_new_upstream_requests")

        codex = root / "codex"
        codex.mkdir(mode=0o700)
        config = codex / "config.toml"
        original = '# fixture comment\nmodel = "native-model"\ncustom = "preserve-me"\n'
        config.write_text(original)
        config.chmod(0o600)
        arguments = ["agents", "connect", "codex", "--model", "fixture/tiny",
                     "--config", str(config), "--gateway", base.removeprefix("http://")]
        run(magpie, [*arguments, "--dry-run"], env)
        assert config.read_text() == original
        run(magpie, [*arguments, "--apply"], env)
        assert 'fixture/tiny' in config.read_text() and 'preserve-me' in config.read_text()
        run(magpie, ["agents", "disconnect", "codex", "--config", str(config)], env)
        assert config.read_text() == original
        checks.append("codex_preview_apply_and_exact_restore_with_live_provider")

        removed = run(magpie, ["providers", "remove", "fixture", "--yes"], env)
        assert removed["credential_cleanup_pending"] is False
        assert run(magpie, ["providers", "list"], env)["providers"] == []
        checks.append("provider_remove_and_credential_cleanup")
        print(json.dumps({"status": "passed", "platform": sys.platform,
                          "keyring": "isolated GNOME Secret Service", "checks": checks}, indent=2))
    finally:
        if gateway is not None:
            stop(gateway)
        upstream.close()
        stop(daemon)
        keyring_log.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--magpie", type=Path, required=True)
    parser.add_argument("--inside", type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    magpie = args.magpie.resolve(strict=True)
    if args.inside:
        inside_session(magpie, args.inside.resolve(strict=True))
        return 0
    if sys.platform != "linux":
        parser.error("this real Secret Service test requires Linux")
    with tempfile.TemporaryDirectory(prefix="magpie-keyring-fixture-") as temp:
        return subprocess.call(["dbus-run-session", "--", sys.executable, str(Path(__file__).resolve()),
                                "--magpie", str(magpie), "--inside", temp])


if __name__ == "__main__":
    raise SystemExit(main())
