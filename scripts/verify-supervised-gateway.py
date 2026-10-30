#!/usr/bin/env python3
"""Verify loss of a desktop owner's pipe using isolated real OS processes.

The observer owns the CLI child so it can always reap it. A separate process is
the sole holder of the lease pipe's write end, just as the desktop would be.
Killing that lease holder must stop the Gateway without any stop/kill signal to
the Gateway itself. This check needs POSIX pass_fds and loopback sockets.
"""

from __future__ import annotations

import argparse
import hashlib
import http.client
import json
import os
from pathlib import Path
import selectors
import socket
import subprocess
import sys
import tempfile
import time


def read_ready(process: subprocess.Popen[bytes]) -> dict:
    assert process.stdout is not None
    data = bytearray()
    with selectors.DefaultSelector() as selector:
        selector.register(process.stdout, selectors.EVENT_READ)
        deadline = time.monotonic() + 5
        while b"\n" not in data:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not selector.select(remaining):
                raise AssertionError("Gateway did not become ready within five seconds")
            chunk = os.read(process.stdout.fileno(), 4097 - len(data))
            if not chunk or len(data) + len(chunk) > 4096:
                raise AssertionError("Gateway ready event was absent or exceeded its bound")
            data.extend(chunk)
    event = json.loads(data)
    assert event["event"] == "gateway_ready"
    assert event["protocol_version"] == 1
    assert event["config_source"] == "provider_store"
    return event


def verify(binary: Path) -> dict:
    if os.name != "posix":
        raise RuntimeError("this check uses POSIX pipe ownership; other OS checks remain separate")
    with tempfile.TemporaryDirectory(prefix="magpie-owner-crash-") as directory:
        read_fd, write_fd = os.pipe()
        owner = None
        gateway = None
        try:
            owner = subprocess.Popen(
                [sys.executable, "-c", "import sys; sys.stdin.buffer.read()"],
                stdin=subprocess.PIPE,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                pass_fds=(write_fd,),
            )
            gateway = subprocess.Popen(
                [str(binary), "serve", "--supervised", "--json", "--listen", "127.0.0.1:0"],
                env={"MAGPIE_CONFIG_DIR": directory},
                stdin=read_fd,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                close_fds=True,
            )
            os.close(read_fd)
            os.close(write_fd)
            read_fd = write_fd = -1
            ready = read_ready(gateway)
            host, port_text = ready["address"].rsplit(":", 1)
            assert host == "127.0.0.1"
            port = int(port_text)
            connection = http.client.HTTPConnection(host, port, timeout=2)
            connection.request("GET", "/health")
            response = connection.getresponse()
            assert response.status == 200
            health = json.loads(response.read(16385))
            connection.close()
            for field in ("server_version", "address", "instance_id", "config_source"):
                assert ready[field] == health[field]
            assert gateway.poll() is None

            started = time.monotonic()
            owner.kill()  # Only the process holding the lease dies.
            owner.wait(timeout=2)
            gateway.wait(timeout=3)
            elapsed = time.monotonic() - started
            assert gateway.returncode == 0
            assert gateway.stderr is not None
            assert gateway.stderr.read(4097) == b""
            try:
                with socket.create_connection((host, port), timeout=1):
                    raise AssertionError("Gateway still accepts connections after owner loss")
            except ConnectionRefusedError:
                pass
            with socket.socket() as listener:
                # The health connection can leave TIME_WAIT after a clean exit.
                # Match std::net::TcpListener's Unix address reuse semantics.
                listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                listener.bind((host, port))
            assert not list(Path(directory).iterdir()), "lifecycle must not write user configuration"
            return {
                "status": "passed",
                "case": "owner_process_crash_releases_lease_and_listener",
                "server_version": ready["server_version"],
                "shutdown_seconds": round(elapsed, 4),
                "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                "uses_real_accounts": False,
                "uses_display_or_gpu": False,
                "platform": sys.platform,
            }
        finally:
            for descriptor in (read_fd, write_fd):
                if descriptor >= 0:
                    os.close(descriptor)
            for process in (owner, gateway):
                if process is not None:
                    if process.poll() is None:
                        process.kill()
                    process.wait(timeout=3)
                    for stream in (process.stdin, process.stdout, process.stderr):
                        if stream is not None:
                            stream.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--magpie", type=Path, required=True)
    arguments = parser.parse_args()
    print(json.dumps(verify(arguments.magpie.resolve(strict=True)), indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
