#!/usr/bin/env bash
# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
# Reconstruct two frozen native Linux environments, build once in each, compare.
set -euo pipefail
if [[ $# -gt 1 ]]; then
  echo "usage: $0 [report-directory]" >&2
  exit 2
fi
repro_repo="$(git rev-parse --show-toplevel)"
repro_report="${1:-$repro_repo/target/reproducibility-report}"
mkdir -p "$repro_report"
repro_report="$(cd "$repro_report" && pwd)"
repro_temporary="$(mktemp -d /tmp/clipsync-reconstruction.XXXXXXXX)"
repro_prefix="clipsync-reconstruction-$(date +%s)-$$"
cleanup() {
  docker image rm "${repro_prefix}:first" "${repro_prefix}:second" >/dev/null 2>&1 || true
  rm -rf -- "$repro_temporary"
}
trap cleanup EXIT
git -C "$repro_repo" bundle create "$repro_temporary/source.bundle" HEAD
git -C "$repro_repo" show HEAD:Dockerfile.reproducible > "$repro_temporary/Dockerfile"
git -C "$repro_repo" rev-parse HEAD > "$repro_report/source-commit.txt"
cp "$repro_temporary/Dockerfile" "$repro_report/Dockerfile.reproducible"
docker version > "$repro_report/docker-version.txt"
for repro_build in first second; do
  # The base image digest is shared. Apt and rustup reconstruct each environment
  # independently, without BuildKit layers or application objects from the first.
  docker build --no-cache --progress plain --tag "${repro_prefix}:${repro_build}" \
    "$repro_temporary" 2>&1 | tee "$repro_report/environment-${repro_build}.log"
  mkdir -p "$repro_report/$repro_build"
  docker image inspect "${repro_prefix}:${repro_build}" \
    > "$repro_report/$repro_build/image.json"
  docker run --rm \
    --mount "type=bind,src=$repro_temporary/source.bundle,dst=/input/source.bundle,readonly" \
    --mount "type=bind,src=$repro_report/$repro_build,dst=/report" \
    --env "REPEATABILITY_CONTAINER=${repro_prefix}:${repro_build}" \
    "${repro_prefix}:${repro_build}" bash -euo pipefail -c '
      git clone --quiet /input/source.bundle /build/source
      cd /build/source
      bash scripts/check-repeatable-build.sh --single /report
    '
done
cmp "$repro_report/first/native-packages.txt" "$repro_report/second/native-packages.txt"
cmp "$repro_report/first/first.sha256" "$repro_report/second/first.sha256"
cp "$repro_report/first/first.sha256" "$repro_report/reproduced.sha256"
echo 'All Linux workspace outputs match across both independently reconstructed environments.' \
  | tee "$repro_report/comparison.txt"
