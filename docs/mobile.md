# Native iOS and Android apps

The mobile clients are **separate repositories**. They share this repo’s E2EE
protocol through `crates/clipsync-mobile` (UniFFI).

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

`CLIPSYNC_CORE` can point at a non-sibling checkout of this repository.

Pair with the desktop CLI as usual (`clipsync room create` / `room join`).
