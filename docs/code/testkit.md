# `testkit` — the shared test helpers

Source: `crates/testkit/` · crate `testkit` · **dev only**

`testkit` holds the fixtures and the process-global helpers the integration
tests share, so they are not duplicated across test binaries. It is **never
a runtime dependency**: the crates that need it pull it in under
`[dev-dependencies]`, and the crate is marked `publish = false`.

```rust
#![forbid(unsafe_code)]
#![warn(missing_docs)]
```

---

## 1. The modules

| module | source | what it holds |
|---|---|---|
| `media_fixtures` | `src/media_fixtures.rs` | synthetic image, audio, TLG and XP3 byte builders |
| `render` | `src/render.rs` | a headless window handle for the null render backend |
| `vm` | `src/vm.rs` | the process-global lock that serializes the TJS VM tests |

The crate root holds the XP3 archive builders, next to the media fixtures:

```rust
pub fn build_xp3(entries: &[(&str, &[u8], bool)]) -> Vec<u8>;
pub fn build_xp3_compressed_index(entries: &[(&str, &[u8], bool)]) -> Vec<u8>;
```

Each entry is `(name, data, zlib)`, where `zlib` selects a zlib-compressed
segment. The bytes follow the reference reader's layout — the index offset
at byte 11, the raw or zlib-compressed index block, and the `info` / `segm`
/ `adlr` chunks — so a test can mount a real archive in memory.

---

## 2. Why the fixtures live here

The image, audio and TLG builders are a deliberate **test-only copy** of the
reference encoders. They live in this dev-support crate so that:

* the production `media` crate contains no `#[cfg(test)]` code;
* every integration-test binary links only the fixtures it actually uses;
* the same synthetic bytes are available to the storage, media and render
  test suites without each of them writing its own encoder.

The Golomb bit-length table is computed at compile time — a `const fn`, no
lazy initialisation and no global mutable state — so the fixture builders
themselves hold no process-global state.

---

## 3. The public surface

```rust
// media fixtures
pub fn pattern(width: u32, height: u32) -> Vec<u8>;
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8>;
pub fn tiny_wav_mono(samples: &[i16], sample_rate: u32) -> Vec<u8>;
pub fn test_image(w: usize, h: usize) -> Vec<u8>;
pub fn encode_tlg5(img: &[u8], w: usize, h: usize, colors: usize, blockheight: usize) -> Vec<u8>;
pub fn encode_tlg6(img: &[u8], w: usize, h: usize, colors: usize) -> Vec<u8>;
pub const TLG5_MAGIC: [u8; 11];
pub const TLG6_MAGIC: [u8; 11];
pub const SDS_MAGIC: [u8; 11];

// render
pub fn headless_window() -> RawWindowHandle;

// vm
pub fn vm_lock() -> MutexGuard<'static, ()>;
```

`headless_window` builds a zeroed Xlib window identifier: the null pipeline
ignores the handle's contents, so it exercises every public entry point
without a display server.

`vm_lock` serializes the tests that touch the process-global TJS VM. The VM
keeps process-global state (the script cache, string interning), so two
engines may not run at once; the lock makes the second one wait. A poisoned
lock is recovered rather than propagated, so one failing test does not
cascade into every test that follows it.

---

## 4. Who uses it

| crate | what it takes |
|---|---|
| `compat` | the XP3 builders |
| `engine` | the XP3 builders and `headless_window` |
| `platform/fs` | the XP3 builders |
| `platform/media` | the image, audio and TLG fixtures |
| `platform/render` | `headless_window` |
| `vm` | `vm_lock` |

---

## 5. Related documents

| topic | document |
|---|---|
| the L4 host that uses `vm_lock` | [vm.md](vm.md) |
| the vendored C++ VM whose tests serialize on the lock | [tjs.md](tjs.md) |
| the test rules and the citation requirement | [`../../AGENTS.md`](../../AGENTS.md) §8, [`../rules/code_styles.md`](../rules/code_styles.md) |
| the Rust review checklist | [`../code-conduct.md`](../../docs.old/code-conduct.md) |
