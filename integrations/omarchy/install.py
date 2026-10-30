#!/usr/bin/env python3
"""Install the native Omarchy bar widget without touching unrelated plugins."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

PLUGIN_ID = "magpie-rs.magpie"
ROOT = Path(__file__).resolve().parent


def command(*args):
    subprocess.run(args, check=True, timeout=15)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=("install", "remove"))
    parser.add_argument("--executable", default="magpie-desktop")
    args = parser.parse_args()
    config = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config"))
    target = config / "omarchy" / "plugins" / PLUGIN_ID
    if args.action == "remove":
        command("omarchy", "plugin", "disable", PLUGIN_ID)
        shutil.rmtree(target)
        command("omarchy-shell", "shell", "rescanPlugins")
        return
    exe = shutil.which(args.executable) or str(Path(args.executable).expanduser().resolve())
    if not Path(exe).is_file():
        parser.error(f"executable not found: {exe}")
    if target.exists():
        parser.error(f"plugin directory already exists: {target}; refusing to overwrite")
    if not shutil.which("omarchy") or not shutil.which("omarchy-shell"):
        parser.error("Omarchy shell commands are unavailable")
    target.mkdir(parents=True)
    try:
        (target / "manifest.json").write_bytes((ROOT / "manifest.json").read_bytes())
        qml = (ROOT / "Widget.qml").read_text()
        qml = qml.replace('"__MAGPIE_RS_EXECUTABLE__"', json.dumps(exe))
        (target / "Widget.qml").write_text(qml)
        command("omarchy-shell", "shell", "rescanPlugins")
        command("omarchy", "plugin", "enable", PLUGIN_ID)
    except Exception:
        shutil.rmtree(target)
        try:
            command("omarchy-shell", "shell", "rescanPlugins")
        except Exception:
            pass
        raise


if __name__ == "__main__":
    main()
