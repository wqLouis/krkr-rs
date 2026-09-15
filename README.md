<p align="center">
  <img src="icon.png" alt="krkr-rs" width="180">
</p>

<h1 align="center">krkr-rs</h1>

<p align="center">
  <b>A Rust KiriKiri2 Emulator
  from the <a href="https://github.com/2468785842/krkr2">krkr2</a> reference sources.</b>
</p>

<p align="center">
  <b>English</b> · <a href="README.zh-CN.md">简体中文</a>
</p>

---

**krkr-rs** is a native Rust reimplementation of the KiriKiri2 (TVP) engine, built to run games based on KiriKiri2 on Linux and Android.

By replacing the original C++/OpenGL/Cocos2dx framework with **Bevy** and **Vulkan**, it brings a modern rendering pipeline to Linux and Android while preserving engine compatibility.

* **Original TJS2 VM:** Uses the upstream C++ TJS2 virtual machine via a thin Rust FFI wrapper to ensure unmodified scripts and save files work seamlessly.
* **Rust Core:** Reimplementations for archive handling, TLG5/TLG6 image decoding, custom blend modes, font rasterization, Ogg/Opus audio, and KAG scenario tags.
* **Video Playback:** Handled via FFmpeg on desktop Linux and native `MediaCodec` on Android.
* **Android Frontend:** Includes a native Kotlin/Material 3 launcher using the system file picker (`arm64-v8a`).

**Quick Start**

```bash
# Build
cargo build

# Run a game
./target/debug/krkr-rs run "/path/to/game"

# Headless smoke test
./target/debug/krkr-rs run "/path/to/game" --headless

```

**Documentation & License**

* **`docs/`** — Design notes on rendering, fonts, NDK Android builds (`docs/android.md`), and parity verification (`docs/native_parity.md`).
* **License:** GPL-3.0 (the vendored TJS2 VM retains its upstream BSD-style license).

---

> **P.S.** macOS and iOS support aren't planned simply because I don't own a Mac or an iPhone to build and test on. Sorry to the Apple folks!
