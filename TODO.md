# TODO — open work

**Branch:** `migration/middle-layer-ash` · **repo:** `/mnt/DATA/Document/code/krkr-rs`
**Real game:** `/mnt/DATA/Games/Others/test` (`data.xp3`, `patch.xp3`, loose `patch.tjs`).

Committed through `d602c98`. The gate is green; `scripts/plugin_parity_allow.txt` is
**empty** (plugin parity 100%, 0 missing, 0 extras). This file lists only what is
still open. History is read with `git log`.

---

## 0. Conventions

* **Subagents must not run `cargo`** (`build`/`check`/`clippy`/`test`/`run`) — parallel builds once exhausted a 16 GB tmpfs and broke the tree. They may use `rustfmt`/`cargo fmt`, run prebuilt artifacts (`target/debug/krkr`, `target/debug/deps/*`), and run the Python scripts. The main agent runs the gate.
* **Only the main agent runs `scripts/surface_lock.py --update`.**
* **Comments** (D31): describe the code as it is — no history, no "used to", no rationale, no decision numbers. Keep `// SAFETY:`, invariants, and reference citations **in tests**.
* **Tests** carry a `reference/cpp/...:line` citation above every behaviour assertion. An assertion without one is not evidence.
* **Printed/logged text must not use internal jargon** (`L1`–`L4`, decision numbers). Tests are exempt.
* **D32:** only the Android backend may be stubbed; L3 may not stub and may not approximate. `#[allow(dead_code)]` is banned (one exception: `crates/compat/tests/common/mod.rs`).

## 1. The gate (run in this order)

```bash
cargo fmt --all -- --check
bash scripts/check-stateless.sh                                   # D9 guard
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p platform --features platform-linux
python3 scripts/surface_lock.py --update                          # then review + commit the diff
python3 scripts/surface_lock.py
python3 scripts/native_parity.py
python3 scripts/plugin_parity.py --no-doc
```

Real-game oracle (expect no `WARN`/`ERROR`, `texture >= 1`):

```bash
target/debug/krkr --headless --frames 10000 --base-dir /mnt/DATA/Games/Others/test
```

## 2. In-flight fixes (dispatched; re-verify and land them)

1. **`Layer.beginTransition` — game blocker.**
   `Sprite.tjs(290)` calls `super.beginTransition(name, withcildren, transsrc, options)`;
   the port raises `the transition source has no main image`. The reference passes
   `withchildren` (default **true**, `LayerIntf.cpp:9819-9821`) to `StartTransition`,
   which requires the destination/source main images **only when `!withchildren`**
   (`:7860-7868`, `TVPTransitionSourceAndDestinationMustHaveImage`); with
   `withchildren` it transitions the whole layer tree through the cache
   (`:7869-7876`). The port checks unconditionally and composites main images only
   (`crates/compat/src/natives/visual/layer/transitions.rs:159-166`, `:844-870`).
   Fix = parse `withchildren`, gate the check on it, and composite the source
   subtree when `withchildren` (add an L2 primitive if the subtree render needs one).
2. **Checkbox / radio / slider UI not rendered, silently.**
   The `GdiPlus` namespace is fixed, so `new GdiPlus.Appearance()` no longer raises,
   yet these widgets stay invisible. Trace the KAG drawing path
   (`system/{selectitem,messageframe,utility,advscreen,album,config}.tjs`) through
   `layer_ex/{draw,appearance,path}.rs` and `engine/src/raster.rs` for the exact
   cause (hatch/gradient paint, empty path, imageless layer, visibility/order, or an
   unreached KAG branch).

## 3. Input / IME — implemented, with documented imprecisions

Landed in `b1372cf`. These are knowingly imprecise; each needs a decision or a
reference source before it can be made exact:

* **Click / double-click timing.** The vendored reference declares
  `tTVPOnDoubleClickInputEvent` but never posts it (no `WM_LBUTTONDBLCLK`, no
  `GetDoubleClickTime`); the cocos2d path synthesizes `onClick` only
  (`TVPWindow.cpp:295-315`). The port reuses its Windows-derived sequence
  (`MOUSE_CLICK_SEQUENCE_FRAMES=30`, `MOUSE_CLICK_MAX_MOVE=4`) and fires
  `onDoubleClick` on the **second release** (Windows fires it on the second
  press). Needs the real thresholds from a shipped binary or the game's data.
* **Touch scaling / rotate.** `onTouchScaling`/`onTouchRotate` are registered but
  never synthesized: no reference platform recognizer posts
  `tTVPOnTouchScaling/RotateInputEvent`, and the `flag` values are never set
  anywhere in `reference/cpp/**`. Guessing them would be an invented
  approximation.
* **Layer-native action owner.** The reference layer `on*` natives also call
  `ActionOwner.action(event)` (`LayerIntf.cpp:476,9970-…`); the port forwards only
  the window path, because doing both would fire `action` twice per event. Decide
  how KAG expects the window and layer events to relate.
* **Window `onMouseEnter`.** Emitted on the first `PointerMoved` after a leave —
  there is no `CursorEntered` platform variant; the reference detects it in the
  stateful layer manager (`LayerManager.cpp:424-468`), not in a raw event.
* **`onMouseDown` also runs `PrimaryMouseMove`** in the reference (so a down can
  emit `onMouseMove`); the port relies on the platform's separate move events.

## 4. Refactor: reshape the internal code the plugin-extras audit left behind

The audit (`16583bc`) deleted 30 port-only plugin members. Two pieces of internal
code were kept for behaviour but should be reshaped to the house pattern
(a shared behaviour **struct** composed by the thin classes, D16 — not a
per-class flag list/registration copy):

* `window_ex` — `window_inert` (the reference `TJS_S_OK` stubs used by `minimize`,
  `maximize`, `showRestore`, `resetWindowIcon`, `setWindowIcon`, `bringTo`,
  `sendToBack`) and the `WindowFlags` set (`disable_resize`, `enable_nc_mouse_event`,
  `message_hook_all`, `sys_menu`, `overlay`) plus `WindowExState`/`with_window`.
  Collapse the inert stubs and the per-object flag map into one shared struct with
  named operations.
* `objthis_has_member` is the dynamic-probe helper for both `Window.registerExEvent`
  and `Pad.registerExEvent`; the probe results are no longer stored. Fold the probe
  into the same shared struct so the two callers do not each re-implement it.
* `ModuleSettings::max_texture_size` was already deleted (write-only; the reference
  `setMaxTextureSize` stores nothing). Confirm nothing else in `motion_player`
  retains write-only state.

## 5. L3 blockers — must behave like the reference

Verify current state first; several were fixed in the last wave.

| member | file | what the reference does | what it needs |
|---|---|---|---|
| `Window.registerMessageReceiver` | `natives/visual/window.rs` | registers a Win32 message receiver and delivers messages | a receiver registry + dispatch from the event loop |
| Platform-menu operations (`MenuItem`, `MenuItemEx`) | `natives/menu_item.rs`, `plugins/window_ex/menu_item.rs` | creates and operates a real `HMENU`; `popupEx` shows it | a synthetic menu-handle registry behind `Window` |
| `DrawDevice.interface` / `DrawDeviceForSteam.interface` | `natives/visual/draw_device.rs`, `plugins/steam/mod.rs` | returns the underlying `iTVPDrawDevice *` | an L1/L2 object to hand back, or a faithful stand-in |
| `MIDISoundBuffer.midiOut`, CD track selection | `natives/cdda_midi.rs` | opens a Win32 MIDI/CD device and reports state | a synthetic device reporting plausible state |
| Class-name constructor member | the ABI/factories | every reference class declares `Constructor()` | classes registered without a class-named method cannot be constructed via `super.X()` (the ABI installs the ctor dispatch only for a class-named method, `tjs2_abi.cpp:2268-2286`); check which classes still lack it |
| `layer_ex` movie playback | `plugins/layer_ex/movie.rs` | advances per frame | `movie_poll` is unit-tested but **no host wires it**, so a movie does not advance in game |
| `windowEx` dynamic events | `plugins/window_ex/window.rs` | fires `onMinimize`/`onResizing`/… when the window state changes | the port matches the dynamic lookup but never raises them (no Win32/NCHITTEST source on Linux) |
| Hatch fill, linear gradient, `LayerExFont` leading metrics | `engine/src/layer_ops.rs`, `plugins/layer_ex/{appearance,font}.rs` | exact pixels / real metrics | approximations today — forbidden by D32; give L2 the missing primitive, never patch L3 |

## 6. Platform stubs — Android only (permitted)

* `crates/platform/platform/src/android.rs` — window + event loop return `NotImplemented`; the AAudio output backend is real. Needs `ANativeWindow` + `GameActivity`, a size/scale source, a redraw request reaching the Vulkan surface, and input→`PlatformEvent` translation (`docs/migration/10-android-backends.md`).
* `crates/platform/media/src/video/mediacodec.rs` — written, **never compiled or run**; the host build uses a typed `stub`.

## 7. Deferred design work

* **L2 owns the frame** — `Engine::frame`/`Engine::run`/`GameFrame` designed, not built (`docs/migration/02 §3`, `07 Phase 9`). The current L3 clock is `platform::Clock`, a wall clock: a headless run of N frames reaches real time only by accident, which is why timer tests need care.
* **TJS VM Rust port** — deferred (`08 §10.7`).
* **L2 API snapshot + CI build step** — `cargo public-api` decided, unwired; CI runs only `cargo fmt --check` + the stateless guard, so the surface lock is a local gate.
* **Windowed audio on a real device** — owed.

## 8. Quality backlog

* **Harness hardening** — encode the §9 traps so they cannot recur: an `eval`-that-reads helper, a "not the primary layer" helper, and a fixture documenting the TJS `typeof` spellings.
* **Remaining bean-style accessors** — `InputState::set_pointer_visible`, `SoundChannel::looping`/`set_looping`, `Mixer::set_app_focused`/`set_app_minimized`, `Storage::set_xp3_extraction_filter`, `TimerState::set_now`, `VideoOverlay::set_bounds`/`set_looping`.
* **`VideoOverlay::new(layer, window, alpha, visible)`** — four positional args incl. a `bool`; wants a builder or `new(layer)` + field writes.
* **`#[non_exhaustive]`** — public structs/DTOs yes; enums L3 matches exhaustively no (a `_` arm turns a compile-time guarantee into a silent fallback).
* **`unsafe_code` lint** — `forbid` where there is no `unsafe`, `deny(unsafe_code)` + a file-level `allow` where the FFI lives.
* **`get_`-prefixed public API left:** `Tjs2Engine::get_member`, `get_class_names`; `Scene::get_province_pixel`.
* **Verify no decision-number tags returned** to plugin sources: `grep -rn "//.*\bD[0-9]\+\b" crates/compat/src`.

## 9. Traps that cost real time — read before triaging anything

1. **`typeof` uses TJS spellings:** `"Object"`, `"Integer"`, `"Real"`, `"String"`, `"Octet"`, `"void"`. Only an **absent member** yields `"undefined"` (`tjsInterCodeExec.cpp:1899-1904`); present-but-unset is `"void"` (`:2601-2628`).
2. **`void` is not `undefined`.** A bare `undefined` is a member lookup and raises; test absence with `typeof x === "undefined"` (or `typeof global.x`, since a bare `typeof <absent global>` also raises in this VM).
3. **`property` is a TJS keyword.** `var property = …` is a syntax error.
4. **Inside a subclass body, a bare native class name resolves to `this.<Name>`** — the copied non-static constructor function, whose `new` is `TJS_E_NOTIMPL`. Write `new global.Layer(…)`.
5. **`Env::eval` compiles in expression mode**: `eval("a = 1; a")` never runs the read. Write-then-read must be **separate executions**.
6. **The first layer created on a window is force-visible** (`LayerManager.cpp:161-172`).
7. **The compat binding is process-global and single** — drop the first `Env` before creating a second in one test.
8. **A dictionary swallows a missing read as `void`**; a custom object reports absence (`tjsDictionary.cpp:739-743`). Use `typeof o[k]`, not `get_member(…).is_err()`.
9. **Vendored C++ is not automatically right.** Where `reference/cpp/**` is a reverse-engineered reconstruction it can contradict the documented format and the shipped data (`json`, `motion_player` PSB layout). Say so explicitly, with the evidence.
10. **TJS2 does not bind `function` declarations inside an anonymous function body** (an IIFE wrapper breaks them). A **named global `function` declaration also binds `this` to the global object** — for a namespace helper, use an anonymous function expression assigned to the namespace member and recurse through `this`.
11. **`Layer.id`/`nativeId`, `Bitmap.id`, `Window.id`, `Font.id` do not exist** by design; identify through real members or the L2 `Scene`.

## 10. Verified behaviour worth not re-opening

XP3 parsing rejects malformed archives with typed errors (no panics); `media` decodes image/audio/video with no silent fallbacks; `wuvorbis` exposes no TJS surface by design; the null-plugin object exists only for DLLs never ported and announces itself; the compat binding is installed exactly once per process; `plugin_parity`/`native_parity` are at 100% with an empty allowlist.
