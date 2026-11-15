#!/usr/bin/env python3
# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
"""Verify vendored sources against checksum-pinned archives and committed diffs."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import subprocess
import tarfile
import tempfile
import urllib.request

ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--offline", action="store_true", help="Use only Cargo's existing archive cache")
    args = parser.parse_args()
    catalog = json.loads((ROOT / "vendor/sources.json").read_text())
    cargo = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    for source in catalog["sources"]:
        filename = f'{source["name"]}-{source["version"]}.crate'
        cached = next((cargo / "registry/cache").glob(f"*/{filename}"), None)
        if cached:
            archive = cached.read_bytes()
        elif args.offline:
            raise SystemExit(f"Missing cached archive: {filename}")
        else:
            with urllib.request.urlopen(source["archive_url"], timeout=30) as response:
                archive = response.read()
        if hashlib.sha256(archive).hexdigest() != source["archive_sha256"]:
            raise SystemExit(f"Archive checksum mismatch: {filename}")
        with tempfile.TemporaryDirectory(prefix="clipsync-source-verification-") as temporary:
            work = Path(temporary)
            prefix = filename.removesuffix(".crate") + "/"
            with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
                for member in tar.getmembers():
                    if not member.isfile():
                        continue
                    if not member.name.startswith(prefix):
                        raise SystemExit(f"Unexpected archive prefix: {member.name}")
                    name = member.name[len(prefix):]
                    if PurePosixPath(name).is_absolute() or ".." in PurePosixPath(name).parts:
                        raise SystemExit(f"Unsafe archive path: {name}")
                    if name in catalog["excluded_archive_files"]:
                        continue
                    path = work / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(tar.extractfile(member).read())
            subprocess.run(["git", "init", "-q", str(work)], check=True)
            subprocess.run(["git", "-C", str(work), "apply", str(ROOT / "vendor" / source["patch"])], check=True)
            expected = {str(p.relative_to(work)): p.read_bytes() for p in work.rglob("*")
                        if p.is_file() and ".git" not in p.relative_to(work).parts}
            actual_root = ROOT / "vendor" / source["name"]
            actual = {str(p.relative_to(actual_root)): p.read_bytes() for p in actual_root.rglob("*") if p.is_file()}
            if actual != expected:
                raise SystemExit(f'Vendored source differs from its patch: {source["name"]}')
        print(f'Verified {source["name"]} {source["version"]} ({source["revision"]})')


if __name__ == "__main__":
    main()
