# `platform` — the host: window, event loop, input, clock, audio

Source: `crates/platform/platform/src/` · crate `platform` · layer **L1**

`platform` owns the window/surface, the event loop, the input devices, the
clock, the audio output, the system-font directories and the process metrics.
It is the only L1 crate with a `#[cfg(target_os)]`.

---

## 1. Responsibility

| concern | item |
|---|---|
| window / surface | `PlatformWindow`, the `RawWindowHandle`/`RawDisplayHandle` projection |
| event loop | `run(config, handler)`, `EventHandler`, `Flow` |
| events | `PlatformEvent::{Resized, ScaleFactorChanged, CloseRequested, Focused, Input}` |
| input devices | `InputEvent`, `MouseButton`, `PhysicalKey` |
| clock | `Clock`, `Ticker`, `FramePacing` |
| audio output | `AudioDevice`, `AudioOutputState`, `AudioQueue` |
| fonts | `system_font_dirs`, `ANDROID_SYSTEM_FONT_DIRS` |
| metrics | `memory_usage`, `process_cpu_ns`, `process_io` |

---

## 2. Layer and dependencies

| | |
|---|---|
| Layer | L1 (bottom core) |
| Depends on | `api` (L1 DTOs + error enums), `raw-window-handle`, `thiserror`, `log`; `winit` and `cpal` are Linux-only and optional; `ndk` is Android-only and target-gated |
| Must not depend on | `engine` (L2), `compat` (L3), `vm`/`tjs` (L4), a composition root, `android-activity` |
| Forbidden | process-global state, a `&mut self` method on a service, a fallback handle where an error is owed |
| Lint | `#![deny(unsafe_code)]`, `#![warn(missing_docs)]` |

The crate is `deny`, not `forbid`: the `ndk` crate wraps the AAudio C API
safely, but its data callback still hands the audio buffer over as a raw
`*mut c_void`, so one small private module converts that pointer into a
writable slice. That is the only `unsafe` in the crate; it carries a scoped
`#[allow(unsafe_code)]` and a `SAFETY:` comment on every block. Every other
backend, including the Linux one, stays entirely safe.

---

## 3. The event loop contract

```rust
pub trait EventHandler {
    fn resumed(&mut self, window: &PlatformWindow) -> Flow;
    fn event(&mut self, event: PlatformEvent) -> Flow;
    fn frame(&mut self, dt_ms: f32, window: &PlatformWindow, pacing: FramePacing) -> Flow;
    fn about_to_wait(&mut self) -> Flow { Flow::Continue }
}

pub enum Flow { Continue, Exit }
pub struct PlatformConfig { pub title: String, pub size: (u32, u32),
                            pub frame_interval: Duration }   // #[non_exhaustive]
pub enum PlatformError { EventLoop(String), Window(String), Audio(AudioError),
                         Handle(String), Unsupported, NotImplemented(String) }

pub fn run<H: EventHandler>(config: PlatformConfig, handler: H) -> Result<(), PlatformError>;
```

The platform loop owns the window and the ticker; the handler owns the engine
and returns `Flow::Exit` to stop the loop. `PlatformEvent::CloseRequested`
always stops the loop, so it cannot be vetoed.

`FramePacing` reports the loop's own measurement of the wait and the input
dispatch that preceded the frame, so the handler can attribute the frame gap
it is given as `dt_ms`:

```rust
pub struct FramePacing { pub poll_ms: f64, pub input_ms: f64 }
```

The handler may query the window's size and request a new inner size, so a
script-driven client-area change reaches the backend; the backend reports the
applied size back as `PlatformEvent::Resized`.

### The Linux backend

`linux.rs` is the only place that names winit types. It normalizes the native
event loop into `PlatformEvent`/`EventHandler` and owns the window, the frame
pacer and the `dt` computation.

Two details worth keeping:

* **Teardown order.** The winit `ApplicationHandler` declares `handler`
  **before** `window`, because Rust drops the fields in declaration order: the
  higher layer (which owns the GPU surface/swapchain) is dropped while the
  window's display connection is still alive. Reordering them would tear the
  display connection down first, which is a use-after-free risk for a Vulkan
  surface.
* **The redraw watchdog.** A due frame whose redraw request never comes back
  is reported once per stall episode, so a loop that keeps scheduling frames
  without drawing anything is visible in the log instead of showing up only as
  an absence of output.

### The Android backend

`android.rs` compiles the real `ANativeWindow` window, the AAudio output
backend and the pure input/density translation. Its **event loop is a typed
stub**: `run` returns `PlatformError::NotImplemented`, because the loop needs
the `android-activity` glue to deliver the window, the lifecycle and the input
queue, and this crate does not depend on it. The backend fails fast rather
than presenting a black surface to an Activity that no code can start. The
Android composition root owns the loop — see
[`../android/host.md`](../android/host.md).

The translation functions are associated on `PlatformWindow` and name no
Android type, so they compile and are tested on the host:

```rust
pub fn physical_key_from_android(code: i32) -> PhysicalKey;
pub fn touch_phase_from_android(action: i32) -> Option<TouchPhase>;
pub fn mouse_button_from_android(button: i32) -> Option<MouseButton>;
pub fn density_scale_from_android(density_dpi: i32) -> f64;
```

The Android pointer index is packed into the high bits of a motion action, so
the phase is read from the low byte. A code the reference names no key for
keeps its raw identity as `PhysicalKey::Native(code)` instead of being
dropped or guessed at. `AKEYCODE_BACK` becomes `PhysicalKey::Escape`, so the
Android back key reaches the engine as Escape. The reference virtual-key codes
are **not** assigned here — the compatibility layer maps a physical key onto
the reference table.

---

## 4. The window

```rust
impl PlatformWindow {
    pub fn raw_window_handle(&self) -> Result<RawWindowHandle, PlatformError>;
    pub fn raw_display_handle(&self) -> Result<RawDisplayHandle, PlatformError>;
    pub fn size(&self) -> (u32, u32);
    pub fn scale_factor(&self) -> f64;
    pub fn set_size(&self, size: (u32, u32)) -> Option<(u32, u32)>;
    pub fn request_redraw(&self);
}
```

On Android the window owns the `ANativeWindow` the activity acquired and
projects it to `RawWindowHandle::AndroidNdk`, so the renderer can create and
recreate its Vulkan surface. Its size is the surface's physical pixel size —
the same space the input coordinates use — and its scale factor is the
display's `densityDpi / 160`.

On Android `set_size` returns `None`: the platform sizes the surface from the
display, not from a client request, so there is no applied size to report.

`headless_window_handle()` returns a synthetic handle for an **explicitly
requested** headless surface creation. The null render backend accepts and
ignores any handle; a real backend with no handle must be a hard error rather
than silently binding to the fabricated canvas.

---

## 5. The clock

```rust
pub struct Clock { start: Instant }
impl Clock { pub fn new() -> Self; pub fn now_ms(&self) -> u64; pub fn elapsed(&self) -> Duration; }

pub struct Ticker { interval: Duration, next: Instant }
impl Ticker {
    pub fn new(interval: Duration, now: Instant) -> Self;
    pub fn deadline(&self) -> Instant;
    pub fn reset(&mut self, now: Instant);
    pub fn poll(&mut self, now: Instant) -> bool;
}
```

`Clock` is a read-only wall-clock view: a higher layer reads the elapsed time
through it, so no layer above owns a platform timer. `Ticker` decides when the
next frame is due, so the frame pacing lives in L1.

`Ticker::new` clamps the interval into `[1 ms, 1 h]`: a zero interval would
make the loop busy-spin, and an absurdly large one could overflow the
deadline arithmetic. `poll` advances the deadline by one interval **from
`now`**, not from the previous deadline, so a late wake-up does not cause a
burst of catch-up frames.

---

## 6. The audio output

```rust
pub struct AudioDevice { spec: AudioSpec, kind: DeviceKind }
pub struct AudioOutputState { kind: StateKind }

impl AudioDevice {
    pub fn default_spec() -> Result<AudioSpec, PlatformError>;
    pub fn open(spec: AudioSpec) -> Result<(Self, AudioOutputState), PlatformError>;
    pub fn null(spec: AudioSpec) -> Result<(Self, AudioOutputState), PlatformError>;
    pub fn spec(&self) -> AudioSpec;
    pub fn write(&self, state: &mut AudioOutputState, pcm: &[f32]) -> Result<(), PlatformError>;
    pub fn stop(&self, state: &mut AudioOutputState);
    pub fn is_running(&self, state: &AudioOutputState) -> bool;
    pub fn queued_seconds(&self, state: &AudioOutputState) -> f64;
}
```

`AudioDevice` is immutable configuration — the requested `AudioSpec` and the
backend chosen when it was opened. All evolving stream state (the OS stream,
the software queue and the lifecycle flags) lives in the caller-owned
`AudioOutputState`; L2 stores it and drives playback with
`AudioDevice::write`. The object remembers nothing, so two devices can
coexist and no global or static is involved.

`AudioDevice::null` is a mock that validates and buffers samples through the
same queue policy as the real backend, so headless hosts and tests need no
audio hardware.

### The format the device is opened at

`AudioDevice::default_spec` asks the default output device what it can do
and picks the **rate** and the **channel count** from that:

| choice | rule |
|---|---|
| rate | 48 kHz when the device offers it, then 44.1 kHz, then the highest rate the device offers |
| channels | stereo when the device offers it, then mono |
| sample format | the best format the device offers **at that rate and channel count**: `f32`, then the 64-, 32- and 16-bit integer formats, and an 8-bit format only when the device offers nothing better |

The backends' own "default" rate is not used: the ALSA backend answers
44.1 kHz whenever the device supports it, which is not the device's native
rate. Opening a 48 kHz device at 44.1 kHz resamples every track twice — once
in the mixer and once in the sound server — so the rate is taken from the
device's own supported rates instead.

The *sample format* is the device's because the mixer renders `f32`: a float
device needs no conversion at all, and an integer device of `n` bits
quantizes the signal with a step of `1 / 2^(n-1)`. An 8-bit device keeps
about 48 dB of signal-to-noise ratio, and the conversion to it truncates the
mantissa, so it is only driven when the device offers nothing better.

`AudioDevice::open(spec)` honours an explicitly requested spec: its channel
count and its rate are used as they are, and only the sample format is
chosen by the device.

### The queue policy

`audio_queue.rs` is pure and device-independent, so the policy is identical
on every platform and unit-testable without an audio device:

* **Pre-fill** — the stream is opened but **not started** until the queue
  holds ~150 ms of audio (inside the 100–250 ms window that suits a visual
  novel). Starting against an empty queue makes the first callbacks drain
  nothing, so ordinary frame jitter is immediately audible.
* **Overrun** — the queue keeps at most **two seconds**, dropping the
  *oldest* samples first. The latency cannot grow without bound.
* **Underrun** — the last emitted sample *for that channel* is repeated (a
  zero-order hold), shaped by a short fade: the shared output gain ramps
  from `1.0` to `0.0` across the held samples and back to `1.0` when real
  samples resume. A bare hold leaves a DC plateau and then steps to an
  unrelated waveform sample, which is the audible pop. The gain is a single
  scalar shared by every channel and advanced once per frame, so a stereo
  underrun cannot shift the stereo image.

### The backends

| backend | target | notes |
|---|---|---|
| cpal | `platform-linux` | the real output device |
| AAudio (via `ndk`) | `platform-android` | `PCM_FLOAT` requested; a device that answers `PCM_I16` is converted exactly as cpal's `FromSample` does |
| the stub | any other host with `platform-android` selected | every entry point returns `PlatformError::NotImplemented` — never a silent fallback |

Two deliberate choices in the AAudio path: `AAudioStream_write` is **never**
called (AAudio forbids `write` on a callback-owned stream, so `write` only
pushes into the queue the data callback drains), and the format is read with
`AudioStream::format()` — the ABI's `AAudioStream_getFormat`; the guide's
`AAudioStream_getDataFormat` snippet is a documentation typo.

The AAudio default-format probe opens a short-lived stream, reads the rate and
the channel count AAudio chose, and closes it. A device that opens but reports
more than two channels (which the mixer cannot consume) falls back to the
documented 44.1 kHz stereo spec with a logged warning.

---

## 7. Fonts and metrics

```rust
pub const ANDROID_SYSTEM_FONT_DIRS: &[&str] = &["/system/fonts", "/product/fonts", "/vendor/fonts"];
pub fn system_font_dirs() -> Vec<PathBuf>;

pub fn memory_usage() -> Option<u64>;
pub fn process_cpu_ns() -> Option<u64>;
pub fn process_io() -> Option<ProcessIo>;
```

`system_font_dirs` returns the host's system-font directories: the Android
list on Android, and the Unix system directories plus the per-user XDG
locations on a desktop. A returned directory may not exist; the caller treats
a missing entry as an absent source rather than an error. The engine (L2) may
not name a platform path, so the composition root supplies this list.

The metrics readers are **stateless free functions**: each reads the host
interface on every call and holds no cache, so the caller's refresh cadence
is the only state. `memory_usage` parses `VmRSS` from `/proc/self/status`,
`process_io` parses `read_bytes`/`write_bytes` from `/proc/self/io`. A
missing or malformed file yields `None`, so the debug overlay shows an
*unknown* value rather than a fabricated zero.

---

## 8. State ownership

| value | owner |
|---|---|
| the requested audio spec and the backend kind | L1 (`AudioDevice`) |
| the OS stream, the software queue, the lifecycle flags | L2 (`AudioOutputState`) |
| the window handle, the surface size, the density | L1 (`PlatformWindow`) |
| the frame clock and the ticker | L1 (`Clock`, `Ticker`); the **engine** owns the frame's `dt` |
| the input state, the focus, the key table | L2 (`engine::input`) |
| the previous metric sample and the refresh cadence | the overlay (L2 / the composition root) |

Nothing in this crate is process-global. The one exception is the
`android-activity`-free design: no statics, no `thread_local!`, no lazy
statics.

---

## 9. Platform gating

The platform is chosen at **compile time**; the other platform's code is not
compiled. Exactly one feature must be active and **there is no default** — the
consumer (or the CI job) selects it explicitly.

| feature | effect |
|---|---|
| `platform-linux` | the real winit 0.30 window/event-loop backend and the real cpal 0.16 audio output backend |
| `platform-android` | the real AAudio output backend (via `ndk`, Android 8.0 / API 26+) plus stubs for the window and the event loop, whose `run` returns `PlatformError::NotImplemented` |

Enabling both or neither is a `compile_error!`, as is `platform-linux` on an
Android target.

Everything that needs the NDK stays gated to `target_os = "android"`, so the
`platform-android` feature also compiles on a desktop host: the
platform-independent helpers and a typed stub build there, and the real
AAudio/window code builds only for an Android target. That is what
`uv run --project scripts krkr-gate android-target` resolves the dependency
graph for.

The rule it protects is in [`arch_rules.md`](../../rules/arch_rules.md) §12.

---

## 10. Failure modes

| failure | behaviour |
|---|---|
| the event loop cannot be created or fails while running | `PlatformError::EventLoop` |
| the window cannot be created | `PlatformError::Window` |
| the raw window/display handle cannot be obtained | `PlatformError::Handle` — a higher layer must treat this as fatal rather than substituting a synthetic handle |
| the audio device fails | `PlatformError::Audio` wrapping the typed `AudioError` |
| the backend is not compiled in | `PlatformError::Unsupported` |
| the compiled backend does not implement the operation | `PlatformError::NotImplemented` |

`AudioError` carries the audio-specific detail: `Unsupported`,
`DeviceUnavailable`, `Open`, `Stream`, `InvalidSpec`, `AlreadyRunning`,
`NotRunning`.

A device that *exists* but reports no usable format yields the documented
fallback format (with a logged warning) instead of an error, so the caller can
still try to open it — that is a *capability* choice, not a silent failure.
Every other case is an error.

---

## 11. Tests

* `crates/platform/platform/tests/audio.rs` — the audio device's open,
  write, stop and lifecycle, and the queue's pre-fill, overrun and underrun
  behaviour.
* `crates/platform/platform/tests/capabilities.rs` — the platform capabilities
  the composition root reports.
* `crates/platform/platform/tests/clock.rs` — the ticker: the deadline, the
  clamp, and the re-based deadline after a late wake-up.
* `crates/platform/platform/tests/events.rs` — the normalized platform events.
* `crates/platform/platform/src/android.rs` `mod tests` — the Android back
  key becoming Escape, the letter/digit/modifier mapping, an unmapped key
  keeping its native code, the pointer actions masked to their phase, and the
  non-contact actions having no phase.
* `crates/platform/platform/src/clock.rs` `mod tests` — the ticker poll
  behaviour and the interval bounds.
* `crates/platform/platform/src/fonts.rs` `mod tests` — the Android font
  directories and the non-empty host list.
* `crates/platform/platform/src/memory.rs` `mod tests` — the `VmRSS` parsing
  and the unknown cases (missing line, unparsable value, overflowing byte
  conversion).

---

## 12. Related documents

| topic | document |
|---|---|
| the L1 overview | [README.md](README.md) |
| L1 as an architecture | [`arch/l1.md`](../../arch/l1.md) §6 |
| the Android backends (AAudio, MediaCodec, `AHardwareBuffer`) | [`10-android-backends.md`](../../../docs.old/migration/10-android-backends.md) · snapshot [`docs.old/migration/10-android-backends.md`](../../../docs.old/migration/10-android-backends.md) |
| the Android composition root that owns the loop | [`../android/README.md`](../android/README.md) |
| the composition root that drives this crate on the desktop | [`crates/app/src/`](../../../crates/app/src/) |
| the frame contract (L2 owns the `dt`) | [`arch_rules.md`](../../rules/arch_rules.md) §4 |
| the platform selection rule | [`arch_rules.md`](../../rules/arch_rules.md) §12 |
| the Rust-level checklist | [`code_styles.md`](../../rules/code_styles.md) |
