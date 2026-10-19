# Portability: per-target backend selection

Scope: how the workspace builds for more than one target without linking the
wrong platform code, and how the `android-target` gate check proves it.
Related: [`android.md`](android.md) (the Android frontend),
[`runtime-config.md`](runtime-config.md) (per-launcher configuration).

The engine is platform-agnostic. Only L1 (`platform`, `media`) holds
platform code, and it is selected by Cargo **features**, never by a runtime
probe or a `default`. Cargo resolves a package's features as a union over
every workspace member, so a backend chosen unconditionally is invisible until
something builds the other way: the workspace still resolves, but the Android
build links a desktop backend and the desktop build pulls the Android NDK, or
two mutually-exclusive backends are enabled together. Two things prevent that:

1. Each backend crate has `default = []` or a default that the other target
   turns off, so no consumer gets a backend it did not ask for.
2. `uv run --project scripts krkr-gate android-target` resolves the dependency
   graph of each composition root for the host and for
   `aarch64-linux-android` and rejects a leak. It compiles nothing and needs no
   NDK, so it runs in the host gate.

---

## 1. The backends

| crate | feature | backend | target |
|---|---|---|---|
| `platform` | `platform-linux` | winit window/event loop + cpal audio | Linux desktop |
| `platform` | `platform-android` | NDK AAudio; window/event-loop helpers are target-gated | Android |
| `media` | `video-ffmpeg` | FFmpeg via `ffmpeg-next` | Linux desktop |
| `media` | `video-mediacodec` | NDK MediaCodec + a raw `AMediaExtractor` shim | Android |

`platform` has `default = []`: every consumer must name exactly one platform
feature, so a platform can never be silently compiled for the wrong target.
`media` defaults to `video-ffmpeg` for the desktop; the Android arm turns the
default off and selects `video-mediacodec`. Selecting neither, or both, is a
hard `compile_error!` in `crates/platform/media/src/video/mod.rs`.

Within `platform`, the window/surface and event loop are the parts split this
way. The input-translation helpers (`PlatformWindow::physical_key_from_android`,
`touch_phase_from_android`, `mouse_button_from_android`) name no Android type,
so they compile and are unit-tested on the host under `platform-android`.
The Android-only NDK dependencies are target-scoped in the manifest, so the
desktop host never has them in its graph:

```toml
[target.'cfg(target_os = "android")'.dependencies]
ndk = { version = "0.9", features = ["audio"] }   # AAudio, in `platform`
ndk = { version = "0.9", features = ["media"] }   # MediaCodec, in `media`
```

---

## 2. How each root selects a backend

`crates/app` is the desktop composition root. It selects its features
per-target rather than relying on the workspace default:

```toml
[target.'cfg(target_os = "linux")'.dependencies]
platform = { workspace = true, features = ["platform-linux"] }
media = { workspace = true, features = ["video-ffmpeg"] }

[target.'cfg(target_os = "android")'.dependencies]
platform = { workspace = true, features = ["platform-android"] }
media = { path = "../platform/media", default-features = false, features = ["video-mediacodec"] }
```

The Android arm names `media` by path, not `workspace = true`, because edition
2024 forbids a member from overriding a workspace dependency's
`default-features`.

`crates/android` is the Android composition root: it owns `android_main`, the
`android-activity` `GameActivity` loop and the JNI reads, and drives the shared
`app` host (see [`android.md`](android.md) §3). Its Android-only dependencies
(`android-activity`, `jni`, `app`, `platform`, `env_logger`) are target-scoped
so the crate still compiles on the desktop host as the host-tested settings and
state modules.

There is no wasm target. The Rust side could compile for wasm only for the
pure type crates; the graphical stack is Vulkan and the VM is the vendored C++
TJS2, which has no browser toolchain in this tree.

---

## 3. The guard — `android-target`

The check drives `cargo tree -p <root> --target <triple> -e normal,dev` for
each root and asserts the resolved feature set. It uses `cargo tree` rather than
`cargo metadata --filter-platform` deliberately: `cargo metadata` computes each
package's features as a union over **all** workspace members and does not filter
that union by target, so it reports both platform features for both targets even
when the manifests are correct.

Checked roots (the composition roots and the crate whose integration tests link
`media`):

* Android target (`aarch64-linux-android`): `android`, `app`, `compat`.
* Host target: `app`, `compat` (`android` is excluded — its Android-only
  dependencies are absent on the desktop by construction).

For each root it enforces:

* Android must resolve `platform/platform-android` and `media/video-mediacodec`,
  and must not resolve `platform/platform-linux` or `media/video-ffmpeg`.
* The host must resolve `platform/platform-linux` and `media/video-ffmpeg`, and
  must not resolve the Android features.
* `platform-linux` and `platform-android` are never both enabled, and the same
  for the two `media` video features.

Run it directly, or as part of the main gate (AGENTS §0):

```bash
uv run --project scripts krkr-gate android-target
```

---

## 4. The Android target is not built on the development host

No NDK and no Android SDK are installed on the development host, and none may
be installed (AGENTS §0, `TODO.md` §14). The Android target itself is therefore
**never built or tested here**. Android-only code is written and reviewed here
and is compiled and run on a machine that has an NDK; anything not compiled is
documented as **unverified**, never described as working. The host bar is
`cargo check --workspace` staying green, the `platform-android` host-feature
steps in the gate, and the `android-target` check.

The vendored C++ VM is target-aware: `crates/tjs/build.rs` reads the Cargo
target (`CARGO_CFG_TARGET_OS`, `TARGET`) and only emits the desktop C++ runtime
flags (`-lc++ -lc++abi -pthread`) when the **target** is desktop Linux, and its
oniguruma `config.h` sizes come from the target, not the build host. A
cross-build still needs an NDK `CC`/`CXX` and is not exercised on this host.

The Android build itself is a `cargo build --target aarch64-linux-android` with
the NDK toolchain and a Gradle package; a real device run is required before any
release.
