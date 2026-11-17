# Native iOS and Android apps

The mobile clients are **separate repositories**. They share this repo’s E2EE
protocol through `crates/clipsync-mobile` (UniFFI).

See the [mobile API reference](interfaces.md#mobile-uniffi-api) for exported
methods, return records, errors, clipboard callbacks, and lifecycle constraints.

| App | Repository |
| --- | --- |
| iOS (SwiftUI) | [Jeon1691/clipsync-ios](https://github.com/Jeon1691/clipsync-ios) |
| Android (Compose) | [Jeon1691/clipsync-android](https://github.com/Jeon1691/clipsync-android) |

Clipboard access stays in the OS layer:

- **iOS** — `UIPasteboard` (text and images) while the app is in the foreground
- **Android** — `ClipboardManager` plus a sticky foreground service after reboot

## Develop against this core

Clone the apps next to this repo:

```bash
git clone https://github.com/Jeon1691/clipsync.git
git clone https://github.com/Jeon1691/clipsync-ios.git
git clone https://github.com/Jeon1691/clipsync-android.git
```

From each app:

```bash
# iOS
cd clipsync-ios && ./scripts/sync-core.sh
open ClipSync.xcodeproj

# Android (needs cargo-ndk + Android NDK)
cd clipsync-android && ./scripts/sync-core.sh
# then open the folder in Android Studio
```

v0.2.0 upgrades UniFFI to 0.32.2 and requires Rust 1.95. Regenerate the
Swift/Kotlin bindings and rebuild the native library from the same core checkout;
do not mix older UniFFI 0.29 generated bindings with the new library. The exported
ClipSync methods, records, errors, and clipboard callback contract are preserved.
See [dependency migration](dependency-migration.md) for validation details.

`CLIPSYNC_CORE` can point at a non-sibling checkout of this repository.

Pair with the desktop CLI as usual (`clipsync room create` / `room join`).
Use `create_room_verified` / `join_room_verified` with a `SasVerifier` that shows
the verification code and asks the user to compare it with the other device;
see [interfaces](interfaces.md) and [security](security.md#verification-code-sas).
The older `create_room()` / `join_room()` still work but skip the comparison.
