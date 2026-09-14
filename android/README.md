# krkr-rs — Android frontend

Experimental. See **[`docs/android.md`](../docs/android.md)** for the full design,
the reasoning behind each decision, the milestones and the open questions. This
README is just how to build it.

## What this is

A native **Material 3** (Jetpack Compose) launcher that manages a library of
KiriKiri game folders and launches the real engine:

```
MainActivity      game library; "Add game" opens the system file picker (SAF)
KrkrGameActivity  hosts the engine surface (AGDK GameActivity → Rust android_main)
```

The engine is Rust. It is **not** built by Gradle: `cargo ndk` produces
`libkrkr_android.so`, which is dropped into
`app/src/main/jniLibs/arm64-v8a/` and loaded by `KrkrGameActivity`.

The library of picked folders is persisted as JSON in the app's private files
directory (`filesDir/games.json`) — see `GameLibrary.kt` and `docs/android.md`
§3.1. It exists so a folder only ever has to be found once.

This is **Android-app state only**. The engine itself takes a game directory and
nothing else, and the desktop build stays a plain CLI — `krkr-rs run <dir>` —
with no library, config file or game list anywhere in the Rust workspace.
`games.json` is never read by the engine and is not a shared schema; the Android
app is simply a UI in front of the same "run this directory" entry point.

**`arm64-v8a` only** — by decision, so there is no emulator (x86_64) build.
**Landscape-locked**, and storage uses `MANAGE_EXTERNAL_STORAGE` because the app
is side loaded (docs/android.md §4).

## Building

Prerequisites: Android SDK, NDK r27c, JDK 17, Rust with the `aarch64-linux-android`
target, `cargo-ndk`, `bison` (the C++ TJS2 VM generates its parsers at build time).

```bash
# 1. the engine .so (from the repository root)
cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs build -p krkr-android

# 2. the APK
cd android && ./gradlew assembleDebug
```

`cargo ndk` also wires `CC`/`AR`/the linker for the crates that compile C
(`blake3`, and opus). `crates/tjs2-sys/build.rs` honours a `CXX` override, so it
picks up the NDK clang automatically under `cargo ndk`.

The game's own files never ship in the APK: the user picks a folder at runtime.

## Current state

| Part | State |
|---|---|
| Compose M3 launcher + SAF picker + JSON library (`games.json`) | written, **not yet compiled** (no Android SDK on the dev box) |
| `crates/android` engine cdylib (`#[bevy_main]` + JNI) | not written — milestone M2 |
| Touch → mouse bridge, on-screen controls | not written — milestone M4 |
| MediaCodec video backend | not written — milestone M5 |
| Gradle `assembleDebug` verified locally | not yet |
| CI workflow (`.github/workflows/android.yml`) | written; runs **only** on the `android` branch and tags, so it does not build yet |

Known unverified pieces are listed honestly in `docs/android.md` §2 and §6 rather
than assumed to work.

## Permissions

`MANAGE_EXTERNAL_STORAGE` is declared because the engine's storage layer is
path-based and the games live in shared storage, while SAF hands out
`content://` URIs. This is option A in `docs/android.md` §4 and is the **decided**
approach, since the app is side loaded and Google Play's policy on that
permission does not apply. All URI→path logic lives in `SafPaths.kt` so the
resolution behaviour is in one place.
