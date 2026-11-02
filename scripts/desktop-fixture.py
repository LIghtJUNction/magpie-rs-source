#!/usr/bin/env python3
"""Run the native desktop against isolated files and a synthetic loopback API.

Linux only; requires bubblewrap. --reuse reopens the same synthetic state and
ports for restart checks. No actual Agent authentication files are exposed.
"""

import argparse
import http.server
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import threading


def unused_port():
    with socket.socket() as connection:
        connection.bind(("127.0.0.1", 0))
        return connection.getsockname()[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--desktop", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--reuse", action="store_true")
    args = parser.parse_args()
    root = args.output.resolve()
    if args.reuse:
        identity = json.loads((root / "fixture.json").read_text())
        if identity.get("kind") != "magpie-synthetic-desktop-v1":
            raise RuntimeError("not a desktop fixture directory")
    else:
        root.mkdir(mode=0o700)
        for folder in ["config", "codex", "claude", "gemini", "xdg", "bin"]:
            (root / folder).mkdir(mode=0o700)
        (root / "codex/config.toml").write_text(
            '# Isolated desktop fixture\nmodel = "fixture-original"\nkeep_me = true\n'
        )
        stub = root / "bin/codex"
        stub.write_text("#!/bin/sh\nexit 0\n")
        stub.chmod(0o700)
        identity = {
            "kind": "magpie-synthetic-desktop-v1",
            "upstream_port": unused_port(),
            "gateway_port": unused_port(),
        }
        (root / "fixture.json").write_text(json.dumps(identity, indent=2) + "\n")
        (root / "config/desktop-preferences.json").write_text(json.dumps({
            "version": 1, "locale": "locale-zh-cn", "appearance": "light",
            "gateway_address": f"127.0.0.1:{identity['gateway_port']}",
        }) + "\n")
    records = []
    lock = threading.Lock()

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *arguments):
            pass

        def reply(self, payload, status=200):
            body = json.dumps(payload).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            if self.path != "/v1/models":
                return self.reply({"error": "unknown fixture path"}, 404)
            self.reply({"object": "list", "data": [
                {"id": model, "object": "model"} for model in ["model-a", "model-b"]
            ]})

        def do_POST(self):
            length = int(self.headers.get("Content-Length", "0"))
            if not 0 < length <= 1024 * 1024:
                return self.reply({"error": "fixture request size"}, 413)
            request = json.loads(self.rfile.read(length))
            with lock:
                records.append({"method": "POST", "path": self.path,
                                "model": request.get("model"), "stream": request.get("stream", False)})
                del records[:-256]
                (root / "upstream-observations.json").write_text(json.dumps(records, indent=2) + "\n")
            if self.path == "/v1/chat/completions" and not request.get("stream"):
                return self.reply({"id": "fixture-chat", "object": "chat.completion",
                    "model": request["model"], "choices": [{"index": 0,
                    "message": {"role": "assistant", "content": "fixture-ok"}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 7, "completion_tokens": 2, "total_tokens": 9}})
            if self.path == "/v1/responses" and not request.get("stream"):
                return self.reply({"id": "fixture-response", "object": "response", "status": "completed",
                    "model": request["model"], "output": [{"type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "fixture-ok"}]}],
                    "usage": {"input_tokens": 7, "output_tokens": 2, "total_tokens": 9}})
            self.reply({"error": "fixture supports JSON chat and responses only"}, 400)

    server = http.server.ThreadingHTTPServer(("127.0.0.1", identity["upstream_port"]), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    environment = {key: value for key, value in os.environ.items()
                   if not any(word in key.upper() for word in ["TOKEN", "SECRET", "API_KEY", "CREDENTIAL", "PASSWORD"])}
    environment.pop("CODEX_HOME", None)
    environment.update({"XDG_RUNTIME_DIR": f"/run/user/{os.getuid()}", "WAYLAND_DISPLAY": "wayland-1",
        "DBUS_SESSION_BUS_ADDRESS": f"unix:path=/run/user/{os.getuid()}/bus",
        "MAGPIE_CONFIG_DIR": str(root / "config"), "MAGPIE_CLI_PATH": str(args.cli.resolve()),
        "XDG_CONFIG_HOME": str(root / "xdg"), "PATH": str(root / "bin") + ":/usr/bin",
        "LANG": "zh_CN.UTF-8", "LC_ALL": "zh_CN.UTF-8"})
    command = ["bwrap", "--ro-bind", "/", "/", "--dev-bind", "/dev", "/dev",
               "--bind", str(root), str(root)]
    for name, folder in [(".codex", "codex"), (".claude", "claude"), (".gemini", "gemini"), (".config", "xdg")]:
        destination = Path.home() / name
        if destination.exists():
            command += ["--bind", str(root / folder), str(destination)]
    command.append(str(args.desktop.resolve()))
    with (root / "desktop.log").open("ab") as log:
        process = subprocess.Popen(command, env=environment, stdout=log, stderr=log)
        signal.signal(signal.SIGTERM, lambda *_: process.terminate() if process.poll() is None else None)
        signal.signal(signal.SIGINT, lambda *_: process.terminate() if process.poll() is None else None)
        print(json.dumps(identity | {"desktop_pid": process.pid, "directory": str(root)}), flush=True)
        try:
            process.wait()
        finally:
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=5)
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)


if __name__ == "__main__":
    main()
