#!/usr/bin/env bash
# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
# Reconstruct two frozen native Linux environments, build once in each, compare.
set -euo pipefail
if [[ $# -gt 1 ]]; then
  echo "usage: $0 [report-parent-directory]" >&2
  exit 2
fi
repro_platform="${CLIPSYNC_REPRO_PLATFORM:-linux/amd64}"
case "$repro_platform" in
  linux/amd64|linux/arm64) ;;
  *) echo "CLIPSYNC_REPRO_PLATFORM must be linux/amd64 or linux/arm64." >&2; exit 2 ;;
esac
repro_repo="$(git rev-parse --show-toplevel)"
repro_report_parent="${1:-$repro_repo/target/reproducibility-report}"
mkdir -p "$repro_report_parent"
repro_report_parent="$(cd "$repro_report_parent" && pwd)"
# Preserve earlier reports under their original source identity. A failed run
# must never reuse old success markers alongside a newly recorded commit.
repro_report="$(mktemp -d "$repro_report_parent/run.XXXXXXXX")"
printf 'Report: %s\n' "$repro_report"
printf '%s\n' "$repro_platform" > "$repro_report/platform.txt"
repro_temporary="$(mktemp -d /tmp/clipsync-reconstruction.XXXXXXXX)"
repro_prefix="clipsync-reconstruction-$(date +%s)-$$"
cleanup() {
  docker image rm "${repro_prefix}:first" "${repro_prefix}:second" >/dev/null 2>&1 || true
  rm -rf -- "$repro_temporary"
}
trap cleanup EXIT
git -C "$repro_repo" bundle create "$repro_temporary/source.bundle" HEAD
git clone --quiet "$repro_temporary/source.bundle" "$repro_temporary/source"
git -C "$repro_temporary/source" show HEAD:Dockerfile.reproducible > "$repro_temporary/Dockerfile"
git -C "$repro_temporary/source" rev-parse HEAD > "$repro_report/source-commit.txt"
cp "$repro_temporary/Dockerfile" "$repro_report/Dockerfile.reproducible"
docker version > "$repro_report/docker-version.txt"
for repro_build in first second; do
  # The base image digest is shared. Apt and rustup reconstruct each environment
  # independently, without BuildKit layers or application objects from the first.
  docker build --platform "$repro_platform" --no-cache --progress plain --tag "${repro_prefix}:${repro_build}" \
    "$repro_temporary" 2>&1 | tee "$repro_report/environment-${repro_build}.log"
  mkdir -p "$repro_report/$repro_build"
  docker image inspect "${repro_prefix}:${repro_build}" \
    > "$repro_report/$repro_build/image.json"
  docker run --platform "$repro_platform" --rm \
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
