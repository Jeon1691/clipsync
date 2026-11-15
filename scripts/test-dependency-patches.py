#!/usr/bin/env python3
# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
"""Run upstream parser/wire/XML tests from disposable copies of our source patches."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import os

ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--toolchain", help="Optional rustup toolchain, e.g. 1.100.0")
    args = parser.parse_args()
    target = ROOT / "target/dependency-next"
    target.mkdir(parents=True, exist_ok=True)
    # Stable copy paths and a separate artifact directory keep compile-time
    # CARGO_MANIFEST_DIR snapshots consistent across repeated upstream test runs.
    work = target / "source-tests"
    work.mkdir(exist_ok=True)
    packages = sorted(source["name"] for source in
                      json.loads((ROOT / "vendor/sources.json").read_text())["sources"])
    actual = {p.name for p in (ROOT / "vendor").iterdir() if (p / "Cargo.toml").is_file()}
    if actual != set(packages) or len(packages) != len(actual):
        raise SystemExit("Every vendored test package must appear exactly once in sources.json")
    for name in packages:
        if (work / name).exists():
            shutil.rmtree(work / name)
        shutil.copytree(ROOT / "vendor" / name, work / name)
    members = ", ".join(f'"{name}"' for name in packages)
    patches = "".join(f'{name} = {{ path = "{name}" }}\n' for name in packages)
    (work / "Cargo.toml").write_text(
        f'[workspace]\nresolver = "2"\nmembers = [{members}]\n[patch.crates-io]\n{patches}'
    )
    # Freeze upstream dev-dependencies separately from ClipSync's product lock.
    # Copy on every invocation; a stale disposable lock cannot affect the run.
    shutil.copyfile(ROOT / "vendor/test-workspace.Cargo.lock", work / "Cargo.lock")
    command = ["cargo"] + ([f"+{args.toolchain}"] if args.toolchain else [])
    command += ["test", "--locked", "--manifest-path", str(work / "Cargo.toml"), "--lib", "--tests"]
    for name in ["askama_derive", "atspi-common", "zbus-lockstep", "zbus-lockstep-macros", "uniffi_internal_macros"]:
        command += ["-p", name]
    env = dict(os.environ, CARGO_TARGET_DIR=str(target / "patch-tests"))
    subprocess.run(command, cwd=ROOT, env=env, check=True)


if __name__ == "__main__":
    main()
