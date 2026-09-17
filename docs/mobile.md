# Native iOS and Android apps

ClipSync’s mobile apps are native (SwiftUI on iOS, Jetpack Compose on Android).
They share the desktop E2EE protocol through a Rust UniFFI crate
(`crates/clipsync-mobile`): SPAKE2 pairing, AES-256-GCM, and the same WebSocket
relay at `https://clipsync.develicit.dev`.

Clipboard access stays in the OS layer:

- **iOS** — `UIPasteboard` (text and images) while the app is in the foreground
- **Android** — `ClipboardManager` plus a sticky foreground service that comes
  back after reboot (`BootReceiver`)

## Build the Rust library

```bash
# Swift + Kotlin bindings (host library)
cargo build -p clipsync-mobile
cargo run -p clipsync-mobile --bin uniffi-bindgen -- generate \
  --library target/debug/libclipsync_mobile.dylib \
  --language swift --out-dir apps/ios/ClipSync/Generated
cargo run -p clipsync-mobile --bin uniffi-bindgen -- generate \
  --library target/debug/libclipsync_mobile.dylib \
  --language kotlin --out-dir apps/android/app/src/main/java

# Device libraries
./scripts/build-mobile.sh ios      # needs Xcode / iOS targets
./scripts/build-mobile.sh android  # needs cargo-ndk and the Android NDK
```

## Open the apps

- iOS: `open apps/ios/ClipSync.xcodeproj` then select your team and run on a device.
- Android: open `apps/android` in Android Studio. Place
  `libclipsync_mobile.so` under `app/src/main/jniLibs/arm64-v8a/`.

Pair with the desktop CLI as usual (`clipsync room create` / `room join`).
