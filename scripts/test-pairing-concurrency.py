#!/usr/bin/env python3
# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
"""Run three complete parallel relay e2e suites under one bounded deadline."""

import argparse
from contextlib import ExitStack
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


def remaining(deadline):
    seconds = deadline - time.monotonic()
    if seconds <= 0:
        raise TimeoutError("global deadline exceeded")
    return seconds


def cleanup(children):
    """Kill active process groups, then allow five seconds total to reap them."""
    for child in children:
        if child.poll() is None:
            try:
                if os.name == "posix":
                    os.killpg(child.pid, signal.SIGKILL)
                else:
                    child.kill()
            except ProcessLookupError:
                pass
    deadline = time.monotonic() + 5
    unreaped = []
    for child in children:
        try:
            child.wait(timeout=max(0.01, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            unreaped.append(child.pid)
    return unreaped


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--executable", type=Path, help="reuse an exact compiled e2e binary")
    parser.add_argument("--output-dir", type=Path, default=Path("target/pairing-concurrency"))
    parser.add_argument("--timeout", type=float, default=180, help="global deadline in seconds")
    args = parser.parse_args()
    if not 0 < args.timeout < float("inf"):
        parser.error("--timeout must be positive and finite")

    args.output_dir = args.output_dir.resolve()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    started = time.monotonic()
    deadline = started + args.timeout
    children = []
    copies = []
    build = None
    report = {"status": "failed", "timeout_seconds": args.timeout, "build": None, "copies": []}

    def interrupted(signum, _frame):
        raise InterruptedError(f"interrupted by signal {signum}")

    signal.signal(signal.SIGTERM, interrupted)
    with ExitStack() as files:
        try:
            executable = args.executable
            if executable is None:
                command = [
                    "cargo", "test", "-p", "clipsync-relay", "--test", "e2e",
                    "--locked", "--no-run", "--message-format=json",
                ]
                artifacts = args.output_dir / "cargo-artifacts.jsonl"
                build_log = args.output_dir / "cargo-build.log"
                report["build"] = {"command": command, "artifacts": str(artifacts), "log": str(build_log)}
                build = subprocess.Popen(
                    command,
                    cwd=Path(__file__).resolve().parent.parent,
                    stdout=files.enter_context(artifacts.open("w")),
                    stderr=files.enter_context(build_log.open("w")),
                    start_new_session=os.name == "posix",
                )
                children.append(build)
                if build.wait(timeout=remaining(deadline)) != 0:
                    raise RuntimeError(f"cargo build failed; see {build_log}")
                executables = set()
                for line in artifacts.read_text().splitlines():
                    artifact = json.loads(line)
                    if (
                        artifact.get("reason") == "compiler-artifact"
                        and artifact["target"]["name"] == "e2e"
                        and "test" in artifact["target"]["kind"]
                        and artifact.get("executable")
                    ):
                        executables.add(artifact["executable"])
                if len(executables) != 1:
                    raise RuntimeError(f"expected one e2e executable, found {len(executables)}")
                executable = Path(executables.pop())

            command = [str(executable.resolve()), "--nocapture", "--test-threads=10"]
            report["executable"] = command[0]
            report["command"] = command
            for number in range(1, 4):
                remaining(deadline)
                log = args.output_dir / f"copy-{number}.log"
                child = subprocess.Popen(
                    command,
                    stdout=files.enter_context(log.open("w")),
                    stderr=subprocess.STDOUT,
                    start_new_session=os.name == "posix",
                )
                children.append(child)
                copies.append(child)
                report["copies"].append({"copy": number, "pid": child.pid, "log": str(log)})

            while True:
                codes = [child.poll() for child in copies]
                if any(code is not None and code != 0 for code in codes):
                    raise RuntimeError(f"e2e copy failed with exit codes {codes}")
                if all(code == 0 for code in codes):
                    report["status"] = "passed"
                    break
                time.sleep(min(0.1, remaining(deadline)))
        except (TimeoutError, subprocess.TimeoutExpired) as error:
            report.update(status="timed_out", error=str(error))
        except (KeyboardInterrupt, InterruptedError) as error:
            report.update(status="interrupted", error=str(error) or "keyboard interrupt")
        except (OSError, ValueError, RuntimeError) as error:
            report["error"] = str(error)
        finally:
            unreaped = cleanup(children)
            if unreaped:
                report.update(status="failed", unreaped_pids=unreaped)
            if build is not None:
                report["build"]["exit_code"] = build.returncode
            for copy, child in zip(report["copies"], copies):
                copy["exit_code"] = child.returncode
            report["elapsed_seconds"] = round(time.monotonic() - started, 3)
            destination = args.output_dir / "report.json"
            destination.write_text(json.dumps(report, indent=2) + "\n")

    print(json.dumps(report, indent=2), flush=True)
    print(f"Pairing concurrency report: {destination}", flush=True)
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main())
