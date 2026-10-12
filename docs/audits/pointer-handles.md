# Pointer-as-handle audit — address-derived values that cross the script or state boundary

- Repo: `/mnt/DATA/Document/code/krkr-rs`
- Branch: `main`
- Date: 2026-10-13
- Scope: the whole workspace (`crates/**`), hunting one class: a pointer or
  address-derived value cast to a **signed or narrower** type; a
  **sign/positivity test** on such a value; a pointer stored in a
  **script-visible** value and read back as a handle (the hidden-`__handle`
  pattern); address-space assumptions (top bit clear, fits in 32 bits,
  numeric pointer comparison, `as usize`-keyed state where identity matters).
  The escaped device defect in `crates/compat/src/natives/plugins/layer_ex/**`
  is one instance of the class; a peer owns its code fix, so this audit
  reports it and does not edit it.
- **This is an audit; the only files written are this one, the decision entry
  in `docs/migration/README.md`, and the guard
  (`scripts/check-error-handling.sh` + `scripts/error_handling_allow.txt`).**
  No `cargo`/`rustc`/`clippy` command was run (AGENTS §0). Method: `rg`
  sweeps for pointer casts, signed casts, sign tests, `NonZero`/`try_from`,
  `HashMap<usize`, `as_ptr`/`as_mut_ptr`, and the `__handle` literal, then a
  hand-read of every hit against `reference/cpp/**`.

House rules referenced: AGENTS §4 (no silent fallback), §5 (house style),
§7 (genericity), D11, D28, D30, D31, D34, and the new **D37**.

---

## 1. Executive summary

The escaped bug is the **only** instance that makes a valid object fail on
aarch64, but it is not the only place where an address is used as a handle.
The class splits cleanly into three groups:

| rank | group | sites | script-observable | aarch64 failure |
|---|---|---|---|---|
| 1 | Hidden `__handle` payload pointer + `> 0` lookup (`layerExDraw`) | 6 | yes (throws) | **yes** |
| 2 | Reference-faithful "pointer as integer" getters (`HWND`, `buffer`, `nativeArray`, `interface`, `mainImageBuffer`) | 11 | yes (opaque token) | no, as written |
| 3 | Internal state keyed by a raw object/buffer address | 8 | no | no |

Group 1 is the device defect: the writer stores the payload pointer as a
signed `i64` and the reader accepts it only when `> 0`. On x86-64 the heap is
at `0x55f3eb11…` (top bit clear → positive → works); on aarch64 Android it is
at `0xb400007a…` (top bit set → negative → the lookup fails). The game ran
on Linux and black-screened on the phone; the message named a type that was
never involved. Group 2 must expose a pointer as an integer because the
reference does (D28/D34) — the rule D37 adds is that no code may *interpret*
that value with a sign test or a narrowing, and group 1's `__handle`
mechanism must go.

The mechanical detector is the new `pointer-sign` rule in
`scripts/check-error-handling.sh`; it currently flags 21 sites, all reviewed
in the allowlist, with the six `layerEx` rows marked as removed by the
in-flight fix (they go **stale** automatically once it lands).

---

## 2. The escaped defect (rank 1) — script-observable, aarch64 failure

**Sites.**

- `crates/compat/src/natives/plugins/layer_ex/appearance.rs:330-339` —
  `Appearance.__handle` getter: `set_int(out, instance as usize as i64)`.
  Registered as the hidden property `__handle` at `:511`.
- `crates/compat/src/natives/plugins/layer_ex/path.rs:300-309` — the same
  getter for `Path`; registered at `:838`.
- `crates/compat/src/natives/plugins/layer_ex/mod.rs:498-516` —
  `read_handle`, which reads `"__handle"` and accepts only
  `TjsValue::Integer(handle) if handle > 0` (and `Real` with `> 0.0`), else
  raises ``"{what}: the argument is not a LayerExDraw value type"``.

**What it does.** The plugin has no per-object native payload reachable from a
sibling member, so it publishes the *payload address* as a hidden script
property and recovers it by reading that property back. The address is cast
to a **signed** `i64`; the reader applies a **positivity** test before
casting to `usize`.

**How a script reaches it.** Any `Layer.drawPolygon(app, …)` /
`Layer.drawPath(path, …)` call whose first argument is an `Appearance` or
`Path` object — e.g. `system_MessageFrame.tjs`:
`app = new GdiPlus.Appearance(); drawPolygon(app, [[…]])`.

**Evidence (device, aarch64).**

```
krkr-rs-logs.txt:13   … (object 0xb400007a797ead30:0xb400007a797ead30) …
krkr-rs-logs (2).txt:769  … Layer.drawPolygon: the argument is not a
                          LayerExDraw value type
```

`0xb400007a797ead30` has the top bit set, so `as i64` is negative and
`handle > 0` is false. On x86-64 the payload is at `0x55f3eb11…` (top bit
clear), `> 0` is true, and the lookup succeeds — which is why the bug
survived every host test. The error string is also wrong (it names
`LayerExDraw` for an `Appearance` argument); that is F2 in
`docs/audits/layerex-patterns.md`.

**Proposed fix.** Resolve the argument through the ABI's native-instance
mechanism — `Tjs2Engine::native_instance_payload(object, "Appearance" |
"Path")`, the reference `TJS_GET_NATIVE_INSTANCE` behind
`NativeInstanceSupport(TJS_NIS_GETINSTANCE, classid)` — exactly as
`matrix.rs:154-163` and `image.rs:156-164` already do. Delete the
`__handle` getters and the property registrations. This also removes a
port-only script member (F9 in `docs/audits/layerex-patterns.md`) and the
wrong error string.

The fix is in flight on a peer's branch; the guard allowlist marks the six
sites as pending removal (§7).

---

## 3. Reference-faithful "pointer as integer" getters (rank 2) — script-observable, no failure as written

These hand the script a native pointer cast to a `tjs_int64`, matching the
reference. They do **not** fail on aarch64 as written because nothing in the
tree reads them back through a sign test or a narrowing. D37 makes that
explicit: the value is an **opaque token**, and a future `> 0`, `.max(0)`,
`as u32`, or `NonZero*` on one of these is a violation.

| site | member | registration | reference |
|---|---|---|---|
| `crates/compat/src/natives/visual/window.rs:1700` | `Window.HWND` = `instance as i64` | `:2317` | `WindowImpl.cpp:2288-2298` `(tTVInteger)(tjs_intptr_t)_this` |
| `crates/compat/src/natives/visual/bitmap.rs:1187` | `Bitmap.buffer` = `rgba.as_ptr() as i64` | `:1353` | reference pixel-buffer address |
| `crates/compat/src/natives/visual/bitmap.rs:1219` | `Bitmap.bufferForWrite` = `rgba.as_mut_ptr() as i64` | `:1358` | reference pixel-buffer address |
| `crates/compat/src/natives/visual/layer/mod.rs:2227` | `Layer.mainImageBuffer` = `scene.main_image_pixel_buffer(id) as i64` | `:2957` | `LayerIntf.cpp:3005` |
| `crates/compat/src/natives/visual/layer/mod.rs:2256` | `Layer.mainImageBufferForWrite` | `:2962` | `LayerIntf.cpp:3005` |
| `crates/compat/src/natives/visual/layer/mod.rs:2312` | `Layer.provinceImageBuffer` | `:2972` | `LayerIntf.cpp:3027` |
| `crates/compat/src/natives/visual/layer/mod.rs:2341` | `Layer.provinceImageBufferForWrite` | `:2977` | `LayerIntf.cpp:3034` |
| `crates/compat/src/natives/rect.rs:702` | `Rect.nativeArray` = `instance as usize as i64` | `:741` | `RectItf.cpp:336-346` `(tTVInteger)(tjs_intptr_t)_this->Get().array` |
| `crates/compat/src/natives/visual/draw_device.rs:218` | `DrawDevice.interface` = `instance as i64` | `:370` | `BasicDrawDevice.cpp:886-896`, `PassThroughDrawDevice.cpp:532-544` |
| `crates/compat/src/natives/plugins/steam/mod.rs:509` | `DrawDeviceForSteam.interface` | `:661` | `DrawDeviceForSteam.cpp:21-24` |
| `crates/compat/src/natives/sound/phase_vocoder.rs:137` | `PhaseVocoder.interface` = `instance as usize as i64` | `:297` | `PhaseVocoderFilter.cpp:43-53`; `WaveIntf.cpp:937-946` resolves it via `PropGet` |

The Layer/Bitmap addresses originate in L2 as `usize`
(`crates/engine/src/scene.rs:876,890,898,917`); the signed cast happens at
the L3 boundary, which is why the L2 type is not the problem.

**`PhaseVocoder.interface` is the one with an in-tree consumer.**
`crates/compat/src/natives/sound.rs:772-773`:

```rust
if let Ok(TjsValue::Integer(handle)) = host.member(element.raw_id(), "interface")
    && let Some(config) = vocoders.get(&(handle as usize))
```

`instance as usize as i64` followed by `handle as usize` is a **bit-exact**
round trip on every platform (the `i64→usize` cast sign-extends back to the
original bits), so this does not fail on aarch64. It is still address-keyed
state and an address-derived script token: if an object is freed and a new
one is allocated at the same address, a stale `filters` entry could resolve.
`vocoders` is cleaned in `pv_destroy` (`phase_vocoder.rs:93-104`), so the
window is narrow. **Proposed fix:** give `PhaseVocoderInst` a stable opaque
id (the way `MenuItemInst` carries `id: Option<u64>`) and key `vocoders` by
that id; return the id from `interface`.

**Device-failure verdict.** None of these fails as written. A script *can*
observe the sign (the reference does the same cast, so a negative value on
aarch64 is reference-faithful), but we must not add a sign-dependent path.

---

## 4. Internal state keyed by an address (rank 3) — not script-visible

These key process state by a raw object/buffer address. They work on both
platforms because they only rely on *pointer identity*, never on a sign, and
the value matters only for the object's lifetime. They are listed because
address reuse is the latent version of the same "address is not identity"
mistake.

| site | key | cleanup / guard |
|---|---|---|
| `crates/compat/src/natives/menu_item.rs:133,239,603` | `instances: HashMap<usize, u64>` keyed by `Value::object_handle()` (the ABI raw object pointer) | removed in `menu_item_destroy` (`:406`) |
| `crates/compat/src/natives/plugins/get_sample/mod.rs:77,127-138` | `params: HashMap<usize, SampleParams>` keyed by `objthis` | removed by the `finalize` member (`:29`) |
| `crates/compat/src/natives/plugins/window_ex/util.rs:222-260` | `HashMap<usize, …>` keyed by `objthis` | **token-guarded** by the script-visible `__windowExToken` integer, so a reused address resets to defaults — the correct pattern |
| `crates/compat/src/natives/sound.rs:567,1792-1815` | `vis_buffers: HashMap<usize, usize>` keyed by the destination buffer address | RAII `VisBufferRegistration` removes on drop |
| `crates/compat/src/host.rs:147-148`, `binding.rs:88-90` | engine/services pointer stored as `usize` in the process binding | single binding, torn down with the engine |
| `crates/compat/src/natives/plugins/xp3_filter/mod.rs:223-229`, `fstat/mod.rs:2086`, `text_render/mod.rs:1211` | reconstruct `CompatCtx` from a raw `usize` | engine-bound, no sign test |
| `crates/compat/src/natives/system.rs:2112` | stack address mixed into the UUID seed | randomness, not a handle |
| `crates/tjs/src/lib.rs:233,251` | `retained`/`array` `usize` reinterpreted as `*mut c_void` | the ABI's own raw object-pointer slot, used by `object_handle()`/`object_objthis()` |

**Proposed fix.** Not urgent. Where a native payload exists
(`MenuItemInst.id`), prefer it; otherwise a monotonic id or the
`__windowExToken`-style object stamp removes the reuse hazard. None of this
crosses into script state as an address.

---

## 5. The fix direction — the ABI's native-instance mechanism

The correct cross-class handle is the one `matrix.rs` and `image.rs` already
use:

```rust
// crates/compat/src/natives/plugins/layer_ex/matrix.rs:154-163
let payload = host
    .native_instance_payload(object.raw_id(), "Matrix")
    .map_err(|error| format!("{what}: {error}"))?;
if let Some(payload) = payload {
    let inst = unsafe { &*(payload.as_ptr() as *const MatrixInst) };
    return Ok(Some(inst.core));
}
```

`native_instance_payload` is documented in `crates/tjs/src/lib.rs:2039` as
the `GetNativePtr()` / `IsValid()` contract; it is the reference
`TJS_GET_NATIVE_INSTANCE`. `crates/compat/src/natives/visual/common.rs:363`
(`native_instance_id`) is the same mechanism for scene ids. A hidden script
property is never needed for this and is a port-only, script-observable
member (D34 divergence).

---

## 6. Coverage — what was checked and found clean

The sweep looked for each pattern named in the brief; absence is stated so
the coverage is auditable.

- **L1 (`platform`, `fs`, `media`, `render`).** No script boundary and no
  pointer-as-handle. The `as i64`/`as i32` hits are pixel, audio-frame and
  file-offset arithmetic (`engine/src/text.rs`, `video.rs`, `raster.rs`,
  `platform/media/**`); none casts an address. No pointer is stored in a
  script-visible value. Nothing to fix.
- **L2 (`engine`).** The only pointers leaving `Scene` are the four
  `*_pixel_buffer*` methods (`scene.rs:876,890,898,917`) returning `usize`;
  they are never signed in L2, never sign-checked, and never keyed. The
  signed cast is L3's (§3). No TVP vocabulary.
- **L4 (`vm`) and `app`/`testkit`.** No pointer→signed crossing and no
  script-boundary handle. `testkit/media_fixtures.rs:553` is a byte length
  (`raw.len() as i32`), not an address.
- **`crates/tjs`.** The `usize`↔`*mut c_void` at `:233`/`:251` is the ABI's
  raw object-pointer slot (`Value::object_handle`), not an address-as-handle
  by itself. The only signed pointer cast (`objthis as usize as i64`, `:3508`)
  is a `#[cfg(test)]` probe and never ships.
- **`NonZero*`.** No `NonZero*` is constructed from a pointer anywhere.
- **32-bit narrowing of an address.** None. `draw_device.drawer` /
  `preferredDrawer` are drawer-type enums, not pointers;
  `debug.rs:481` narrows a script count, not an address. The `i32::MAX`
  clamps in `engine` are size/coordinate clamps.
- **Numeric pointer comparison.** None. The `as usize >= len` comparisons in
  `psb_file`/`psd_file` compare offsets to slice lengths, not addresses.
- **Address-space assumptions.** No top-bit-clear assumption and no
  `usize::BITS`/`0x7fff…` handling exists in the tree.

---

## 7. The `pointer-sign` guard

`scripts/check-error-handling.sh` gains a rule named **`pointer-sign`**
(documented in its rule list) that flags, outside `#[cfg(test)]` and
`crates/**/tests/`:

1. a cast to a signed type (`as i8|i16|i32|i64`) whose 80-character receiver
   window contains a pointer-like token (`instance`, `objthis`, `ptr`,
   `addr`, `address`, `handle`, `retained`, `into_raw(`, `as_ptr()`,
   `as_mut_ptr()`, `object_handle()`, `pixel_buffer`); and
2. a sign/positivity test (`> 0`, `>= 0`, `< 0`, `.max(0)`, `.abs()`)
   directly on such a token; and
3. a hidden `name: "__handle"` property registration.

It is deliberately a **review trigger**, not a proof (the repo accepts
over-reporting for these guards).

**Before** (checkout state, no rule):

```
error-handling guard: clean (67 site(s) checked, 57 allowlisted) — scripts/error_handling_allow.txt
```

**After** (rule added, 21 findings all reviewed):

```
error-handling guard: clean (88 site(s) checked, 78 allowlisted) — scripts/error_handling_allow.txt
```

The 21 `pointer-sign` findings are the 17 rank-1/rank-2 sites plus four
false-positive review triggers (a macro reading a numeric KAG field through
`instance`, an FFT `window` size, and a dereferenced cache limit); all were
allowlisted with a reason. The rank-3 internal address-keyed state is not a
syntactic cast or sign test, so the guard does not flag it — it is covered by
the audit and the D37 rule, not by the regex. The six `layer_ex` rows use **code anchors**, so
once the peer's fix removes the getters, the registration lines and the `> 0`
tests, those entries match nothing and the guard prints them as **stale** —
the self-enforcing part.

**What the guard cannot catch** (and therefore why the audit matters):

- a pointer laundered through a `usize`-returning function whose name does
  not say "pointer" (e.g. `scene.main_image_pixel_buffer`) and then cast;
- dataflow across lines or through a struct field (`let h = ptr as i64; … if
  h > 0`);
- a sign test on a variable that was populated by a cast on a different line;
- `NonZero*`/`try_from` applied via a differently-named intermediate;
- a pointer that round-trips through script state (TJS) and is read back;
- a narrowing helper (`fn to_u32(p: *const T) -> u32`) that hides the cast.

---

## 8. What needs a device (aarch64) to confirm

1. The `layerEx` fix: run the same title (`五分`, `krkr-rs-logs*.txt`) and
   confirm `Layer.drawPolygon(app, …)` / `Layer.drawPath(path, …)` no longer
   raises on the phone. This is the D36 device-run requirement.
2. Whether any shipped script numerically compares `HWND`, `buffer`,
   `nativeArray`, `mainImageBuffer` or `interface`; the reference yields the
   same negative value on aarch64, so a script that branches on the sign is
   reference-faithful but untested here.
3. Address-reuse collisions in `get_sample`/`menu_item` under a long real
   session (host-observable too, but only a real game exercises the lifetimes).
4. That the `phase_vocoder` round trip stays bit-exact after any id change.

---

## 9. The decision

Recorded as **D37** in `docs/migration/README.md` §3: a value that may carry
a pointer is never sign-checked or narrowed; a handle that crosses
script-visible state, or that state is keyed by, is an opaque id, never an
address; and the ABI's native-instance mechanism is how one plugin member
obtains another plugin object. `TODO.md` is outside this task's scope, so it
is not updated; if a follow-up is wanted, the natural entry is "remove the
`layerEx` `__handle` mechanism and give `PhaseVocoder` an opaque id".
