# Native surface parity

> **Generated** by `scripts/native_parity.py` on 2026-10-12 — do not edit by hand.

Machine-diff of the reference C++ native class surface (`reference/cpp/core/**/*.cpp`) against the native classes the Rust engine registers. A *missing* member exists in the reference but is not registered by Rust; an *extra* member is registered by Rust but has no reference counterpart (engine-internal helpers such as `id` and `nativeId` are expected here).

## Usage

```sh
python3 scripts/native_parity.py                 # check + regenerate this report
python3 scripts/native_parity.py --no-doc        # check only
python3 scripts/native_parity.py --update-allow  # accept current gaps after review
```

The checker exits non-zero when a missing member is **not** listed in `scripts/native_parity_allow.txt` (a regression). When an allowlisted member is implemented it prints a stale-entry warning so the allowlist can shrink. Coverage can therefore only move in one direction.

## Coverage summary

| metric | value |
| --- | ---: |
| tracked classes | 22 |
| reference members | 636 |
| matched | 636 |
| **missing** | **0** |
| extra (Rust only) | 2 |
| coverage | **100.0%** |

## Per-class coverage

| class | ref | rust | matched | missing | extra | coverage |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `Layer` | 149 | 149 | 149 | 0 | 0 | 100.0% |
| `Bitmap` | 19 | 19 | 19 | 0 | 0 | 100.0% |
| `Window` | 82 | 82 | 82 | 0 | 0 | 100.0% |
| `Font` | 21 | 21 | 21 | 0 | 0 | 100.0% |
| `Timer` | 5 | 5 | 5 | 0 | 0 | 100.0% |
| `System` | 49 | 49 | 49 | 0 | 0 | 100.0% |
| `Debug` | 12 | 12 | 12 | 0 | 0 | 100.0% |
| `Plugins` | 3 | 3 | 3 | 0 | 0 | 100.0% |
| `VideoOverlay` | 69 | 69 | 69 | 0 | 0 | 100.0% |
| `AsyncTrigger` | 5 | 5 | 5 | 0 | 0 | 100.0% |
| `MenuItem` | 21 | 21 | 21 | 0 | 0 | 100.0% |
| `Storages` | 13 | 13 | 13 | 0 | 0 | 100.0% |
| `Scripts` | 10 | 10 | 10 | 0 | 0 | 100.0% |
| `KAGParser` | 24 | 26 | 24 | 0 | 2 | 100.0% |
| `WaveSoundBuffer` | 32 | 32 | 32 | 0 | 0 | 100.0% |
| `Pad` | 24 | 24 | 24 | 0 | 0 | 100.0% |
| `Rect` | 19 | 19 | 19 | 0 | 0 | 100.0% |
| `Clipboard` | 2 | 2 | 2 | 0 | 0 | 100.0% |
| `ImageFunction` | 12 | 12 | 12 | 0 | 0 | 100.0% |
| `BitmapLayerTreeOwner` | 36 | 36 | 36 | 0 | 0 | 100.0% |
| `CDDASoundBuffer` | 14 | 14 | 14 | 0 | 0 | 100.0% |
| `MIDISoundBuffer` | 15 | 15 | 15 | 0 | 0 | 100.0% |

## Missing members by class

_None for tracked classes._
## Extra members by class

Rust registrations with no reference counterpart; usually intentional engine internals.

### `KAGParser` — 2 extra

`multiLineTagEnabled`, `paramMacros`

## Reference classes not registered in Rust

| class | reference members |
| --- | ---: |
| `BasicDrawDevice` | 6 |
| `PassThroughDrawDevice` | 7 |
| `PhaseVocoder` | 6 |
| `WaveFlags` | 4 |

## Rust-only registered classes

These builders have no native C++ counterpart in `reference/cpp/core` (TJS-level or plugin classes).

| class | Rust members | source |
| --- | ---: | --- |
| `SoundBuffer` | 0 | `crates/compat/src/natives/sound.rs#SoundBuffer` |
| `SoundChannel` | 0 | `crates/compat/src/natives/sound.rs#SoundChannel` |
