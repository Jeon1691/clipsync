#!/usr/bin/env python3
"""Live E2E against the Homebrew clipsync binary and hosted relay."""

from __future__ import annotations

import json
import os
import pty
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

CS = shutil.which("clipsync") or "/opt/homebrew/bin/clipsync"
RELAY = os.environ.get("CLIPSYNC_RELAY_URL", "https://clipsync.develicit.dev")
TIMEOUT = 45


class Fail(Exception):
    pass


def log(msg: str) -> None:
    print(msg, flush=True)


def run(home: Path, *args: str, input_bytes: bytes | None = None, timeout: int = 30) -> str:
    env = os.environ.copy()
    env["CLIPSYNC_HOME"] = str(home)
    env["CLIPSYNC_NO_DAEMON"] = "1"
    env["CLIPSYNC_RELAY_URL"] = RELAY
    try:
        proc = subprocess.run(
            [CS, *args],
            input=input_bytes,
            capture_output=True,
            timeout=timeout,
            env=env,
        )
    except subprocess.TimeoutExpired as e:
        raise Fail(f"{args} timed out after {timeout}s\nstdout:\n{e.stdout!r}\nstderr:\n{e.stderr!r}") from e
    out = (proc.stdout or b"").decode()
    err = (proc.stderr or b"").decode()
    if proc.returncode != 0:
        raise Fail(f"{args} rc={proc.returncode}\nstdout:\n{out}\nstderr:\n{err}")
    return out


def run_json(home: Path, *args: str, timeout: int = 30) -> dict:
    raw = run(home, "--json", *args, timeout=timeout).strip().splitlines()[-1]
    return json.loads(raw)


def spawn_create(home: Path) -> tuple[int, int]:
    env = os.environ.copy()
    env["CLIPSYNC_HOME"] = str(home)
    env["CLIPSYNC_NO_DAEMON"] = "1"
    env["CLIPSYNC_RELAY_URL"] = RELAY
    pid, fd = pty.fork()
    if pid == 0:
        os.execvpe(CS, [CS, "--json", "room", "create", "--no-auto-sync"], env)
    return pid, fd


def read_until_code(fd: int, deadline: float) -> str:
    buf = ""
    while time.time() < deadline:
        r, _, _ = select.select([fd], [], [], 0.2)
        if fd in r:
            try:
                chunk = os.read(fd, 4096).decode(errors="replace")
            except OSError:
                break
            if not chunk:
                break
            buf += chunk
            for line in buf.splitlines():
                line = line.strip()
                if not line.startswith("{"):
                    continue
                try:
                    data = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if data.get("pairing_code"):
                    return data["pairing_code"]
    raise Fail(f"pairing code not printed\n{buf}")


def wait_exit(pid: int, timeout: int) -> int:
    deadline = time.time() + timeout
    while time.time() < deadline:
        wpid, status = os.waitpid(pid, os.WNOHANG)
        if wpid == pid:
            return os.waitstatus_to_exitcode(status)
        time.sleep(0.1)
    os.kill(pid, signal.SIGTERM)
    raise Fail(f"process {pid} did not exit")


def main() -> int:
    if not Path(CS).exists():
        raise Fail(f"clipsync not found: {CS}")
    log(f"binary={CS}")
    log(subprocess.check_output([CS, "--version"], text=True).strip())
    log(f"relay={RELAY}")

    a = Path(tempfile.mkdtemp(prefix="cs-a-"))
    b = Path(tempfile.mkdtemp(prefix="cs-b-"))
    log(f"homeA={a}")
    log(f"homeB={b}")

    log("== init ==")
    run(a, "init", "--relay-url", RELAY)
    run(b, "init", "--relay-url", RELAY)

    log("== doctor ==")
    doctor = run_json(a, "doctor")
    if not doctor.get("identity"):
        raise Fail(f"identity missing: {doctor}")
    relay = doctor.get("relay") or {}
    if not relay.get("ok"):
        raise Fail(f"relay unhealthy: {doctor}")
    log(f"doctor relay ok status={relay.get('status')}")

    log("== config ==")
    cfg = run_json(a, "config", "get", "relay_url")
    if RELAY not in json.dumps(cfg):
        raise Fail(f"relay_url not set: {cfg}")
    run(a, "config", "set", "notify.enabled", "false")
    notify = run_json(a, "config", "get", "notify.enabled")
    if "false" not in json.dumps(notify):
        raise Fail(f"notify.enabled not persisted: {notify}")

    log("== wrong code ==")
    rejected = False
    last_err = ""
    for attempt in range(3):
        try:
            run(b, "room", "join", "000000", "--no-auto-sync", timeout=15)
            raise Fail("wrong code should fail")
        except Fail as e:
            last_err = str(e)
            if "join failed" in last_err or "code_invalid" in last_err or "unknown pairing" in last_err:
                rejected = True
                break
            log(f"wrong-code attempt {attempt + 1} unexpected: {last_err[:200]}")
            time.sleep(1)
    if not rejected:
        raise Fail(f"wrong code not rejected: {last_err}")
    log("wrong code rejected")

    log("== pairing ==")
    pid, fd = spawn_create(a)
    try:
        code = read_until_code(fd, time.time() + TIMEOUT)
        log(f"pairing code={code}")
        join_out = run(b, "room", "join", code, "--no-auto-sync", timeout=TIMEOUT)
        log(join_out.strip())
        rc = wait_exit(pid, TIMEOUT)
        if rc != 0:
            raise Fail(f"room create exited {rc}")
    finally:
        try:
            os.close(fd)
        except OSError:
            pass

    log("== status/devices/list ==")
    st_a = run_json(a, "status")
    st_b = run_json(b, "status")
    if not st_a.get("room_id") or not st_b.get("room_id"):
        raise Fail(f"missing room: a={st_a} b={st_b}")
    if st_a["room_id"] != st_b["room_id"]:
        raise Fail(f"room mismatch {st_a['room_id']} vs {st_b['room_id']}")
    log(f"room={st_a['room_id']}")
    devices = run(b, "devices")
    if not devices.strip():
        raise Fail("empty devices")
    log("devices:\n" + devices.strip())
    listed = run(a, "room", "list")
    if st_a["room_id"] not in listed:
        raise Fail(f"room list missing id: {listed}")

    log("== text push/pull ==")
    payload = f"clipsync-e2e-{int(time.time())}"
    run(a, "push", "--type", "text", input_bytes=payload.encode())
    pulled = run(b, "pull", "--wait", timeout=TIMEOUT)
    if payload not in pulled:
        raise Fail(f"text pull mismatch: {pulled!r}")
    log(f"pulled text: {pulled.strip()}")

    log("== reverse text ==")
    back = payload + "-back"
    run(b, "push", "--type", "text", input_bytes=back.encode())
    pulled_a = run(a, "pull", "--wait", timeout=TIMEOUT)
    if back not in pulled_a:
        raise Fail(f"reverse pull mismatch: {pulled_a!r}")
    log(f"pulled reverse: {pulled_a.strip()}")

    log("== file push while peer waits ==")
    src = a / "report.txt"
    src.write_text("file-payload-ok\n", encoding="utf-8")
    outdir = b / "inbox-out"
    env_b = os.environ.copy()
    env_b["CLIPSYNC_HOME"] = str(b)
    env_b["CLIPSYNC_NO_DAEMON"] = "1"
    env_b["CLIPSYNC_RELAY_URL"] = RELAY
    pull_proc = subprocess.Popen(
        [CS, "pull", "--wait", "--output", str(outdir)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env_b,
        text=True,
    )
    time.sleep(1.0)
    try:
        run(a, "push", "--file", str(src), timeout=TIMEOUT)
        try:
            pout, perr = pull_proc.communicate(timeout=TIMEOUT)
        except subprocess.TimeoutExpired:
            pull_proc.kill()
            raise Fail("file pull timed out")
        log(f"file pull out={pout!r} err={perr!r}")
        found = list(outdir.rglob("report.txt")) if outdir.exists() else []
        if not found:
            raise Fail(f"file was not written to --output. out={pout!r} err={perr!r}")
        text = found[0].read_text()
        if "file-payload-ok" not in text:
            raise Fail(f"file contents wrong: {text!r}")
        log(f"file saved at {found[0]}")
    finally:
        if pull_proc.poll() is None:
            pull_proc.kill()

    log("== image push while peer waits ==")
    png = bytes.fromhex(
        "89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c489"
        "0000000a49444154789c63000100000500010d0a2db40000000049454e44ae426082"
    )
    img_proc = subprocess.Popen(
        [CS, "--json", "pull", "--wait"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env_b,
        text=True,
    )
    time.sleep(1.0)
    try:
        run(a, "push", "--type", "image", input_bytes=png, timeout=TIMEOUT)
        try:
            iout, ierr = img_proc.communicate(timeout=TIMEOUT)
        except subprocess.TimeoutExpired:
            img_proc.kill()
            raise Fail("image pull timed out")
        log(f"image pull out={iout!r} err={ierr!r}")
        blob = (iout or "").lower()
        if "image" not in blob:
            raise Fail(f"image pull mismatch: out={iout!r} err={ierr!r}")
    finally:
        if img_proc.poll() is None:
            img_proc.kill()

    log("== logs / watch smoke ==")
    logs_out = run(a, "logs")
    log(f"logs bytes={len(logs_out)}")
    env_watch = os.environ.copy()
    env_watch["CLIPSYNC_HOME"] = str(b)
    env_watch["CLIPSYNC_RELAY_URL"] = RELAY
    env_watch["CLIPSYNC_NO_DAEMON"] = "1"
    watch = subprocess.Popen(
        [CS, "watch"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env_watch,
        text=True,
    )
    try:
        time.sleep(2.0)
        if watch.poll() is not None:
            wout, werr = watch.communicate()
            raise Fail(f"watch exited early rc={watch.returncode} out={wout!r} err={werr!r}")
        log("watch stayed up")
    finally:
        watch.send_signal(signal.SIGTERM)
        try:
            watch.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            watch.kill()

    log("== daemon run / status IPC ==")
    env_a = os.environ.copy()
    env_a["CLIPSYNC_HOME"] = str(a)
    env_a["CLIPSYNC_RELAY_URL"] = RELAY
    daemon = subprocess.Popen(
        [CS, "daemon", "run"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env_a,
        text=True,
    )
    try:
        deadline = time.time() + 20
        ipc_ok = False
        last = ""
        while time.time() < deadline:
            try:
                last = run(a, "--json", "status", timeout=5)
                data = json.loads(last.strip().splitlines()[-1])
                if data.get("daemon") is True:
                    log(f"daemon status: {data}")
                    ipc_ok = True
                    if data.get("connected") is True:
                        break
            except Exception:
                time.sleep(0.3)
            else:
                time.sleep(0.3)
        if not ipc_ok:
            raise Fail(f"daemon IPC status not ready: {last}")
        data = json.loads(last.strip().splitlines()[-1]) if last else {}
        if data.get("connected") is not True:
            raise Fail(f"daemon never connected to relay: {data}")
        run(a, "sync", "pause")
        paused = run_json(a, "status", timeout=5)
        if paused.get("paused") is not True:
            raise Fail(f"sync pause did not stick: {paused}")
        run(a, "sync", "resume")
        resumed = run_json(a, "status", timeout=5)
        if resumed.get("paused") is True:
            raise Fail(f"sync resume did not stick: {resumed}")
    finally:
        daemon.send_signal(signal.SIGTERM)
        try:
            daemon.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            daemon.kill()

    log("== leave ==")
    run(a, "room", "leave", "--yes")
    run(b, "room", "leave", "--yes")

    log("ALL CHECKS PASSED")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Fail as e:
        log(f"FAIL: {e}")
        raise SystemExit(1)
