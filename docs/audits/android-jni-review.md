# Android JNI seam audit — Kotlin launcher ↔ Rust engine

Read-only audit of the seam between `android/app/src/main/kotlin/dev/krkr/rs/`
and `crates/android/src/`, checked against `docs/android.md`,
`docs/runtime-config.md`, `android/README.md`,
`crates/platform/api/src/config.rs`, `crates/compat/src/event.rs`, the platform
backends and the `android-activity` 0.6.1 contract. No cargo/gradle was run
(host builds only; the Android target is never built on this machine).
Everything below was verified by reading both sides; items marked **[device]**
cannot be confirmed without hardware.

Findings are ranked: crash/black-screen > wrong behaviour > dead code/docs.

---

## Findings, ranked

### F5 (wrong behaviour, **[device]**) — surface recreation restarts the game from scratch

`MainEvent::InitWindow` always calls `handler.resumed`, which creates a fresh
`GameHost::open_with_preload` (`crates/android/src/host.rs:258-267`, `301-313`);
`TerminateWindow` only drops the running game (`host.rs:273-276`). A
`TerminateWindow` → `InitWindow` cycle (backgrounding, surface loss, some OEM
lifecycles) therefore remounts the archives and re-runs `startup.tjs`; all
in-memory progress since the last save is lost, silently. Whether the cycle
actually occurs with AGDK `GameActivity` needs a device. The VM has no
suspend/resume, so any fix is a design decision, not a patch — flagged, not
prescribed.

### F8 (documented divergence) — drag slop is fixed 16 physical px; the reference scales it by DPI

`gesture.rs:55` uses `DRAG_SLOP_PX = 16.0`; the reference computes
`(Device::getDPI() / 10)²` (`reference/cpp/core/environ/cocos2d/MainScene.cpp:494-496`
— verified present), i.e. ~32 px at 320 dpi. On a typical high-dpi phone our
drag resolves about twice as early as the reference's. This is a deliberate,
documented divergence (`docs/android.md` §5, "The 16 px slop is a fixed
physical-pixel value"), consistent with the module's "no display service"
constraint, but it is a D28-adjacent deviation that a reviewer should keep on
the books. `LONG_PRESS_MS = 1000` matches the reference exactly
(`MainScene.cpp:1097-1101` — verified).

### F10 (hygiene) — duplicated option vocabulary between Kotlin and Rust with no drift guard

`RuntimeOptions` mirrors `api::config` as hand-copied constants
(`RuntimeSettings.kt:37-64` vs `crates/platform/api/src/config.rs:102-131`).
**Verified identical today**, but the Kotlin app is not built by this
workspace (`android/README.md`), so nothing fails if one side changes. The
comments say "mirrors" and rely on review. A comment in `RuntimeSettings.kt`
is the only guard; consider a host-side test that greps the Kotlin constants
against the Rust ones, or accept the risk explicitly.

---

## Verified by reading both sides (no device needed)

1. All JNI static fields: name, type, declaring class, and the
   before-`super.onCreate` write order.
2. The full state-file contract: keys, vocabulary, null handling, atomicity,
   marker-vs-state mtime logic, and every Rust exit path's terminal write. A
   settings-field read that fails is reported as `Failed` and is distinct from
   an unset field (`read_settings_field` → `Err`, `crates/android/src/lib.rs`).
3. The log contract: format, encoding, line endings, rotation vs tail bounds;
   a log that cannot be opened is a reported startup failure.
4. Config ranges/defaults/names on both sides (frame rate `1..=1000`, preload,
   debug overlay); the no-knob claim for texture resolution and audio
   buffering.
5. Input event-kind parity against the winit backend and the compat layer's
   expectations; gesture state machine coherence and its reference citations
   (spot-checked against `MainScene.cpp:494-496`, `:820-829`, `:1097-1101`,
   `:1017-1146` — all present and consistent).
6. Absence of `#[allow(dead_code)]`, `todo!`, `unwrap`/`expect` on the JNI
   path, and target-gating (`#[cfg(target_os = "android")]` confined to
   `crates/android` and `crates/platform/platform/src/android.rs`, as allowed).

## Needs a device

* F5 frequency of `TerminateWindow → InitWindow` with AGDK `GameActivity`.
* Whether Android always delivers `ACTION_CANCEL` before `Pause` (phantom
  long-press window in `gesture_tick`).
* The whole documented "not verified on hardware" list in `docs/android.md`
  §8 still stands: motion-event ordering into the queue, synthesized clicks
  landing in a real KAG menu, `<filesDir>` writability, and the Logs screen
  reading real records.
