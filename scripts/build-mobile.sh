#!/usr/bin/env bash
# Generate UniFFI bindings and native libraries for iOS and Android.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

mode="${1:-all}"

echo "generating UniFFI scaffolding"
cargo build -p clipsync-mobile
HOST_LIB="target/debug/libclipsync_mobile.dylib"
if [[ ! -f "$HOST_LIB" ]]; then
  HOST_LIB="target/debug/libclipsync_mobile.so"
fi
BINDGEN=(cargo run -q -p clipsync-mobile --bin uniffi-bindgen --)
mkdir -p apps/ios/ClipSync/Generated apps/android/app/src/main/java
"${BINDGEN[@]}" generate --library "$HOST_LIB" --language swift --out-dir apps/ios/ClipSync/Generated
"${BINDGEN[@]}" generate --library "$HOST_LIB" --language kotlin --out-dir apps/android/app/src/main/java --no-format

if [[ "$mode" == "all" || "$mode" == "ios" ]]; then
  echo "iOS: aarch64-apple-ios + sim"
  rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios >/dev/null
  cargo build -p clipsync-mobile --release --target aarch64-apple-ios
  cargo build -p clipsync-mobile --release --target aarch64-apple-ios-sim || true
  mkdir -p apps/ios/ClipSync/Generated apps/ios/lib
  "${BINDGEN[@]}" generate \
    --library target/aarch64-apple-ios/release/libclipsync_mobile.a \
    --language swift \
    --out-dir apps/ios/ClipSync/Generated
  cp target/aarch64-apple-ios/release/libclipsync_mobile.a apps/ios/lib/libclipsync_mobile.a
  echo "wrote apps/ios/lib and Swift bindings"
fi

if [[ "$mode" == "all" || "$mode" == "android" ]]; then
  if ! command -v cargo-ndk >/dev/null 2>&1; then
    echo "cargo-ndk not found; install with: cargo install cargo-ndk" >&2
    echo "skipping Android native library build" >&2
  else
    echo "Android: arm64-v8a"
    cargo ndk -t arm64-v8a -o apps/android/app/src/main/jniLibs \
      build -p clipsync-mobile --release
    mkdir -p apps/android/app/src/main/java
    lib=$(find apps/android/app/src/main/jniLibs -name 'libclipsync_mobile.so' | head -1)
    if [[ -n "$lib" ]]; then
      "${BINDGEN[@]}" generate \
        --library "$lib" \
        --language kotlin \
        --out-dir apps/android/app/src/main/java
    fi
    echo "wrote Android jniLibs and Kotlin bindings"
  fi
fi
