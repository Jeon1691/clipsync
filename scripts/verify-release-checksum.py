#!/usr/bin/env python3
# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
"""Verify exactly one GNU text/binary SHA-256 record for the expected archive."""

import argparse
import hashlib
from pathlib import Path
import re


def verify_checksum(archive, checksum):
    archive, checksum = Path(archive), Path(checksum)
    record = checksum.read_bytes().decode("ascii")
    match = re.fullmatch(r"([0-9a-fA-F]{64}) ([ *])([^\r\n]+)(?:\r?\n)?", record)
    if not match or match[3] != archive.name:
        raise ValueError("checksum must contain exactly one record for the expected archive basename")
    digest = hashlib.sha256()
    with archive.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    if match[1].lower() != digest.hexdigest():
        raise ValueError("checksum digest does not match the release archive")
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("checksum", type=Path)
    args = parser.parse_args()
    try:
        verify_checksum(args.archive, args.checksum)
    except (OSError, ValueError) as error:
        parser.exit(1, f"checksum: {error}\n")
    print(f"Verified SHA-256 for {args.archive.name}")


if __name__ == "__main__":
    main()
