<p align="center">
  <img src="icons/ic_launcher_foreground%20%C2%B7%20108%20%C3%97%20108%20%C2%B7%20transparent.svg" alt="krkr-rs" width="180">
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

By replacing the original C++/OpenGL/Cocos2dx framework with a **pure Rust + Vulkan** engine, it brings a modern rendering pipeline to Linux and Android while preserving engine compatibility.

* **Original TJS2 VM:** Uses the upstream C++ TJS2 virtual machine via a thin Rust FFI wrapper to ensure unmodified scripts and save files work seamlessly.
* **Rust Core:** Reimplementations for archive handling, TLG5/TLG6 image decoding, custom blend modes, font rasterization, Ogg/Opus audio, and KAG scenario tags.
* **Native Vulkan Renderer:** A Rust rendering engine built directly on Vulkan (`ash` + `ash-window`). Text is rasterized on the CPU into the layer bitmap.
* **Video Playback:** Handled via FFmpeg on desktop Linux and native `MediaCodec` on Android.
* **Android Frontend:** Includes a native Kotlin/Material 3 launcher using the system file picker (`arm64-v8a`).

**Quick Start**

```bash
# Build
cargo build

# Run a game
./target/debug/krkr --base-dir "/path/to/game"

# Headless smoke test
./target/debug/krkr --headless --frames 1000 --base-dir "/path/to/game"

```

The build needs Rust stable, a C++17 compiler, Zig, bison and the system FFmpeg/ALSA/libc++
development packages; [`AGENTS.md`](AGENTS.md) §1.2 lists the exact set and the environment
variables the test suite uses.

**Documentation & License**

* **`docs/`** — the architecture (`docs/arch/`), the module map (`docs/code/`), the rules (`docs/rules/`), and the parity verification (`docs/native_parity.md`).
* **The reference engine** — the behaviour oracle every compatibility claim is measured against — is a separate checkout and is **not** part of this repository: `git clone https://github.com/2468785842/krkr2 reference`. The `reference/cpp/...:line` citations in the tests point into it.
* **License:** GPL-3.0 (the vendored TJS2 VM retains its upstream BSD-style license).

---

> **P.S.** macOS and iOS support aren't planned simply because I don't own a Mac or an iPhone to build and test on. Sorry to the Apple folks!
