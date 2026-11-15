#!/usr/bin/env python3
# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
"""Render the native Wayland/Crossfont title on an isolated Weston compositor.

Needs weston, Mesa's software EGL renderer and an installed font. The screenshot
is retained for visual review. Usage: python3 scripts/test-linux-wayland.py
    target/debug/clipsync-desktop target/wayland-smoke
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time


def stop(process):
    if process is None:
        return
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


def main():
    if sys.platform != "linux":
        raise SystemExit("This integration check requires Linux.")
    inside = sys.argv[1:2] == ["--private-session"]
    args = sys.argv[2:] if inside else sys.argv[1:]
    binary = Path(args[0]).resolve(strict=True)
    out = Path(args[1]).resolve()
    out.mkdir(parents=True, exist_ok=True)
    if not inside:
        subprocess.run(["dbus-run-session", "--", sys.executable,
                        str(Path(__file__).resolve()), "--private-session", str(binary), str(out)], check=True)
        return
    with tempfile.TemporaryDirectory(prefix="clipsync-wayland-test-") as temporary:
        fixture = Path(temporary)
        runtime = fixture / "runtime"
        runtime.mkdir(mode=0o700)
        data = fixture / "data"
        data.mkdir(mode=0o700)
        identity = data / "identity.json"
        identity.write_text(json.dumps({
            "device_id": "wayland-integration-fixture", "secret": "07" * 32,
            "public": "00" * 32, "fingerprint": "synthetic-fixture",
        }))
        identity.chmod(0o600)
        env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), WAYLAND_DISPLAY="clipsync-test",
                   CLIPSYNC_HOME=temporary, CLIPSYNC_RELAY_URL="http://127.0.0.1:9")
        env.pop("DISPLAY", None)
        app = None
        with (out / "weston.log").open("w") as weston_log, (out / "app.log").open("w") as app_log:
            compositor = subprocess.Popen([
                "weston", "--backend=headless-backend.so", "--use-gl", "--width=1000",
                "--height=800", "--socket=clipsync-test", "--idle-time=0", "--debug", "--no-config",
            ], env=env, stdout=weston_log, stderr=subprocess.STDOUT)
            try:
                for _ in range(100):
                    assert compositor.poll() is None, "Weston exited; inspect weston.log"
                    if (runtime / "clipsync-test").exists():
                        break
                    time.sleep(0.05)
                else:
                    raise AssertionError("Weston did not create its private socket")
                app = subprocess.Popen([str(binary)], env=dict(env, WAYLAND_DEBUG="1"),
                                       stdout=app_log, stderr=subprocess.STDOUT)
                time.sleep(3)
                assert app.poll() is None, "Desktop exited; inspect app.log"
                assert 'set_title("ClipSync")' in (out / "app.log").read_text()
                before = set(out.glob("*.png"))
                subprocess.run(["weston-screenshooter"], cwd=out, env=env, check=True,
                               timeout=10, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
                screenshots = set(out.glob("*.png")) - before
                assert screenshots, "The compositor produced no screenshot"
                print(json.dumps({"native_title": "ClipSync", "screenshots": sorted(map(str, screenshots))}))
            finally:
                stop(app)
                stop(compositor)


if __name__ == "__main__":
    main()
