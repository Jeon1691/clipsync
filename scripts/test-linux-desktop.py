#!/usr/bin/env python3
# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
"""Check the native window and AT-SPI tree on a private X server and D-Bus.

Needs dbus-run-session, xvfb-run, xwininfo, python3-pyatspi and an installed font.
Usage: python3 scripts/test-linux-desktop.py target/debug/clipsync-desktop
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time


def main():
    if sys.platform != "linux":
        raise SystemExit("This integration check requires Linux.")
    inside = sys.argv[1:2] == ["--private-session"]
    binary = Path(sys.argv[2] if inside else sys.argv[1]).resolve(strict=True)
    if not inside:
        subprocess.run(
            ["dbus-run-session", "--", "xvfb-run", "-a", sys.executable,
             str(Path(__file__).resolve()), "--private-session", str(binary)], check=True
        )
        return

    import pyatspi
    from gi.repository import Gio, GLib

    session = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    session.call_sync(
        "org.a11y.Bus", "/org/a11y/bus", "org.freedesktop.DBus.Properties", "Set",
        GLib.Variant("(ssv)", ("org.a11y.Status", "IsEnabled", GLib.Variant("b", True))),
        None, Gio.DBusCallFlags.NONE, 5000, None,
    )
    with tempfile.TemporaryDirectory(prefix="clipsync-desktop-test-") as fixture:
        data = Path(fixture) / "data"
        data.mkdir(mode=0o700)
        identity = data / "identity.json"
        identity.write_text(json.dumps({
            "device_id": "desktop-integration-fixture", "secret": "07" * 32,
            "public": "00" * 32, "fingerprint": "synthetic-fixture",
        }))
        identity.chmod(0o600)
        env = dict(os.environ, CLIPSYNC_HOME=fixture,
                   CLIPSYNC_RELAY_URL="http://127.0.0.1:9", NO_AT_BRIDGE="0")
        with (Path(fixture) / "app.log").open("w+") as log:
            app = subprocess.Popen([str(binary)], env=env, stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 20
                names = []
                while time.monotonic() < deadline:
                    if app.poll() is not None:
                        log.seek(0)
                        raise AssertionError(f"Desktop exited ({app.returncode}): {log.read()}")
                    names = []
                    def visit(node, depth=0):
                        if depth > 8:
                            return
                        names.append((node.getRoleName(), node.name))
                        for child in node:
                            if child is not None:
                                visit(child, depth + 1)
                    visit(pyatspi.Registry.getDesktop(0))
                    labels = {name for _, name in names}
                    if {"CLIPSYNC", "Desktop", "Not paired", "Create room", "Join"}.issubset(labels):
                        break
                    time.sleep(0.1)
                else:
                    raise AssertionError(f"AT-SPI window/button tree unavailable: {names!r}")
                windows = subprocess.check_output(["xwininfo", "-root", "-tree"], text=True)
                assert '"ClipSync"' in windows, windows
                print(json.dumps({"native_title": "ClipSync", "atspi_nodes": names}))
            finally:
                app.terminate()
                try:
                    app.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    app.kill()
                    app.wait()


if __name__ == "__main__":
    main()
