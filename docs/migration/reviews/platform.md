# Platform / host code review — `krkr-platform` (L1), `krkr-app` (composition root)

Scope: `crates/krkr-platform/` (L1 platform core) and `crates/krkr-app/`
(composition root), with their tests (`krkr-platform/tests/*`,
`krkr-app/tests/*`) and the D9 guard `scripts/check-stateless.sh`.

Read against `docs/migration/02-target-architecture.md` (§1.1, §2.1–2.2,
§3–§4, §6, §7, §9), `docs/migration/03-middle-layer-api.md` (§6, §8, §9)
and `docs/migration/07-roadmap.md` (revision note).

Method: source read-only. Only `cargo check` was run, as permitted:

* `cargo check -p krkr-platform --all-targets` — clean, no warnings;
* `cargo check -p krkr-app --all-targets` — clean, no warnings;
* `cargo check -p krkr-platform --no-default-features --features platform-android --all-targets` — clean;
* `--no-default-features` (neither) and `--features platform-android`
  (both) both fail to compile, as intended (see finding 4 for the noise).

No `build`/`run`/`test` was executed.

Severity legend:

| Severity | Meaning |
|----------|---------|
| **Critical** | Can panic/abort or cause memory-unsafe behaviour from a normal public call. |
| **High** | Layer/D11/D13 violation with functional consequences (wrong data, silently wrong platform, API that cannot be relied on). |
| **Medium** | Correctness edge cases, fragile lifecycle, unvalidated public input, missing guard/coverage. |
| **Low** | Local idiom/robustness/consistency; worth fixing opportunistically. |

Overall the two crates are structurally clean: `krkr-platform` has no
mutable statics/`thread_local!`/hidden caches, `#![forbid(unsafe_code)]` at
the root, and no L2/L3/L4 dependency; the winit types are confined to
`linux.rs`; the D13 "neither/both features" guard works. The problems are
(a) the composition root actually contains the L2 engine state, (b) several
silent fallbacks at the handle/DPI boundary, and (c) the D9 guard does not
cover the crate it is supposed to guard.

---

## 1. The L2 engine state store lives in `krkr-app`, not `krkr-engine` — High

`crates/krkr-app/src/lib.rs:137-174` defines `pub struct Host`, whose fields
are exactly the `Engine` of `docs/migration/03-middle-layer-api.md:457-479`:
the L1 stateless services (`pipeline`, `fs`, `media`) plus all L2 state
(`scene`, `render_scene`, `render_state`, `surface`, bitmap→texture table,
input snapshot). `Host::step` / `sync_dirty_bitmaps` /
`prune_stale_textures` / `reproject_render_scene`
(`crates/krkr-app/src/lib.rs:242-262, 273-302, 381-462`) are the L2
projection seam described in `02-target-architecture.md` §2.2 and §7, and
`krkr-engine` already owns `projection::project_scene`, `Scene` and depends
on `krkr-render`/`krkr-fs`.

The architecture is explicit that this is not where it belongs:
`02-target-architecture.md:241` ("`krkr-app` — thin binary: select platform,
wire L1..L4, start the loop") and `:263` ("Composition/wiring — `krkr-app` —
no platform logic"). `krkr-app`'s own manifest even names the drift:
`crates/krkr-app/Cargo.toml:3` — *"krkr-rs host: L2 engine host + L1 render
assembly"*.

Why it matters: the composition root is not thin; the L2 state/API now has
two candidate homes and `krkr-engine`'s `Engine` is still missing. The
integration tests (`crates/krkr-app/tests/host.rs`, `tests/common/mod.rs`)
therefore exercise the root crate instead of `krkr-engine`, and any second
frontend (the Android cdylib, `krkr-vm`) must reconstruct `Host` rather than
reuse the engine.

Fix: move `Host` (and `HostConfig`/`StepReport`/`HostError`) into
`krkr-engine` as the documented `Engine`/`EngineConfig`, adding the
`krkr-media` dependency it needs; leave surface/handle creation in the root
(or pass a prebuilt `Surface`/`RenderState` into `Engine::new` so L2 never
touches a window handle); move the host tests to `crates/krkr-engine/tests`.
`krkr-app` then only parses the CLI and wires `Engine` + `krkr_platform::run`.

## 2. Silent fallback on missing raw handles, and a fabricated headless handle — High (D11)

`crates/krkr-platform/src/linux.rs:36-41` and `:44-49` collapse the
`HasWindowHandle`/`HasDisplayHandle` `Result` to `Option` with `.ok()`,
dropping the `HandleError`. The consumer then treats the absence as
"headless": `crates/krkr-app/src/lib.rs:199-206` substitutes
`krkr_platform::headless_window_handle()`, and
`crates/krkr-platform/src/window.rs:11-13` fabricates
`RawWindowHandle::Web(WebWindowHandle::new(0))` on **every** target. Because
`crates/krkr-app/src/main.rs:217-218` passes those `Option`s straight
through, a handle lookup failure on a real window silently produces a
surface bound to a fake Web canvas instead of failing. This is precisely the
"no silent fallback" rule of `02-target-architecture.md` §9.5 (D11).

Fix: make `raw_window_handle()`/`raw_display_handle()` return
`Result<RawWindowHandle, PlatformError>` (new `PlatformError::Handle`)
and propagate through `DesktopApp::resumed` (`Flow::Exit` on error). Make
`HostConfig`/`EngineConfig` carry an explicit `headless: bool` instead of
inferring headless from `window_handle == None`, and only build the
synthetic handle when the backend is `Null` and headless was requested; a
real backend with no handle must be an error.

## 3. Input coordinates are physical but documented/typed as logical — High

`crates/krkr-platform/src/input.rs:29-31, 36-39, 70-73` state the
coordinates are "logical window coordinates", but `map_input` copies the
winit values verbatim, and all of them are **physical** pixels:
`CursorMoved.position` (`linux.rs:200-206`), `MouseScrollDelta::PixelDelta`
(`linux.rs:215`) and `Touch.location` (`linux.rs:226-227`). Nothing calls
`window.scale_factor()`, and `PlatformWindow` exposes no `scale_factor()`
accessor (`linux.rs:34-60`). `PlatformEvent::Resized` and
`PlatformWindow::size()` are physical, so the input space is the odd one out.
On any HiDPI display, L3 hit-testing against logical layer rects will be off
by the scale factor.

Fix: choose one space and enforce it. Given `size()`/`Resized` are physical,
either (a) convert `position`/`delta`/`touch.location` to logical using the
current scale factor before constructing the `InputEvent`, or (b) rename the
docs to "physical" and add `PlatformWindow::scale_factor() -> f64` so L3 can
convert consistently. Either way, `ScaleFactorChanged` must update whatever
scale the mapping uses (see finding 7).

## 4. D13 Android platform selection is not actually enforced — High

`crates/krkr-platform/Cargo.toml:12-15` defaults to `platform-linux`, and
nothing overrides it: both consumers depend on it with default features
(`crates/krkr-app/Cargo.toml:15`, `crates/krkr-vm/Cargo.toml:11`), and no
CI file passes `--no-default-features --features platform-android`
(`.github/workflows/` has only `android.yml`, which does not cargo-build
these crates). Building for `aarch64-linux-android` therefore compiles the
real `linux.rs`/winit backend. The `compile_error!` guards
(`crates/krkr-platform/src/lib.rs:35-40`) only catch *neither* and *both* —
not "the wrong platform for this target". The Android stub's `run` returning
`Unsupported` (`src/android.rs:35-42`) is unreachable unless the caller
remembers the right flags.

Fix: make the default empty (`default = []`) and have each build target/CI
job select exactly one platform feature explicitly, or add a
`cfg(target_os)`-based selection inside L1 (the only place where
`target_os` is allowed) that `compile_error!`s when the feature and the
target disagree. Wire the Android job to pass its feature. (Nit: when both
features are set, `mod linux` and `mod android` both compile and produce
`E0252` duplicate-definition errors before/next to the intended
`compile_error!`; gate each module with `#[cfg(all(feature = X, not(feature
= Y)))]` to keep the message clean.)

## 5. D9 CI guard omits `krkr-platform` and is not wired to CI — Medium

`scripts/check-stateless.sh:19-24` checks only
`krkr-api`/`krkr-fs`/`krkr-media`/`krkr-render`. The acceptance criterion
that D9 must be "Guarded in CI" (`02-target-architecture.md` §9.3) is
therefore not applied to the L1 crate the criterion names — L1 includes
`krkr-platform`. The script is also missing the `&mut self`-on-service
check that `03-middle-layer-api.md` §9 promises. No workflow invokes the
script at all. (The crate is currently clean — `rg` for
`static mut`/`thread_local!`/`LazyLock`/`OnceLock`/`OnceCell` in
`crates/krkr-platform/src` finds nothing — so this is a guard gap, not an
existing violation.)

Fix: add `"crates/krkr-platform"` to `CRATES`; add the `&mut self` pattern
or a targeted check; add a Linux CI job that runs
`scripts/check-stateless.sh` and `cargo check -p krkr-platform -p krkr-app
--all-targets`.

## 6. `resumed` is not re-delivered on re-resume — Medium

`crates/krkr-platform/src/linux.rs:117-120` returns early whenever the
window already exists, so `EventHandler::resumed` is never called a second
time. That contradicts the trait contract at
`crates/krkr-platform/src/lib.rs:111-115` ("On platforms with a
suspend/resume lifecycle this may be called again after a resume; the
handler must be idempotent"). On Android the second `resumed` is exactly
when a higher layer must recreate its surface.

Fix: call `handler.resumed(window)` on every winit `resumed` (only guard the
*window creation* on `self.window.is_some()`), and keep the idempotency
requirement; or, if re-delivery is not intended, narrow the trait doc.

## 7. DPI units, initial surface size and ignored scale changes — Medium

Three unit/lifecycle inconsistencies around the same boundary:

* `PlatformConfig.size` is documented "in pixels"
  (`crates/krkr-platform/src/lib.rs:78-79`) but is passed to
  `LogicalSize` (`crates/krkr-platform/src/linux.rs:123-126`), so on a
  HiDPI display the created window is `size * scale` physical pixels.
* `DesktopApp::resumed` creates the host at `surface_size: DEFAULT_SIZE`
  (`crates/krkr-app/src/main.rs:216`), not at the window's actual physical
  size (`PlatformWindow::size()`, `linux.rs:52-55`), so the initial
  `Surface`/swapchain can be created at the wrong size until a `Resized`
  arrives.
* `PlatformEvent::ScaleFactorChanged` is forwarded
  (`linux.rs:162-164`) but `DesktopApp::event`
  (`crates/krkr-app/src/main.rs:233-245`) ignores it (and every event other
  than `Resized`), so a DPI change never triggers a resize/recreate.

Fix: state the units explicitly in both structs; create the host from
`window.size()`; handle `ScaleFactorChanged` by resizing the surface (and
reconciling the input-space decision from finding 3) or by documenting that
the render backend is expected to self-resize.

## 8. Unvalidated `Ticker`/`PlatformConfig` input — Medium

`Ticker::poll` sets `self.next = now + self.interval`
(`crates/krkr-platform/src/clock.rs:79-84`). For a pathological
`PlatformConfig::frame_interval` this panics on `Instant` overflow, and
`frame_interval = Duration::ZERO` makes `about_to_wait` request a redraw on
every wake (busy loop). `PlatformConfig` has public fields
(`lib.rs:75-82`) with no validation and no `#[non_exhaustive]`; the same is
true of the other new public enums (`PlatformError`, `PlatformEvent`,
`InputEvent`, `MouseButton`), which will be breaking to extend.

Fix: reject/normalize a zero interval in `run` (e.g. `max(1ms)`) or in
`Ticker::new`; use `checked_add`/saturating for the deadline; add
`#[non_exhaustive]` to the public config/event/error types before the API
freezes.

## 9. Teardown order is implicit and the comment is on the wrong struct — Medium

The Vulkan surface must be destroyed while the window's display connection
is still alive. That ordering is provided only by the field declaration
order of `AppLoop` — `handler` (`crates/krkr-platform/src/linux.rs:85-88`)
is declared before `window`, so it drops first. The explanatory comment is
on `DesktopApp` (`crates/krkr-app/src/main.rs:189-195`), which owns no
window and whose field order (`backend`, `host`) is irrelevant; a future
reorder of `AppLoop` fields would silently invalidate the ordering.

Fix: move/duplicate the comment onto `AppLoop` and make it explicit, or
implement `Drop for AppLoop` that drops the handler (or just the surface)
before the window. Correct or remove the `DesktopApp` comment.

## 10. Test coverage and minor idioms — Low

* Tests cover only value equality (`crates/krkr-platform/tests/events.rs`)
  and the `Ticker`/`Clock` happy path (`tests/clock.rs`). There is no test
  for `headless_window_handle`, the `Ticker` boundaries (exact deadline,
  zero/huge interval), or the feature-selection guard; the event loop itself
  is untestable as written (winit is hard-wired in `AppLoop`). Consider
  extracting the `Ticker`-driven decision (`poll` + `request_redraw`) so the
  lifecycle can be unit-tested.
* `crates/krkr-platform/src/linux.rs:222` clones the owned `Ime::Commit`
  text (`text.clone()`); because `window_event` already owns `event`,
  `map_input` could take it by value and avoid the allocation.
* `crates/krkr-app/src/lib.rs:255` uses `frame_index.wrapping_add(1)`,
  masking an overflow that `saturating_add` (or no wrapping) would surface.
* `crates/krkr-platform/src/linux.rs:153-157` discards the handler's
  `Flow` on `CloseRequested`; a handler cannot veto/confirm close. Fine if
  intended, but the API otherwise advertises veto via `Flow`.
* `crates/krkr-platform/src/linux.rs:260-265` casts winit `KeyCode` to
  `u32`; the discriminant is not a stable key identifier across winit
  releases and can collide with `NativeKeyCode` values (`:268-278`), which
  L3 must map to TJS key codes. If L3 needs a stable id, define an explicit
  mapping enum in L1 instead of relying on the cast.
* `crates/krkr-platform/src/lib.rs:68-70`: `PlatformError::Unsupported`
  says "platform backend not compiled in" but the Android stub uses it to
  mean "host not implemented yet"; the message will mislead the CLI user.

---

## Note (out of the reviewed crates)

`#[cfg(target_os = "android")]` still appears in the legacy
`crates/engine/src/logging.rs:152,200,215` and
`crates/engine/Cargo.toml:20`. That violates "no `#[cfg(target_os)]`
outside L1", but `crates/engine` is the legacy anchor slated for deletion
per `07-roadmap.md` and is not in `default-members`; the reviewed new crates
are clean.
