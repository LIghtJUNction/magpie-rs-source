#!/usr/bin/env python3
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("magpie_omarchy_install", Path(__file__).with_name("install.py"))
install = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(install)


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.config = Path(self.temp.name) / "config"
        self.executable = Path(self.temp.name) / 'Magpie "local"'
        self.executable.write_text("#!/bin/sh\nexit 0\n")
        self.executable.chmod(0o700)
        self.target = self.config / "omarchy/plugins" / install.PLUGIN_ID
        self.calls = []
        original_which = install.shutil.which
        self.env = patch.dict(os.environ, {"XDG_CONFIG_HOME": str(self.config)})
        self.which = patch.object(install.shutil, "which", side_effect=lambda name: "/fake/" + name if name in ("omarchy", "omarchy-shell") else original_which(name))
        self.cmd = patch.object(install, "command", side_effect=lambda *args: self.calls.append(args))
        for mock in (self.env, self.which, self.cmd):
            mock.start()
            self.addCleanup(mock.stop)

    def add_plugin(self):
        install.main(["install", "--executable", str(self.executable)])

    def test_installs_escaped_executable_and_native_launch_contract(self):
        self.add_plugin()
        qml = (self.target / "Widget.qml").read_text()
        line = next(line for line in qml.splitlines() if "readonly property string magpieExecutable:" in line)
        self.assertEqual(json.loads(line.split("magpieExecutable:", 1)[1]), str(self.executable))
        self.assertIn("Quickshell.execDetached([root.magpieExecutable])", qml)
        self.assertNotIn('"panel"', qml)
        self.assertNotIn('"app"', qml)
        self.assertEqual(json.loads((self.target / "manifest.json").read_text())["id"], install.PLUGIN_ID)
        self.assertEqual(self.calls, [("omarchy-shell", "shell", "rescanPlugins"), ("omarchy", "plugin", "enable", install.PLUGIN_ID)])

    def test_existing_plugin_is_preserved(self):
        self.target.mkdir(parents=True)
        marker = self.target / "user-file"
        marker.write_text("preserve")
        with self.assertRaises(SystemExit):
            self.add_plugin()
        self.assertEqual(marker.read_text(), "preserve")
        self.assertEqual(self.calls, [])

    def test_non_executable_is_rejected_without_mutation(self):
        self.executable.chmod(0o600)
        with self.assertRaises(SystemExit):
            self.add_plugin()
        self.assertFalse(self.target.exists())
        self.assertEqual(self.calls, [])

    def test_failed_enable_rolls_back_only_created_plugin(self):
        other = self.target.parent / "other.plugin"
        other.mkdir(parents=True)
        (other / "keep").write_text("preserve")
        def fail_enable(*args):
            self.calls.append(args)
            if args[:3] == ("omarchy", "plugin", "enable"):
                raise subprocess.CalledProcessError(1, args)
        with patch.object(install, "command", side_effect=fail_enable):
            with self.assertRaises(subprocess.CalledProcessError):
                self.add_plugin()
        self.assertFalse(self.target.exists())
        self.assertEqual((other / "keep").read_text(), "preserve")
        self.assertIn(("omarchy", "plugin", "disable", install.PLUGIN_ID), self.calls)
        self.assertEqual(self.calls[-1], ("omarchy-shell", "shell", "rescanPlugins"))

    def test_remove_is_scoped_and_idempotent(self):
        self.add_plugin()
        other = self.target.parent / "other.plugin"
        other.mkdir()
        (other / "keep").write_text("preserve")
        self.calls.clear()
        install.main(["remove"])
        self.assertFalse(self.target.exists())
        self.assertEqual((other / "keep").read_text(), "preserve")
        self.assertEqual(self.calls, [("omarchy", "plugin", "disable", install.PLUGIN_ID), ("omarchy-shell", "shell", "rescanPlugins")])
        self.calls.clear()
        install.main(["remove"])
        self.assertEqual(self.calls, [])

    def test_remove_rejects_wrong_identity(self):
        self.target.mkdir(parents=True)
        (self.target / "manifest.json").write_text('{"id":"other.plugin"}')
        with self.assertRaises(SystemExit):
            install.main(["remove"])
        self.assertTrue(self.target.exists())
        self.assertEqual(self.calls, [])

    def test_symlink_is_never_followed(self):
        outside = Path(self.temp.name) / "outside"
        outside.mkdir()
        (outside / "keep").write_text("preserve")
        self.target.parent.mkdir(parents=True)
        self.target.symlink_to(outside, target_is_directory=True)
        for action in ["install", "remove"]:
            with self.assertRaises(SystemExit):
                install.main([action, "--executable", str(self.executable)])
        self.assertEqual((outside / "keep").read_text(), "preserve")
        self.assertEqual(self.calls, [])


if __name__ == "__main__":
    unittest.main()
