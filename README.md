<p align="center">
  <img src="icon.png" alt="krkr-rs" width="180">
</p>

<h1 align="center">krkr-rs</h1>

<p align="center">
  <b>The KiriKiri2 (TVP) visual-novel engine, reimplemented in Rust + Bevy — ported
  from the <a href="https://github.com/2468785842/krkr2">krkr2</a> reference sources.</b>
</p>

<p align="center">
  <b>English</b> · <a href="README.zh-CN.md">简体中文</a>
</p>

---

krkr-rs plays **real, unmodified KiriKiri games** — the same `.xp3` archives,
`.tjs` scripts and `.ks` scenarios they ship with, untouched. Point it at a game
folder and it runs:

```bash
./target/debug/krkr-rs run "/path/to/game"
```

Menu → title → scenario → dialogue → saves → video, on real games. No game files
are modified, converted, or repacked.

## The breakthrough

**The engine was rebuilt, not wrapped.**

This is a **port**, not a clean-room project. The original KiriKiri2/TVP C++
sources — [krkr2](https://github.com/2468785842/krkr2) — are the reference for
every native semantic, file format and edge case here: behaviour was read out of
them and reproduced, not guessed or reinvented. What krkr-rs brings is a
different application around that behaviour: the original's Android platform
layer is built on **Cocos2d-x**, and krkr-rs replaces that whole stack with
**Rust + Bevy**, rendering through **Vulkan** (via Bevy/wgpu — which also brings
Metal, DirectX 12 and GLES along for free, and, with the same codebase, an
Android app).

Almost everything above the scripting language is a native Rust reimplementation
— each piece ported against its counterpart in the reference sources:

- **Archives and storage** — real `.xp3` reading, chained indexes, the
  `Storages` / `Scripts` APIs, and the case-insensitive semantics the games rely
  on.
- **Graphics** — KiriKiri's own **TLG5/TLG6** formats plus PNG/JPEG/WebP/BMP/GIF
  and more, the full `Layer` compositing model, every one of the engine's blend
  modes, affine layers, transitions and screen capture.
- **Text** — the `.tft` pre-rendered fonts and real font rasterization, with the
  baseline and measurement behaviour the layout code depends on.
- **Audio** — Ogg Vorbis and Opus, `.sli` looping, `WaveSoundBuffer` /
  `SoundChannel`, mixed and streamed through the platform's audio stack.
- **Video** — MP4/H.264/AAC: FFmpeg on desktop, **MediaCodec** on Android.
- **The KAG scenario language** — parser and tags, saves and loads, the in-game
  save/load and configuration screens.
- **Input, windows and the Win32-flavoured API surface** that the games call
  into, including hundreds of native members discovered by diffing against the
  original and machine-checked for parity.

One deliberate exception: the **TJS2 virtual machine is the original C++ one** —
vendored, minimally patched, and driven through a thin Rust FFI layer. Language
and script-level compatibility matters more here than rewriting the VM, and it is
what lets unmodified games run.

And it is not desktop-only: an **Android frontend** is in the tree — a native
**Material 3** launcher that picks a game folder through the system file picker
and hands it to the same engine, built for `arm64-v8a` with the NDK.

## With thanks

krkr-rs would not exist without **[krkr2](https://github.com/2468785842/krkr2)**
by [@2468785842](https://github.com/2468785842) — the KiriKiri2/TVP sources that
serve as the reference for this port.

Having the original implementation available to read is what makes a faithful
rewrite possible: native semantics, file formats and edge cases were ported from
it rather than guessed, and its behaviour is the standard this project measures
itself against. Huge thanks to the author and to everyone who has kept KiriKiri
alive.

## Building and running

```bash
cargo build

# windowed
./target/debug/krkr-rs run "/path/to/game"

# headless smoke test (no window or GPU; dumps the scene once)
./target/debug/krkr-rs run "/path/to/game" --headless
```

Movie playback uses the system FFmpeg libraries and can be disabled on systems
that cannot provide them (`cargo build --no-default-features`); everything else
builds either way.

Android: see [`android/README.md`](android/README.md).

## Documentation

- [`docs/`](docs) — design notes and investigations (portability, fonts,
  rendering, parity).
- [`docs/android.md`](docs/android.md) — the Android port: architecture,
  decisions, milestones, open questions.
- [`docs/native_parity.md`](docs/native_parity.md) — how the engine's native
  surface is machine-checked against the original.
- [`TODO.md`](TODO.md) — what is done, what is next, and the full investigation
  log.

## License

MIT OR Apache-2.0, at your option.
