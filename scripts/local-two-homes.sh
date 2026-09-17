#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
A="$ROOT/tmp/home-a"
B="$ROOT/tmp/home-b"
mkdir -p "$A" "$B"
export CLIPSYNC_RELAY_URL="${CLIPSYNC_RELAY_URL:-http://127.0.0.1:7600}"
echo "Home A: CLIPSYNC_HOME=$A"
echo "Home B: CLIPSYNC_HOME=$B"
echo "Start the relay, then:"
echo "  CLIPSYNC_HOME=$A cargo run -p clipsync-cli -- room create --no-auto-sync   # inits, prints code, waits in background"
echo "  CLIPSYNC_HOME=$B cargo run -p clipsync-cli -- room join <CODE> --no-auto-sync"
