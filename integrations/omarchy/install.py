#!/usr/bin/env python3
"""Install the native Omarchy launcher without changing unrelated plugins."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess

PLUGIN_ID = "magpie-rs.magpie"
ROOT = Path(__file__).resolve().parent


def command(*args):
    subprocess.run(args, check=True, timeout=15)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("install", "remove"))
    parser.add_argument("--executable", default="magpie-desktop")
    args = parser.parse_args(argv)
    config = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config")
    target = config / "omarchy" / "plugins" / PLUGIN_ID
    if target.is_symlink():
        parser.error(f"plugin directory is a symlink: {target}; refusing to follow it")
    if args.action == "remove":
        if not target.exists():
            return
        try:
            manifest = json.loads((target / "manifest.json").read_text())
        except (OSError, ValueError):
            parser.error(f"cannot verify plugin ownership: {target}")
        if manifest.get("id") != PLUGIN_ID:
            parser.error(f"unexpected plugin identity: {target}; refusing to remove it")
    else:
        candidate = shutil.which(args.executable) or args.executable
        exe = str(Path(candidate).expanduser().resolve())
        if not Path(exe).is_file() or not os.access(exe, os.X_OK):
            parser.error(f"executable not found or not executable: {exe}")
        if target.exists():
            parser.error(f"plugin directory already exists: {target}; refusing to overwrite")
    if not shutil.which("omarchy") or not shutil.which("omarchy-shell"):
        parser.error("Omarchy shell commands are unavailable")
    if args.action == "remove":
        command("omarchy", "plugin", "disable", PLUGIN_ID)
        shutil.rmtree(target)
        command("omarchy-shell", "shell", "rescanPlugins")
        return
    target.mkdir(parents=True)
    enable_attempted = False
    try:
        (target / "manifest.json").write_bytes((ROOT / "manifest.json").read_bytes())
        qml = (ROOT / "Widget.qml").read_text()
        qml = qml.replace('"__MAGPIE_RS_EXECUTABLE__"', json.dumps(exe))
        (target / "Widget.qml").write_text(qml)
        command("omarchy-shell", "shell", "rescanPlugins")
        enable_attempted = True
        command("omarchy", "plugin", "enable", PLUGIN_ID)
    except Exception:
        if enable_attempted:
            try:
                command("omarchy", "plugin", "disable", PLUGIN_ID)
            except Exception:
                pass
        shutil.rmtree(target)
        try:
            command("omarchy-shell", "shell", "rescanPlugins")
        except Exception:
            pass
        raise


if __name__ == "__main__":
    main()
