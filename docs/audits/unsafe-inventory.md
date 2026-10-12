# `unsafe` inventory and C++ boundary audit

- Repo: `/mnt/DATA/Document/code/krkr-rs`
- Branch: `migration/middle-layer-ash`
- Date: 2026-10-11
- Scope: every `unsafe` construct in the Rust tree and the two hand-written C++
  files (`crates/tjs/cpp/tjs2_abi.cpp`, `crates/tjs/cpp/streams.cpp`) plus the
  vendored TJS2 core. **This is an audit; no source was modified.**
- House rules referenced: `AGENTS.md` §4 (no silent fallbacks), §5 (errors,
  unsafe policy), D9 (stateless L1), D11 (typed errors), D14 (static plugins),
  D32 (no L3 stubs).

Method used (all read-only): `grep -rn`, `python3`, `scripts/find_stubs.py`,
`scripts/check-stateless.sh`, plus one already-built binary
(`target/debug/krkr --headless`) for an observation. No `cargo
build/check/clippy/test` was run.

---

## 1. Executive summary

The Rust tree contains **2,039 lines mentioning `unsafe`** (compat 1,563;
tjs 190; platform 176; app 7; vm 3), comprising **29 `unsafe fn`, 4
`unsafe impl`, 9 `unsafe extern` blocks** (2 in tests) and roughly **1,980
`unsafe { … }` blocks**. These concentrate into a small number of repeating
patterns, almost all of them the native-callback FFI seam in `crates/compat`.

The design is overwhelmingly **category (a) genuinely required FFI**, with a
handful of **category (b) safely-encapsulable** helpers and a few
**category (d) genuinely unsound** sites. There are **no `#[allow(dead_code)]`
stubs** and the D9 stateless guard is clean.

The ranked findings are in §9. The four that matter before shipping:

1. **`WaveSoundBuffer.getVisBuffer` writes to an arbitrary script-supplied
   integer as a pointer** (`compat/src/natives/sound.rs`). Reference-faithful
   (`WaveImpl.cpp:3517` does the same cast) but UB by Rust rules.
2. **`VmHost` drops the C++ engine before `CompatHost`**, while
   `CompatServices` state holds `DetachedValue`s whose `Drop` calls
   `tjs2_release_value` on the engine pointer. This violates
   `DetachedValue`'s own documented "engine MUST outlive" contract and is a
   latent use-after-free at teardown.
3. **`Tjs2Engine` and `DetachedValue` are `unsafe impl Send + Sync`** although
   the TJS2 VM has process-global state and is single-threaded. Safe code can
   race two engines.
4. **`phase_vocoder::payload` is a *safe* `fn` returning `&'static mut`** from
   a raw pointer — safe callers can create aliasing `&mut` references.

Additional systemic issues: **633 of 1,539** compat `unsafe { … }` blocks have
no nearby `// SAFETY:` comment (AGENTS §5 requires one); `tjs2_assign_message`
is the one C++ entry point with no `try/catch` (a `boost` UTF-8 conversion
throw can cross the C ABI); and every function-pointer callback is an
`extern "C" fn` with **no explicit `catch_unwind`** (modern rustc aborts on
unwind out of `extern "C"`, so no UB, but a panic is a process abort).

---

## 2. `unsafe` inventory

### 2.1 Crate-level policy

| crate | attribute | notes |
|---|---|---|
| `api`, `fs`, `engine`, `testkit` | `#![forbid(unsafe_code)]` | no unsafe at all |
| `compat` | `#![deny(unsafe_code)]` + 79 file-level `#![allow]` | the FFI seam |
| `app` | `#![deny(unsafe_code)]` + scoped allows in `game.rs`, file-level in `video.rs` | two wiring functions |
| `vm` | `#![deny(unsafe_code)]` + file-level allow in `host.rs` | log callback + service attach |
| `platform` | `#![deny(unsafe_code)]` + scoped allow in `android.rs` | AAudio callback buffer |
| `platform/media` | `forbid` on non-Android, `deny` on Android + scoped allow in `mediacodec.rs` | MediaCodec/memfd FFI |
| `platform/render` | `#![deny(unsafe_code)]` + `#![deny(unsafe_op_in_unsafe_fn)]` + 7 file-level allows in `vulkan/` | ash |
| `tjs` | no crate lint; all-FFI by design | the C++ VM binding |

### 2.2 Files with an inner `#![allow(unsafe_code)]` (91)

`app` (1): `src/video.rs`.
`vm` (1): `src/host.rs`.
`render` (7): `vulkan/{device,draw,mod,pipelines,swapchain,target,texture}.rs`.
`compat` (79): `binding.rs`, `ctx.rs`, `event.rs`, `host.rs`,
`native_util.rs`, `null_plugin.rs`, `plugins_native.rs`, `resource.rs`, and
the 71 `natives/**` files listed in Appendix A.

Scoped outer `#[allow(unsafe_code)]` (5): `app/src/game.rs:86,143`;
`platform/media/src/video/mediacodec.rs:111`;
`platform/platform/src/android.rs:382`; plus the doc comment mentions in
`platform/*/src/lib.rs` (not attributes).

### 2.3 `unsafe fn` (29; 24 non-test)

| file:line | signature | category |
|---|---|---|
| `app/src/video.rs:138` | `pub unsafe fn install_video_source(host, media, storage)` | (b) |
| `compat/src/ctx.rs:80,88,115,130` | `attach_scene/storage/input/timer(&mut self, *mut T)` | (b) |
| `compat/src/ctx.rs:246` | `from_raw(engine: usize, services: usize)` | (b) |
| `compat/src/ctx.rs:258,267,277` | `engine()/services()/services_mut()` | (b) |
| `compat/src/natives/input.rs:459` | `input_state(&CompatCtx) -> Option<&InputState>` | (b) |
| `compat/src/natives/input.rs:1371` | `pad_state(*mut) -> &'static PadState` | (b)/(d) |
| `compat/src/natives/kag/mod.rs:661,670` | `state_of/instance_of(*mut) -> &'static mut` | (b) |
| `compat/.../motion_player/helpers.rs:64,77` | `inst_mut<'a,T>/inst_ref<'a,T>` | (b)/(d) |
| `compat/src/natives/rect.rs:185` | `payload(*mut) -> &'static mut RectData` | (b) |
| `compat/src/native_util.rs:27` | `args<'a>(*const Value, c_int) -> &'a [Value]` | (b) |
| `tjs/src/lib.rs:267` | `Value::octet_bytes(&self) -> Option<&[u8]>` | (b) |
| `tjs/src/lib.rs:790` | `set_log_cb(&self, Option<LogCb>, *mut c_void)` | (a) |
| `tjs/src/lib.rs:2085` | `pub unsafe fn retain_object_arg_raw` | (a) |
| `tjs/src/lib.rs:2124,2154,2163` | `take_value/take_error/take_error_string` | (a) |
| `tjs/src/lib.rs:2714` | `counter_ptr` (test helper) | test |
| tests (`tjs/src/lib.rs:2714` etc., `tjs/tests/*`) | — | test |

### 2.4 `unsafe impl` (4)

- `tjs/src/lib.rs:584,585` — `unsafe impl Send/Sync for DetachedValue`.
- `tjs/src/lib.rs:610,611` — `unsafe impl Send/Sync for Tjs2Engine`.
- `compat/src/binding.rs` deliberately avoids one by storing `usize` addresses
  under a `LazyLock<Mutex>`.

### 2.5 `unsafe extern "C"` (9)

- `tjs/src/lib.rs:285` — the main ABI declaration block.
- `tjs/src/lib.rs:515` — `tjs2_read_text_stream_all` (test-only).
- `tjs/src/lib.rs:508` — `#[link(name="z")] unsafe extern "C" {}` (empty).
- `compat/src/native_util.rs:199` — `tjs2_register_native_builtin_static_members`.
- `compat/src/natives/plugins/scripts_ex/mod.rs:63` — small declarations.
- `platform/media/src/video/mediacodec.rs:150,184` — `mediateandk` + `syscall`.
- Tests: `compat/tests/misc_plugins.rs:39`, `tjs/tests/save_struct_stream.rs:34`.

### 2.6 `unsafe { … }` patterns in `compat/src` (1,539 blocks)

| pattern | count | what it is |
|---|---|---|
| `unsafe { args(argv, argc) }` | 335 | view callback argv as a slice |
| `unsafe { &*(instance as *const T) }` | 227 | downcast the instance payload (shared) |
| `unsafe { &mut *(instance as *mut T) }` | 107 | downcast the instance payload (exclusive) |
| `unsafe { &*value }` | 120 | downcast a property `*const Value` argument |
| `unsafe { ctx.services() }` | 329 | resolve the bound L2 service set |
| `unsafe { ctx.engine() }` | 79 | resolve the bound VM |
| `unsafe { &*(payload as *const T) }` | 4 | payload from `native_instance_payload` |
| `Box::from_raw` / `Box::into_raw` | 43 / 65 | payload create/destroy |
| `binding::resolve(engine as *mut Engine)` | 20 | engine→context lookup |

**SAFETY-comment coverage:** 901 `SAFETY:` comments for 1,539 blocks. With a
4-line look-back window, **633 blocks have no nearby `// SAFETY:`** (worst:
`visual/window.rs` 64, `visual/layer/mod.rs` 56,
`visual/layer/pixels.rs` 49, `video.rs` 44, `menu_item.rs` 35). AGENTS §5
says every `unsafe` block carries a note; the compat crate does not meet it.

### 2.7 Non-compat `unsafe` sites

- **`tjs/src/lib.rs`** — the whole FFI wrapper (190 mentions). Every `tjs2_*`
  call is category (a). `Value::octet_bytes` and `args` are (b).
- **`platform/render/src/vulkan/**`** — standard `ash` usage
  (`create_*`, `cmd_*`, `destroy_*`, `CStr::from_ptr` on Vulkan-owned
  NUL-terminated names, `slice::from_raw_parts` over a local
  `PushConstants`). All category (a); no script input reaches them.
- **`platform/media/src/video/mediacodec.rs`** — `AMediaExtractor` FFI,
  `memfd_create` `syscall`, `File::from_raw_fd`. Category (a); the extractor
  owns its handle and frees once in `Drop`.
- **`platform/platform/src/android.rs`** `buffer::{fill_f32, fill_i16}` —
  `slice::from_raw_parts_mut` over the AAudio callback buffer, length derived
  from `numFrames * channelCount`. Category (a); guarded by the stream format.
- **`app/src/game.rs`, `app/src/video.rs`** — raw scene/storage pointers
  attached to the lifetime-free compat binding; `install_video_source`.
  Category (a)/(b).
- **`vm/src/host.rs`** — the log callback `CStr::from_ptr` + `attach_input`/
  `attach_timer`. Category (a)/(b).

---

## 3. Classification and concrete safe rewrites

### (a) Genuinely required FFI / Vulkan boundary — keep

All `tjs2_*` calls, all `ash`/NDK/FFI blocks, `#[link]`, `extern "C" fn`
callbacks, `Box::into_raw/from_raw` for native payloads, `memfd_create`,
AAudio buffer slices. No safe rewrite exists; the only lever is narrowing the
lifetime/ownership contracts (see (b)).

### (b) Required but encapsulable behind a safe abstraction

| site | concrete safe rewrite |
|---|---|
| `native_util::args<'a>(*const Value, c_int) -> &'a [Value]` | Return a scope-bound guard instead of an unbounded `'a`: `pub(crate) fn args<R>(argv, argc, f: impl FnOnce(&[Value]) -> R) -> R` or a `CallbackArgs<'a>` newtype built only by a wrapper that calls the callback and never lets the slice escape. Eliminates 335 `unsafe` blocks at once. |
| `Value::octet_bytes(&self) -> Option<&[u8]>` | Copy into an owned `Vec<u8>` or expose `with_octet_bytes(&self, f: impl FnOnce(&[u8]))`; the current API lets a caller store the slice past the next engine call (documented but unenforced). |
| `ctx.engine()/services()/services_mut()` (`ctx.rs:258/267/277`) | Give `CompatCtx` a real borrowing handle: make the binding own `Box<CompatServices>` + a `NonNull<Tjs2Engine>` inside a single `'static` allocation and hand out `&'static` refs obtained once at install (the binding already guarantees "one live VM"); the raw `usize` reconstruction is not needed. |
| `CompatServices::attach_scene/storage/input/timer(*mut T)` | Store `NonNull<T>` plus a lifetime brand (`PhantomData<&'a mut T>` captured at install) so the compiler rejects a handle that outlives the service. |
| `tjs::take_value/take_error*` | Take `&Value`/`&CStr` where the caller already has a reference; the pointer deref (the only `unsafe`) stays in one place. |
| `input::pad_state`, `kag::{state_of,instance_of}`, `rect::payload`, `motion_player::{inst_mut,inst_ref}` | The `'static`/caller-chosen `'a` is the hazard. Tie the returned reference to the `instance` argument's lifetime via a scoped callback (`with_inst(instance, |inst| { … })`) or return the raw `*mut T` and keep the deref at one audited call site. `inst_ref<'a,T>`/`inst_mut<'a,T>` can be used to mint two aliasing references in safe code. |
| `install_video_source` | Hand ownership of the storage reference into the source with a lifetime parameter rather than a `*const Storage`, so the `GameHost` field order becomes a compile error if wrong. |

### (c) Not needed — writable in safe Rust

- `phase_vocoder::payload` (see (d)) is the inverse: it is already *safe* but
  should be `unsafe`/scoped, not "rewritten safe".
- `take_value`, `take_error`, `take_error_string` are `unsafe fn` only to
  deref a pointer the caller already owns; callers could pass `&Value`.
- Many `unsafe { &*value }` property-arg derefs become safe once the
  trampoline gives the callback a `&Value` (the same wrapper as `args`).
- `ctx.rs` `from_raw`/`engine`/`services` are safe once the binding carries a
  real `'static` handle.

### (d) Suspicious / likely unsound — see §5–§6

`getVisBuffer`, `Send/Sync` impls, `phase_vocoder::payload`,
`VmHost` drop order, `window_draw_device_set` re-entrancy, `tjs2_assign_message`.

---

## 4. Invariant audit of the `// SAFETY:` comments

Representative claims, checked against the code:

- **"argv must point to argc valid `Value` entries"** (`native_util.rs:27`).
  Established: yes. C++ `args_to_values` (`tjs2_abi.cpp:423`) resizes a
  `std::vector<tjs2_value>` to `numparams` and passes `argv.data()` (or null
  for 0). The entries live for the callback. Sound.
- **"the C++ side guarantees a NUL-terminated UTF-8 string"**
  (`native_util.rs:46`, `tjs/src/lib.rs:2131`). Established: yes.
  `u16_to_utf8` decodes surrogate pairs and replaces unpaired surrogates with
  U+FFFD; result strings live in `e->last_string` until the next engine call
  and are copied immediately. Sound.
- **"the trampoline passes a valid out slot"** (`set_void/set_int/...`).
  Established: yes — every dispatcher declares a stack `tjs2_value out` and
  passes `&out`. Sound.
- **"instance came from create and is released once"** (`Box::from_raw`
  destroy callbacks). Established: yes — `tjs2_native_instance::release_payload`
  calls `destroy` exactly once from the destructor, and `Invalidate` is
  guarded by `invalidated`. Sound.
- **"the payload outlives the call and no other reference is live"**
  (`bitmap.rs` etc.). Established for the *single* call, but **not across
  script re-entry** — see §6.4.
- **"entries are boxed and never removed, so `cell` stays valid"**
  (`ctx.rs:194-207`). Established: via `Box::new` and no `remove`/`clear`;
  the `RefCell` still enforces borrows. Sound.
- **"the C++ side's release is idempotent"** (`DetachedValue::drop`).
  Established: yes (`tjs2_release_value` erases a missing key). Sound — but
  only while the engine is alive (§5.2).
- **"the caller resolved ctx from the installed binding"** (`ctx.services()`).
  Established only if the caller checked the binding; the safe wrappers do
  (`bound()`/`ctx()` return `Err` first). Sound at those sites.

## 5. Script-influenced raw-pointer / length / id sites

### 5.1 `WaveSoundBuffer.getVisBuffer` — arbitrary pointer write (unsound)

`compat/src/natives/sound.rs:1660-1690` (`read_vis_buffer`) and `:1712`
(`sound_get_vis_buffer`):

```rust
// sound.rs:1712
let dest = value_as_i64(&raw_args[0]) as usize as *mut i16;
// sound.rs:1685-1688
*dest.add(index) = downmix_to_i16(...);      // downmix
*dest.add(index * 2) = float_to_i16(left);   // interleaved
```

The destination is a **raw integer supplied by the script**. `numsamples`
and `channels` are also script integers; the loop writes `count * channels`
`i16`s to that address. Breaking inputs (all valid TJS):

```javascript
var w = new WaveSoundBuffer(window);
w.open("x.wav"); w.play();
w.getVisBuffer(0, 1, 1);          // null pointer, non-null deref/UB
w.getVisBuffer(0xdeadbeef, 1000, 2); // arbitrary wild address, 4000 bytes
w.getVisBuffer(-1, 0x7fffffff, 2);   // pointer from a negative int + huge span
```

The reference is equally unsafe (`reference/cpp/core/sound/win32/WaveImpl.cpp:3517`:
`tjs_int16 *dest = (tjs_int16 *)(tjs_int64)(*param[0]);`), so this is
**reference-faithful behaviour**, but it is UB in Rust. Recommended fix that
preserves reference semantics for the common case: keep a registry of
buffers this process handed to scripts (e.g. the `getSample`/plugin scratch
and any future allocator), and raise a typed TJS error for an address not in
it; or gate the write behind the same address the script obtained from a
port-provided allocator. If raw addresses must stay supported, wrap the loop
in an explicit, documented `unsafe` "script-provided pointer" primitive so it
is auditable in one place (the current `// SAFETY:` says only "the reference
trusts it as well", which is not a safety argument).

### 5.2 `DetachedValue` outliving the engine (teardown UAF)

`tjs/src/lib.rs:583-598` — `DetachedValue::drop` calls
`tjs2_release_value(self.engine, self.id)`; the doc says "The engine MUST
outlive every `DetachedValue`".

`vm/src/host.rs:32-38` declares `VmHost { engine, input, timer, compat,
adapter }`; `impl Drop for VmHost` (`:168`) only removes the binding. Rust
drops fields in declaration order, so **`engine` (a `Box<Tjs2Engine>` whose
`Drop` calls `tjs2_destroy`, which `delete`s the C++ `tjs2_engine`) is dropped
before `compat`** (a `CompatHost` whose `Box<CompatServices>` owns the state
map). Many state structs hold live `DetachedValue`s that are not cleared by
`CompatHost::uninstall`:

- `natives/visual/common.rs ObjectRegistry` (windows/layers/bitmaps/fonts),
- `natives/visual/window.rs WindowInst`, `natives/visual/layer/mod.rs LayerInst`,
- `natives/menu_item.rs MenuState.{text_to_keycode,keycode_to_text}` (static
  caches, never cleared),
- `natives/plugins/window_ex/window.rs WindowFlags.{sys_menu,overlay}`,
- `natives/plugins/motion_player/mod.rs` `use_d3d_probe`, `MotionStore`,
- `natives/plugins/xp3_filter/mod.rs Xp3FilterState`,
- `natives/kag/mod.rs KagParserInstance`, `natives/sound.rs SoundObject`,
  `natives/debug.rs DebugState`, `natives/system.rs ContinuousHandler`, …

If any survives `tjs2_destroy`, its `Drop` dereferences a freed
`tjs2_engine*`. A 2-frame headless run against
`/mnt/DATA/Games/Others/test` exited cleanly (the `ObjectRegistry` entries
are released by native-destroy callbacks during `Shutdown`), but the
MenuItem/windowEx/motion/xp3 caches are never explicitly drained, so this is
reachable for games that touch them. Fix: in `VmHost::drop` (or a
`CompatHost::shutdown()` called there) drop the `CompatServices` state map
**before** `Tjs2Engine` drops, or store those cached handles as raw ids (as
`CompatServices::noop_closure` already does precisely to avoid this).

### 5.3 `Tjs2Engine`/`DetachedValue` `Send + Sync` (unsound API)

`tjs/src/lib.rs:584,585,610,611`. The comments say "sound only because
krkr-rs confines all VM use to a single thread". That is not a property the
public `tjs` API enforces; the types are `pub`. Breaking safe code:

```rust
let a = Tjs2Engine::new().unwrap();
let b = Tjs2Engine::new().unwrap();
std::thread::spawn(move || { let _ = a.eval("1", "t"); });
std::thread::spawn(move || { let _ = b.eval("1", "t"); }); // races C++ globals
```

Two engines on two threads corrupt the process-wide TJS2 script cache/string
pool. Fix: drop the `Sync` impls (keep `Send` only if the VM moves between
threads but is never shared, which `&Tjs2Engine` methods forbid) or wrap the
engine in a `!Send + !Sync` marker and require the host to own it on one
thread. `DetachedValue` should inherit the marker from the engine.

### 5.4 `phase_vocoder::payload` — safe `fn` returning `&'static mut`

`compat/src/natives/sound/phase_vocoder.rs:103-106`:

```rust
fn payload(instance: *mut c_void) -> &'static mut PhaseVocoderInst {
    unsafe { &mut *(instance as *mut PhaseVocoderInst) }   // SAFETY internal only
}
```

Because the function is **safe**, safe callers can write
`let a = payload(i); let b = payload(i);` and obtain two aliasing `&mut` to
the same payload — immediate UB. Same class (but `unsafe fn`, so at least
caller-acknowledged): `motion_player/helpers.rs:64,77` (`inst_mut<'a,T>`
returns an unbounded caller-chosen `'a`), `kag/mod.rs:661,670`,
`rect.rs:185`, `input.rs:1371`. Fix: make `payload` `unsafe fn` (or
`pub(crate) unsafe fn`) and/or return `*mut PhaseVocoderInst`, keeping a
single deref at the callback.

### 5.5 `window_draw_device_set` — `&mut` held across script re-entry

`compat/src/natives/visual/window.rs:1602-1647`:

```rust
let inst = unsafe { &mut *(instance as *mut WindowInst) };
if let Some(previous) = inst.draw_device.replace(object)
    && let Err(error) = host.invalidate_retained(previous.raw_id()) { … }
```

`invalidate_retained` runs the previous object's script `finalize`
(`tjs2_invalidate_retained` → `clo.Invalidate`). A `finalize` that reads or
writes `window.drawDevice` re-enters `window_draw_device_set` and creates a
second `&mut WindowInst` while the first is live. The `&mut` borrow is also
held across the `host.invalidate_retained` call. Fix: clone/take the previous
handle and `drop` the borrow before invalidating:

```rust
let previous = unsafe { &mut *(instance as *mut WindowInst) }.draw_device.take();
if let Some(previous) = previous { host.invalidate_retained(previous.raw_id()) … }
```

(The other re-entrancy candidates spotted by a body scan —
`menu_item_fire_click`, `timer_on_timer`, `bitmap_load_async`,
`text_render_get_characters` — scope or end the payload borrow before calling
back into the VM and were judged sound.)

### 5.6 Other script-influenced sites checked and found sound

- `plugins/get_sample/mod.rs::call_get_vis_buffer` passes a Rust `Vec` pointer
  to the object's own `getVisBuffer`; the `Vec` outlives the call, so the
  native write is bounded.
- `scripts_ex/mod.rs::set_octet_out` (and `layer_ex`/`plugins_native` string
  arrays) keep the scratch buffer in `CompatServices` state and set
  `string`/`array`/`array_count` together; C++ copies during the callback.
- `wutcwf` IMA `stepindex` is range-checked at block parse
  (`ima_stepindex > 88` → typed `InvalidStepIndex`) and clamped at update, so
  `IMA_STEP_SIZE[stepindex]` cannot panic.
- `psd_file::layer_at` validates `0 <= no < len`; `color_table.transparency_index`
  is range-checked; `debug.rs` weekday/month indices are `rem_euclid`/1-based.
- `text.rs::decode_container_zlib` checks `bytes.len() >= 21` before
  `bytes[5..13]`/`bytes[13..21]`.
- `kag/state.rs` `expect`/`unreachable!` (`:1284,:1360,:1404,:1407`) are
  guarded by preceding `is_empty`/`Option`/`label_idx`-set checks.

---

## 6. FFI panic audit (`extern "C"` callbacks)

There is **no `catch_unwind` / `AssertUnwindSafe` anywhere** in the workspace
(verified by grep). Every native method/property is an `extern "C" fn`
(1,041 of them in `compat/src`) called from the C++ trampolines
(`tjs2_dispatch_native_method`, `…_property_get/set`, `…_instance_method`,
`…_objthis_method`, `…_constructor`). Each C++ body wraps the Rust call in
`try { … } TJS_CONVERT_TO_TJS_EXCEPTION`, which converts **C++** exceptions to
TJS errors — it does **not** catch Rust panics.

Consequence: a Rust panic unwinds out of an `extern "C" fn`. On rustc 1.97
(and any target since the `C-unwind` split) this is **abort-guarded by the
compiler**: the function cannot unwind, so the process aborts. It is *not* UB,
but it is a process abort (DoS). If any callback were ever changed to
`extern "C-unwind"`, it would become UB; none is.

Panic-capable operations in non-test compat production code are minimal:

| site | guarded? |
|---|---|
| `ctx.rs:207` `.expect("state is keyed by TypeId")` | internal invariant; `T` always matches the key |
| `text.rs:116-117` `.expect("8 header bytes")` | guarded by the length check above |
| `kag/state.rs:1284,1404,1407` `.expect("checked …")` | guarded by `is_empty`/`is_some_and` |
| `kag/state.rs:1360` `unreachable!` | `label_idx` only set from an `Event::Label` |

All `.unwrap()` occurrences in `compat/src` are inside `#[cfg(test)]`
modules. There are no reachable `todo!`/`unimplemented!` (D32 clean). The risk
is therefore **future**: any new `.unwrap()`/slice index/`RefCell` double
borrow in a callback aborts the process. Recommendation: add a single
`catch_unwind` shim at the Rust trampoline boundary (or a
`#[panic_handler]`-style guard per callback family) so a script can trigger a
TJS error instead of killing the host, and so the panic never reaches the C++
`try` (which cannot see it).

`tjs/src/lib.rs` non-test panics: only `set_data_dir`'s
`CString::new(dir).unwrap_or_default()` — a silent fallback (D11): a path
containing NUL silently becomes `""`. `Tjs2Engine::new` handles null.

---

## 7. C++ boundary invariants

### 7.1 What Rust relies on, and whether C++ establishes it

| invariant Rust assumes | established by C++? |
|---|---|
| `argv` = `argc` valid `tjs2_value` for the call | yes: `args_to_values` (`tjs2_abi.cpp:423-435`), null when empty |
| result strings/octets are NUL-terminated UTF-8 / valid bytes, valid until the next engine call | yes: `variant_to_value`/`variant_to_value_one` store in `e->last_string`; `u16_to_utf8` emits valid UTF-8 (`:122-158`) |
| `instance` payload valid for the call, freed exactly once | yes: `tjs2_native_instance` (`:1048-1118`): `GetNativePtr()` null after `Invalidate`, `release_payload()` once from the destructor |
| retained ids are non-zero, live until released, release idempotent | yes: `retain_variant` skips 0; `tjs2_release_value` erases a missing key |
| `tjs2_get_native_instance` returns null on any mismatch and never throws across the ABI | yes: `:1929-1953` `try/catch(...) return nullptr` |
| `tjs2_free_string`/`tjs2_malloc` are malloc/free-compatible | yes: `:2143-2148` |
| length fields agree with buffers | yes for both directions: `tvtOctet` uses `GetLength()`; Rust `set_octet_out` sets `array_count = bytes.len()`; C++ `value_to_variant` VAL_ARRAY checks `array && array_count > 0` |
| no freed `tTJSVariant` is read after a map mutation | yes: `call_value`/`call_member`/`prop_set`/`prop_set_default` convert arguments **before** looking up the id and re-resolve after (`:2763-3108`); the comments at `:2795-2803` call this out explicitly |
| `objthis` is a live dispatch object during the call | yes: passed straight from the VM's `FuncCall` frame |

### 7.2 C++ problems Rust depends on

1. **`tjs2_assign_message` has no `try/catch`** (`tjs2_abi.cpp:1767-1773`)
   and calls `boost::locale::conv::utf_to_utf` (`utf8_to_u16`), which throws
   `conversion_error` on malformed UTF-8; `TJSAssignMessage` can also throw.
   A C++ exception crossing the `extern "C"` ABI is UB. Rust only ever passes
   valid UTF-8 (`CString::new` on a `&str`), so it is a latent bug, not a
   reachable one, but it is the only entry point without a guard. `tjs2_set_data_dir`
   (`streams.cpp:42-44`) similarly assigns a `std::string` unguarded.
2. **`utf8_to_u16` default method is `stop`** (throw), used in every
   Rust→C++ string conversion. All call sites except the two above are inside
   `try/catch`, so an invalid string becomes a TJS error. Rust must keep
   passing valid UTF-8 (it does).
3. **`array_count` narrowing**: `variant_to_value_one` writes
   `(int)oct->GetLength()` (`:404`) and `value_to_variant` narrows
   `in->array_count`; an octet >2 GiB truncates. Implausible from a game but
   technically unchecked. Use `tjs_uint` consistently.
4. **`tjs2_compile_script` deletes its output stream** (`delete output`) — the
   stream is a `tTJSBinaryStream`; this matches the reference ownership.
5. **`streams.cpp`** establishes the path/encoding invariants Rust relies on:
   `utf8_to_utf16`/`utf16_to_utf8` handle invalid/lone surrogates, the
   `FE FE` container checks `compressed > raw.size()-payload`,
   `uncompressed % 2`, and zlib return codes, and every factory returns a
   heap object whose `Destruct()` deletes it once. No Rust-visible length
   disagreement found.

---

## 8. `scripts/find_stubs.py` and `scripts/check-stateless.sh`

- `find_stubs.py` reports only long functions (no `todo!`/`unimplemented!`);
  the D32 stub ban holds — the only `todo!`-class tokens are in vendored
  `tjs2/`.
- `check-stateless.sh` reports **`D9 guard: clean`** (no globals/caches or
  stateful services in L1). The one deliberate process-global is the L3
  `binding.rs` `LazyLock<Mutex<Option<Binding>>>`, which is allowed.

---

## 9. Ranked findings

### Needs a fix before shipping

1. **`getVisBuffer` arbitrary pointer write** —
   `compat/src/natives/sound.rs:1660-1690,1712`. Script controls `dest`,
   `numsamples`, `channels`; writes `count*channels` `i16`s to any address.
   Reference-faithful but Rust-UB. Fix: validate `dest` against a known-buffer
   registry or funnel it through one audited primitive.
2. **Teardown use-after-free from `VmHost` drop order** —
   `vm/src/host.rs:32-38,168` + `tjs/src/lib.rs:592-598` +
   `compat/src/host.rs:16-22,148` + the state structs in §5.2. Engine `tjs2_engine*`
   is freed before `CompatServices` state's `DetachedValue`s are dropped.
   Fix: drop/clear the services state before `Tjs2Engine`.
3. **`Send + Sync` on `Tjs2Engine`/`DetachedValue`** —
   `tjs/src/lib.rs:584,585,610,611`. Safe code can run two VMs concurrently
   and corrupt process-global VM state. Fix: remove `Sync` (and probably
   `Send`) or add a `!Send + !Sync` token owned by the host.

### Should fix (soundness hygiene)

4. **`phase_vocoder::payload` is a safe `fn` yielding `&'static mut`** —
   `compat/src/natives/sound/phase_vocoder.rs:103`. Make it `unsafe fn`/return
   a raw pointer. Review the same pattern in
   `motion_player/helpers.rs:64,77`, `kag/mod.rs:661,670`, `rect.rs:185`,
   `input.rs:1371`.
5. **`window_draw_device_set` holds `&mut WindowInst` across
   `invalidate_retained`** — `compat/src/natives/visual/window.rs:1636-1645`.
   Drop the borrow before the script `finalize`.
6. **`tjs2_assign_message` / `tjs2_set_data_dir` lack `try/catch`** —
   `tjs2_abi.cpp:1767`, `streams.cpp:42`. A `boost`/`bad_alloc` throw crosses
   the C ABI.
7. **No panic guard at the FFI boundary** — 1,041 `extern "C"` callbacks, no
   `catch_unwind`. Currently abort (not UB), but a script-triggered abort is a
   DoS. Add one shim per callback family.
8. **633 compat `unsafe` blocks without a `// SAFETY:` note** (AGENTS §5).
   Worst offenders: `visual/window.rs` (64), `visual/layer/mod.rs` (56),
   `visual/layer/pixels.rs` (49), `video.rs` (44).

### Acceptable with documentation / narrowing

9. `args()`'s unbounded `'a` and `octet_bytes()`'s caller-owned slice — safe
   today, but brittle; encapsulate (§3b).
10. `CompatCtx::engine/services/services_mut` and `attach_*` — a real
    `'static` handle would make them safe.
11. `set_data_dir` `unwrap_or_default` silent NUL fallback (D11) — should be a
    typed error.
12. `octet` length narrowing to `c_int` in the C++ ABI (>2 GiB) — use
    `tjs_uint`.

### Sound as-is

13. All `ash`/Vulkan, NDK/MediaCodec, AAudio, `memfd_create`, and `tjs` FFI
    call sites (category (a)).
14. The `binding.rs` single process-wide table (addresses stored as `usize`,
    no `unsafe impl`).
15. The 335 `args()` uses, 227/107 payload downcasts, 43 `Box::from_raw`
    destroy callbacks, and the `binding::resolve` lookups — all uphold their
    stated contracts on the current call paths (modulo the re-entrancy cases
    in §5.5 and the drop-order bug in §5.2).

---

## Appendix A — files with `#![allow(unsafe_code)]` (91)

```
crates/app/src/video.rs:29
crates/vm/src/host.rs:3
crates/platform/render/src/vulkan/{device.rs:9, draw.rs:12, mod.rs:39,
    pipelines.rs:9, swapchain.rs:7, target.rs:12, texture.rs:7}
crates/compat/src/{binding.rs:15, ctx.rs:2, event.rs:10, host.rs:3,
    native_util.rs:6, null_plugin.rs:43, plugins_native.rs:8, resource.rs:11}
crates/compat/src/natives/{async_trigger.rs:49, cdda_midi.rs:18,
    clipboard.rs:34, constants.rs:14, debug.rs:40, image_function.rs:42,
    input.rs:54, kag/mod.rs:31, menu_item.rs:48, rect.rs:23, scripts.rs:29,
    sound.rs:130, sound/phase_vocoder.rs:27, sound/wave_flags.rs:25,
    storages.rs:37, system.rs:56, tree_owner.rs:51, video.rs:42,
    visual/bitmap.rs:2, visual/common.rs:2, visual/draw_device.rs:25,
    visual/font.rs:16, visual/layer/focus.rs:13, visual/layer/mod.rs:2,
    visual/layer/order.rs:12, visual/layer/pixels.rs:13,
    visual/layer/transitions.rs:17, visual/layer/util.rs:6, visual/mod.rs:79,
    visual/timer.rs:17, visual/window.rs:2,
    plugins/add_font/mod.rs:29, plugins/csv_parser/mod.rs:28,
    plugins/dirlist/mod.rs:39, plugins/fft_graph/mod.rs:27,
    plugins/fstat/mod.rs:52, plugins/get_sample/mod.rs:34,
    plugins/json/mod.rs:56, plugins/layer_ex/appearance.rs:10,
    plugins/layer_ex/draw.rs:13, plugins/layer_ex/font.rs:10,
    plugins/layer_ex/gdiplus.rs:13, plugins/layer_ex/geom.rs:10,
    plugins/layer_ex/image.rs:9, plugins/layer_ex/matrix.rs:10,
    plugins/layer_ex/mod.rs:26, plugins/layer_ex/movie.rs:21,
    plugins/layer_ex/path.rs:16, plugins/layer_ex/perspective.rs:22,
    plugins/motion_player/adaptors.rs:9, plugins/motion_player/emote.rs:9,
    plugins/motion_player/helpers.rs:8, plugins/motion_player/mod.rs:30,
    plugins/motion_player/player.rs:14, plugins/motion_player/resource.rs:9,
    plugins/psb_file/mod.rs:61, plugins/psd_file/mod.rs:25,
    plugins/save_struct/mod.rs:56, plugins/scripts_ex/mod.rs:32,
    plugins/steam/mod.rs:32, plugins/text_render/metrics.rs:12,
    plugins/text_render/mod.rs:26, plugins/win32_dialog/mod.rs:42,
    plugins/window_ex/console.rs:16, plugins/window_ex/menu_item.rs:13,
    plugins/window_ex/scripts.rs:12, plugins/window_ex/system.rs:20,
    plugins/window_ex/util.rs:9, plugins/window_ex/window.rs:17,
    plugins/wutcwf/mod.rs:38, plugins/xp3_filter/mod.rs:67}
```

Category mapping for these files: (A) raw FFI / (B) `extern "C"` callback,
(C) binding/ctx services, (D) payload downcast. All 79 compat files need at
least (B)+(D); the ~8 pure-registration files (`constants.rs`,
`plugins/dirlist/mod.rs`, …) only need (C) and could narrow the allow to the
`ctx.services()` call sites.

## Appendix B — methodology commands (read-only)

```bash
grep -rn 'allow(unsafe_code)' crates --include=*.rs
grep -rn '\bunsafe\b' crates --include=*.rs
grep -rn 'unsafe fn\|unsafe impl\|unsafe extern' crates --include=*.rs
grep -rn 'from_raw_parts\|copy_nonoverlapping\|Box::from_raw' crates --include=*.rs
grep -rn 'catch_unwind' crates --include=*.rs              # none
python3 scripts/find_stubs.py
bash scripts/check-stateless.sh                            # D9 guard: clean
./target/debug/krkr --headless --frames 2 --backend null /mnt/DATA/Games/Others/test
```

Reference citations used: `reference/cpp/core/sound/win32/WaveImpl.cpp:3517`,
`:3401`, `:3378`; the port C++ is `crates/tjs/cpp/tjs2_abi.cpp` and
`crates/tjs/cpp/streams.cpp`.
