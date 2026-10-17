# Android backends (AAudio + MediaCodec + AHardwareBuffer) — design & references

Status: **implemented inside the Android composition root; unverified on a
device.** **Android
is a first-class host target (D18); Windows is emulated, not a host.** We
cannot build or run Android here, so this file pins the behaviour to the
official Android documentation and defines exactly how the L1 backends map onto
our existing, already-tested API surface. Both backends are **compile-time
selected** (D13) and implemented against the NDK; each is only compiled on an
Android target (host builds use a stub), so they are compile-checked there and
must be validated on a device.

## Official references

**Audio output (AAudio / NDK)**
- AAudio guide: <https://developer.android.com/ndk/guides/audio/aaudio/aaudio>
- AAudio API reference: <https://developer.android.com/ndk/reference/group/aaudio>
- Header/lib: `<aaudio/AAudio.h>`, `libaaudio.so` (Android 8.0 / API 26+)
- Recommended by Google: **Oboe** (C++ wrapper over AAudio, OpenSL ES fallback)
  <https://github.com/google/oboe>

**Video decode (MediaCodec / NDK)**
- NDK media reference: <https://developer.android.com/ndk/reference/group/media>
- Java `MediaCodec`: <https://developer.android.com/reference/android/media/MediaCodec>
- Headers/libs: `<media/NdkMediaCodec.h>`, `<media/NdkMediaExtractor.h>`,
  `<media/NdkMediaFormat.h>`, `libmediandk.so`

**Render readback (AHardwareBuffer / dmabuf)**
- NDK `AHardwareBuffer` reference:
  <https://developer.android.com/ndk/reference/group/a-hardware-buffer>
- Header: `<android/hardware_buffer.h>` (API 26+), `libnativewindow`
- Vulkan extension `VK_ANDROID_external_memory_android_hardware_buffer`
  (device extension 130, API 27+):
  <https://docs.vulkan.org/refpages/latest/refpages/source/VK_ANDROID_external_memory_android_hardware_buffer.html>
- `VK_EXT_external_memory_dma_buf` + `VK_EXT_image_drm_format_modifier` (the
  raw-dmabuf and DRM-modifier route; Linux-oriented)
- `ndk::hardware_buffer` (safe `AHardwareBuffer` bindings, `rust-mobile/ndk`):
  <https://docs.rs/ndk/latest/ndk/hardware_buffer/index.html>

## AAudio → `platform::AudioDevice`

| Our API | AAudio |
|---|---|
| `AudioDevice::open(spec)` | `AAudio_createStreamBuilder` → set direction/sample-rate/channel-count/format/sharing-mode/performance-mode/buffer-capacity → `AAudioStreamBuilder_openStream` → `AAudioStreamBuilder_delete`. Store the `AAudioStream` in `AudioOutputState`. |
| `AudioOutputState` queue | An `AudioQueue` held by the state — the same buffer policy as the Linux backend (150 ms pre-fill gate before playback starts, 2 s cap dropping oldest-first, a ~5 ms fade to/from silence on underrun) — and the `AAudioStream_dataCallback` drains it into `audioData`. Use `AAUDIO_PERFORMANCE_MODE_LOW_LATENCY`. |
| `AudioDevice::write(state, pcm)` | Push to the queue (preferred, callback-driven) or `AAudioStream_write` for a blocking/non-blocking push. |
| `AudioDevice::stop` | `AAudioStream_requestStop` → `AAudioStream_close`. |
| `AudioDevice::is_running` | Track in the state; AAudio state transitions are async and use `AAudioStream_waitForStateChange`. |

Format handling: request `AAUDIO_FORMAT_PCM_FLOAT`; if the device returns
`AAUDIO_FORMAT_PCM_I16` convert `f32 → i16` (as cpal's `FromSample` does). Handle
`AAUDIO_STREAM_STATE_DISCONNECTED` via the error callback; always stop+close a
disconnected stream. The AAudio API is **not fully thread-safe**: do not close a
stream while another thread reads/writes it (serialise through the queue).

**Implemented** on `ndk::audio` (`AudioStreamBuilder` / `AudioStream`). Two
deliberate choices: (1) `AAudioStream_write` is **never** called — AAudio forbids
`write` on a callback-owned stream, so `write` only pushes into the queue the
data callback drains; (2) the format is read with `AudioStream::format()` (the
ABI's `AAudioStream_getFormat` — the guide's `AAudioStream_getDataFormat`
snippet is a **doc typo**; that symbol is not in `AAudio.h`). Sharing mode is
`SHARED`; performance mode `LOW_LATENCY`.

## Window and input → `platform::PlatformWindow`

| Our API | Android |
|---|---|
| `PlatformWindow::raw_window_handle` | the owned `ndk::native_window::NativeWindow` projected as `RawWindowHandle::AndroidNdk`; `ash-window` binds the Vulkan surface to it |
| `PlatformWindow::raw_display_handle` | `RawDisplayHandle::Android(AndroidDisplayHandle::new())` — Android's display handle carries no data |
| `PlatformWindow::size` | `ANativeWindow_getWidth` / `getHeight`, in physical pixels |
| `PlatformWindow::scale_factor` | `Configuration.densityDpi / 160` |
| key event | `AKeyEvent_getKeyCode` → `PlatformWindow::physical_key_from_android` → `PhysicalKey` |
| touch motion | `AMotionEvent_getAction` / `getX` / `getY` → `PlatformWindow::touch_phase_from_android` + `InputEvent::Touch` |
| mouse-style motion | `AMotionEvent_getButtonState` + `ACTION_BUTTON_PRESS`/`RELEASE` → `PlatformWindow::mouse_button_from_android` + `InputEvent::MouseButton` |

**Implemented** (compiler-checked for `aarch64-linux-android`): the
`ANativeWindow` wrapper and raw-handle projection, the physical size and the
density scale, and the pure key/touch/button translation. The translation names
no Android type, so it is host-tested; the reference `VK_*` mapping stays in the
compatibility layer (`crates/compat/src/natives/input.rs`). `AKEYCODE_BACK` maps
to `PhysicalKey::Escape`
(`reference/cpp/core/environ/cocos2d/MainScene.cpp:820-829`), not to a new key.

**The loop** lives in the Android composition root, not in `platform`:
`crates/android/src/host.rs` drives `android-activity`'s `GameActivity`, owns
the lifecycle, constructs a `PlatformWindow` from the `ANativeWindow` and feeds
the translation above. `platform`'s own Android `run` stays a typed stub
(`PlatformError::NotImplemented`) because it has no `android-activity`
dependency; the composition root does not call it.

**Device validation still needed:** that the `ANativeWindow` handed over in
`onNativeWindowCreated` is the one the renderer binds to and that recreation
after `onNativeWindowDestroyed` rebuilds the surface; that `getWidth`/`getHeight`
return the rotated landscape size; that the back and volume keys arrive as
expected; and that the touch coordinates are in the same physical space as
`size()`.

## Device-pixel readback → `render` (AHardwareBuffer interop)

This is the Android transport for the L2 pixel-translation readback
(`11-presentation-and-pixel-translation.md` §7): the single backing is a
**device-resolution** image, and a script's VM-coordinate read is served by
locking the device region `[x·s,(x+1)·s)×[y·s,(y+1)·s)` and copying it out —
the buffer is never downscaled. The scope is any device-resolution surface the
engine must read on the CPU: the frame backing now, and later a GPU-composed
layer buffer whose CPU copy has been discarded.

### The load-bearing constraint: only one direction works

`AHardwareBuffer` is the Android handle for gralloc/dmabuf memory; it is
imported into and exported from Vulkan. But the two directions are **not**
symmetric, and the extension's own issue 4 is explicit:

> Implementations are not required to support host-visible memory types for
> imported Android hardware buffers … Implementations are not required to
> support linear-tiled images backed by Android hardware buffers, even if the
> `AHardwareBuffer` has CPU usage. There is no reliable way to allocate memory
> in Vulkan that can be exported to a `AHardwareBuffer` with CPU usage.

and `vkMapMemory` is **not** `AHardwareBuffer_lock` (the former is a persistent
raw view with no ownership; the latter is temporary, exclusive, and does the
cache/reformat work). So the design does **not** allocate a Vulkan image and
try to export/map it. It does the opposite:

**allocate the `AHardwareBuffer` first, import it into Vulkan, render into it,
then `AHardwareBuffer_lock` to read.**

### Our API → Android

| our API | Android |
|---|---|
| allocate the device-resolution backing | `AHardwareBuffer_allocate` (`ndk::hardware_buffer::HardwareBuffer::allocate`) with usage `GPU_COLOR_OUTPUT \| GPU_SAMPLED_IMAGE \| CPU_READ_OFTEN`, format `R8G8B8A8_UNORM`, one layer. Validate with `is_supported` (API 29) and fail with a typed error on unsupported (D11) — never a silent downgrade |
| import into Vulkan | `vkGetAndroidHardwareBufferPropertiesANDROID` → `vkAllocateMemory` (dedicated, `VkImportAndroidHardwareBufferInfoANDROID` chained) → bind to a `VkImage` created with `VK_EXTERNAL_MEMORY_HANDLE_TYPE_ANDROID_HARDWARE_BUFFER_BIT_ANDROID` and usage `COLOR_ATTACHMENT \| SAMPLED \| TRANSFER_SRC` |
| draw the frame | render the device-space `RenderScene` into the imported image (the D40 projection); the image is the persistent backing |
| present | blit the backing image to the acquired swapchain image. Readback never comes from a swapchain image: its contents are undefined after present and the set is rotated |
| read a VM-pixel region | fence the submit that wrote the region, then `HardwareBuffer::lock_and_get_info(CPU_READ_OFTEN, fence, rect)` (API 29) for the address **and** the row stride, copy the strided rows, then `unlock` |

### Lock semantics that constrain the code

* **`lock`, not `mmap`.** `lock` may block until the GPU finishes and performs
  the cache maintenance and any tiling reformat (gralloc may hand back an
  AFBC/UBWC-tiled buffer); a raw map would read garbage. That is why the
  exported-image route above is rejected.
* **Stride is required and only the newer calls give it.** `lock` returns a
  bare address; `lock_and_get_info` (API 29) returns bytes-per-pixel and
  bytes-per-stride, and `lock_planes` returns the same per plane. A missing
  stride is a typed error, never an assumed width (D11).
* **One layer only**; a multi-layer buffer fails `lock`.
* **`rect` is the `PixelTranslation::vm_pixel_region` box**, so the lock is
  scoped to the region the script read, not the whole frame.
* Read locks are shareable across threads; a concurrent write lock is
  undefined, so the engine serializes its own writes (`unlock` before the next
  submit touching the buffer). `unlock` blocks; `unlock_async` returns a fence.

### Where the dmabuf fd fits (and where it does not)

The dmabuf is reached **through** `AHardwareBuffer`, not through
`VK_EXT_external_memory_dma_buf`. `AHardwareBuffer_getNativeHandle` (the VNDK
header `vndk/hardware_buffer.h`, whose `native_handle_t.data[0]` is the fd) is
not part of the safe `ndk` crate and is vendor-dependent, and it is only needed
to hand the memory to a **non-Vulkan** consumer. Our readback is a CPU read, so
`lock` is the transport; the fd stays an interop escape hatch. Cross-process
sharing, if ever needed, uses the sanctioned channel —
`send_handle_to_unix_socket`/`recv_handle_from_unix_socket`, both exposed by
`ndk`.

On the **Linux** host the analogue is `VK_EXT_external_memory_dma_buf` plus
`VK_EXT_image_drm_format_modifier` (a dmabuf fd negotiated with a DRM format
modifier, `DRM_FORMAT_MOD_LINEAR` for a linear view), but the simplest desktop
transport is the host-visible **staging copy** below.

### Capability and fallback

The zero-copy path needs `VK_ANDROID_external_memory_android_hardware_buffer`
(API 27+) and API 29 for the stride-returning locks. When the extension, the
format, or the required lock entry point is unavailable, the fallback is the
existing **staging-copy** readback: `vkCmdCopyImageToBuffer` the region into a
`HOST_VISIBLE | HOST_COHERENT` `VkBuffer`, wait, map, copy. It returns the same
bytes through a different transport, so this is a capability choice (like
FFmpeg vs MediaCodec, D5), **not** a silent semantic fallback — and it is
probed explicitly, not guessed.

Host builds compile a stub for all of the above, as AAudio and MediaCodec do.
The unlock/lock path returns a raw pointer, so the module uses
`#![deny(unsafe_code)]` with a scoped `#[allow(unsafe_code)]` (the same pattern
as the audio buffer conversion).

### Cost, and when the staging copy is the better transport

The AHB path keeps one persistent, lockable backing, so a script read is a
region lock instead of a full-image copy — but that backing is presented by a
blit to the swapchain every frame, and `lock` may stall on the GPU. When
readback is rare (a one-off `captureCanvas`, not a per-frame `getPixel` loop),
the staging copy is cheaper and is the default. Whether the *frame* backing must
be device-resolution and lockable at all, or only the offscreen canvases and the
GPU-composed bitmaps do, is left to Stage 3
(`11-presentation-and-pixel-translation.md` §9).

## MediaCodec → `media` video backend

| Our API | NDK |
|---|---|
| `open_video` | `AMediaExtractor_new` + `setDataSource` (fd/memory) → `getTrackCount`/`getTrackFormat` per track; pick video + audio. `AMediaCodec_createDecoderByType` → `AMediaCodec_configure` (with the track `AMediaFormat`) → `AMediaCodec_start`. |
| `present_video(at_ms)` | Feed samples: `AMediaExtractor_readSampleData` → `AMediaCodec_dequeueInputBuffer`/`getInputBuffer`/`queueInputBuffer`; drain with `AMediaCodec_dequeueOutputBuffer`/`getOutputBuffer`/`releaseOutputBuffer`. Seek via `AMediaExtractor_seekTo` + `AMediaCodec_flush`. Output is typically YUV; convert to packed RGBA8 (we already have `media/src/yuv.rs`). |
| `video_audio` | The selected audio track decoded through a second `AMediaCodec`, or the whole track decoded at open (same eager behaviour the FFmpeg backend currently has). |
| release | `AMediaCodec_stop`/`delete`, `AMediaExtractor_delete`, `AMediaFormat_delete` on `Drop`. |

`AMediaFormat` accessors (`getInteger`-style helpers are `AMediaFormat_getInt32`
etc. — see the reference) read width/height/frame-rate/duration; `AMediaCodec`
reports a **format change** on the output buffer (`AMediaCodec_getOutputFormat`)
that must be re-read.

**Implemented** with `ndk::media::media_codec` + `ndk::media::media_format` for
the codec and format work; only `AMediaExtractor_*` (not exposed by `ndk`) stays
as a small raw shim, including `memfd_create` (per-ABI syscall numbers) to feed
the in-memory `Arc<[u8]>` to `AMediaExtractor_setDataSourceFd`.

## Rust bindings: use the maintained `ndk` crate

Third-party AAudio/MediaCodec crates exist but most are stale: `aaudio`
(v0.1.1, ~6 years old), `mediacodec` (~4 years old), and `oboe`/`oboe-sys`
(11 M downloads, but wraps Google's **Oboe** *C++* library with an OpenSL ES
fallback — use only if we need pre-API-26 fallback).

**Decision:** use the maintained **`ndk` crate** (`rust-mobile/ndk`, 59 M
downloads), which ships *safe* Rust bindings for exactly what we need:
- `ndk::audio` (feature `audio`) — `AAudioStream` / `AAudioStreamBuilder`,
  data/error callbacks, `AudioFormat`, performance/sharing modes;
- `ndk::media::media_codec` + `ndk::media::media_format` (feature `media`) —
  MediaCodec + `AMediaFormat` (`AMediaExtractor` is **not** covered; use a small
  raw `extern "C"` shim for the extractor only);
- `ndk::hardware_buffer` — `HardwareBuffer`, `HardwareBufferDesc`,
  `HardwareBufferUsage`, `lock`/`lock_planes`/`unlock`. The module is gated by
  the **`api-level-26`** feature and the stride-returning
  `lock_and_get_info`/`lock_planes`/`is_supported` by **`api-level-29`**. It
  exposes **no** `AHardwareBuffer_getNativeHandle`, so the raw-dmabuf-fd path
  would need a shim; readback deliberately goes through `lock` instead.

`ndk` only compiles for an Android target, so add it **target-gated** to keep
host checks working:

```toml
[target.'cfg(target_os = "android")'.dependencies]
ndk = { version = "0.9", features = ["audio"] }   # or ["media"], or
                                                  # ["api-level-29"] for the
                                                  # hardware buffer (26 for the
                                                  # module, 29 for the locks)
```

and implement the real path under `#[cfg(target_os = "android")]` with a
`#[cfg(not(target_os = "android"))]` stub alongside. Then a host
`cargo check --features platform-android` compiles the stub, while a real Android
build (NDK target) compiles the `ndk` path.

Lint: `ndk` is safe, but two operations still need `unsafe` — the audio callback
hands the buffer over as a raw `*mut c_void` (no safe slice wrapper), and the
extractor shim is raw FFI. The `media` crate therefore uses
`#![cfg_attr(not(target_os = "android"), forbid(unsafe_code))]` +
`#![cfg_attr(target_os = "android", deny(unsafe_code))]`, with `#[allow(unsafe_code)]`
scoped to the one module each (`media`'s extractor shim; `platform`'s buffer
conversion). The `platform` crate is `#![deny(unsafe_code)]` (not `forbid`) with
a scoped allow on the Android audio module. Rustc rejects a local `allow`
under a crate-wide `forbid`, which is why the Android path uses `deny`.

## What we cannot verify here
- No device/emulator in this environment, so the backends **cannot be run**. They
  *are* compile-checked by the host `platform-android` feature and by
  `scripts/check-android-target.py` (which resolves the per-target dependency
  graph without compiling); the real Android target is compiled on a machine
  with an NDK, and everything Android-only is documented as unverified.
  Validate on a device before calling it done. Everything testable stays covered
  by the Linux/null backends.
- On-device validation still needed: AAudio open/start + callback drain without
  underruns, `PCM_I16` correctness, real disconnect reap, whether `LOW_LATENCY`
  is granted; MediaCodec demux/decode loop, `memfd`+`setDataSourceFd`, seek +
  `flush`, colour-format negotiation (`COLOR_FormatYUV420Flexible` and the
  documented planar fallback), stride/slice-height/crop geometry, format-change
  timing, PCM encoding, and EOS/drain termination.
- AHardwareBuffer readback (device-only): that `AHardwareBuffer_isSupported`
  returns true for the chosen desc on the target GPU, that the driver accepts
  the imported `VkImage`, which tiling/compression modifier gralloc picks, that
  `CPU_READ_OFTEN` is honoured, that `lock`/`lock_and_get_info` return the stride
  and the region is correct, how much `lock` stalls the GPU, and that the blit
  to the swapchain matches the old direct-to-swapchain output. Until then the
  staging-copy transport is the default and the AHB path is unverified.
