#!/usr/bin/env python3
# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
"""Check first-party source/build files, or add missing project notices with --fix."""

import argparse
from pathlib import Path
import subprocess
import sys


SUFFIXES = {
    ".rs", ".py", ".sh", ".ps1", ".rb", ".nix", ".toml", ".yml", ".yaml",
    ".udl", ".h", ".c", ".cpp", ".swift", ".kt", ".kts",
}
SLASH_COMMENTS = {".rs", ".udl", ".h", ".c", ".cpp", ".swift", ".kt", ".kts"}


def sources(root):
    result = subprocess.run(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
        cwd=root, check=True, capture_output=True,
    )
    for name in sorted(set(result.stdout.decode().split("\0")) - {""}):
        path = root / name
        if path.is_file() and (path.suffix in SUFFIXES or path.name in {"Dockerfile", "PKGBUILD"} or name.startswith(".githooks/")):
            yield path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fix", action="store_true")
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    missing = []
    count = 0
    for path in sources(root):
        count += 1
        content = path.read_text(encoding="utf-8")
        beginning = "\n".join(content.splitlines()[:12])
        if "Copyright" in beginning and "SPDX-License-Identifier:" in beginning:
            continue
        if not args.fix:
            missing.append(str(path.relative_to(root)))
            continue
        # Do not replace an existing copyright or license with a different one.
        if "Copyright" in beginning or "SPDX-License-Identifier:" in beginning:
            missing.append(str(path.relative_to(root)) + " (review existing notice)")
            continue
        prefix = "//" if path.suffix in SLASH_COMMENTS else "#"
        notice = f"{prefix} Copyright the ClipSync contributors.\n{prefix} SPDX-License-Identifier: MIT\n"
        lines = content.splitlines(keepends=True)
        # Preserve executable shebangs and Docker's required first-line directive.
        insertion = 1 if lines and (lines[0].startswith("#!") or lines[0].startswith("# syntax=")) else 0
        lines.insert(insertion, notice)
        path.write_text("".join(lines), encoding="utf-8")
    for name in missing:
        print(f"Missing/review source notice: {name}", file=sys.stderr)
    if missing:
        return 1
    print(f"Checked {count} first-party source/build files.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
