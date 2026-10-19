# Audit — SIGSEGV during an in-game save

Status: read-only audit. No source was modified. HEAD at time of audit:
`d840cce`.

Reference oracle: `reference/cpp/**`. The only port code that runs inside the
`saveStruct` write path is the custom TJS2 stream layer in
`crates/tjs/cpp/streams.cpp`; everything else is vendored TJS2.

---

## 0. What actually runs on a save (facts)

The game is `HULOTTE — 相见5分属于我` at `$KRKR_TEST_GAME`.
`system/status.tjs:17` sets `__FILE_CRYPT__ = 1`, so:

| step | caller | ported member |
|---|---|---|
| thumbnail | `system_advscreen.tjs:7427` | `Layer.saveLayerImage` → `crates/compat/src/natives/visual/layer/pixels.rs:2141` → `crates/app/src/image_codec.rs` → `media::encode_image` |
| save data | `system_advscreen.tjs:7440` `temp.saveStruct(DATA_PATH+file, "zo<GetImageFileSize>")` | **vendored TJS2** `Array.saveStruct` (`reference/cpp/core/tjs2/tjsArray.cpp:434`) + port `TextWriteStream` (`crates/tjs/cpp/streams.cpp:534`) |
| manager file | `system_system.tjs:450` `_obj.saveStruct(DATA_PATH+"saveMng.dat", "z")` | vendored `Array.saveStruct` (same) |

`GetImageFileSize` = `imageWidth*imageHeight*4+54` (`system/utility.tjs:2205`);
`SAVEDATA_THUMB_WIDTH/HEIGHT = 192/105` (`system/status.tjs:140-141`), so the
append offset is `80694`.

`temp[0] = _param`, where `_param` is the `Savedata` **Dictionary**
(`system_advscreen.tjs:7359`), populated by `Savedata.set(adv)` from the live
`ADVScreen` (`system_advscreen.tjs:7006-7190`).

### Measured behaviour (release binary, headless)

I ran the save primitives directly through an `AfterStartup.tjs` game hook
(game file only, since restored):

* `saveMan.save()` (the `Array.saveStruct` "z" path) → **completes**; writes a
  complete save.
* `new Savedata().save(file, 0, /*adv=*/null)` (the `"zo<size>"` path with the
  `FRM_0351` thumbnail) → **completes**; file = `80694 + 21 + zlib` bytes.
* `Array.saveStruct` with 300 000 string elements ("z" and `"o<offset>"`) →
  **completes**.
* A Dictionary nested in an Array → **completes**, but (see C3) the output
  contains extra keys.
* A 40 000-deep nested Dictionary → **`fatal runtime error: stack overflow,
  aborting`** (SIGSEGV converted to abort by the Rust handler). See C2.

So the crash is **not** reached by the plain `saveStruct` primitives; it needs
the real `adv != null` `Savedata.save` path (or a specific value in `_param`).

### Leftover crash artifacts

`$KRKR_TEST_GAME/savedata/save01_001.bmp` and `qsave01.bmp`
(both 80699 bytes, written 21:08 / 21:14 while the user was testing) are exactly
`BMP 192x105 (bfSize 80694)` **plus 5 bytes** `FE FE 02 FF FE` — the crypt
signature + UTF-16 BOM that `TextWriteStream` writes first. There is no zlib
size header (16 bytes) and no compressed payload. This localises the fault to
the text-stream write stage (thumbnail written, structured payload not), and
points at the port's `TextWriteStream` (C1) or at the serializer (C2/C3); it
does **not** point at the image encoder.

---

## C1 — port `TextWriteStream` / structured-write stage  (confidence: medium-high for the save crash)

Files:
* `crates/tjs/cpp/streams.cpp:534-621` — `TextWriteStream` (the port replacement
  for the reference `tTVPTextWriteStream`, `reference/cpp/core/base/TextStream.cpp`).
* `crates/tjs/cpp/streams.cpp:546-566` — `write_raw` (zlib).
* `crates/tjs/cpp/streams.cpp` — constructor: writes the crypt header
  (modes 0/1 and 2: `FE FE <mode>` + `FF FE`, 5 bytes), `deflateInit`, then the
  two 64-bit `compression_size_position` placeholders.
* `crates/tjs/cpp/streams.cpp:623-656` — destructor patches the sizes.
* `crates/tjs/cpp/streams.cpp:716-721` — `tjs2_wire_stream_factories`.

### The `FE FE` header offset (resolved: the vendored offset-4 read was a reconstruction error)

The vendored reader starts a mode 0/1 payload at a fixed offset 4
(`reference/cpp/core/base/TextStream.cpp:85-88`, `len = (size - 4) / 2`) and a
mode 2 payload at offset 21 (`:104-116`). Its writer emits a 3-byte signature
and then an unconditional `FF FE` BOM (`:269-280`), i.e. `FE FE <mode> FF FE`
(5 bytes) for modes 1 and 2; `crypt_mode_sig[3] = 0` is declared but never
written. The mode-0/1 `+4` is inconsistent with the writer, with the same
file's mode 2 (`+5`), and with the shipped engine.

The shipped engine's `tTVPTextReadStream` consumes the 3-byte signature, then
reads 2 more bytes and requires them to be `FF FE` (raising
`TVPUnsupportedCipherMode` otherwise), and only then reads the mode 0/1
payload — i.e. offset 5, matching mode 2 and the writer. The device evidence
confirms it the other way: the boot failures in `krkr-rs-logs (6).txt` have odd
drop counts (`289 = 2·144 + 1`, `9 = 2·4 + 1` — a trailing half unit), and
encrypting a real 70 KB UTF-16 script with the reference mode-0/1 cipher shows
an offset-4 read yields 32 unpaired surrogates plus one trailing byte while
offset 5 matches the plaintext exactly. This is the AGENTS §9.9 case: the
vendored reconstruction contradicted the shipped data, so the port follows the
shipped data.

The reader still governs, but it is the shipped reader at **offset 5 with a
required `FF FE` BOM**, not the reconstruction's offset 4. The port now reads
offset 5, requires the BOM, and writes the same 5-byte `FE FE <mode> FF FE`
header for modes 0/1 as mode 2, so a port-written stream round-trips. Pinned by
`crates/compat/src/text.rs::fefe_cipher_payload_starts_at_offset_five`,
`fefe_cipher_requires_the_byte_order_mark`,
`fefe_containers_keep_every_code_unit`,
`crates/tjs/src/lib.rs::rejects_a_cipher_container_without_the_bom`,
`writer_emits_the_reference_crypt_header`, and
`crates/compat/tests/storage_media_prefix.rs::save_struct_strips_the_file_media_prefix_and_doubled_slash`.

Why it can SIGSEGV:
* The placeholders after the BOM are written **after** `deflateInit`. The
  artifact ends right after the BOM, i.e. the fault is between `:604` and
  `:617`. The only operation there is `deflateInit(&zs, compression_level)`
  and `stream->GetPosition()`. A crash inside zlib is possible if the state or
  the stream is corrupt, but that is not evident statically; the *behaviour*
  (abrupt stop after BOM) is what the artifact shows.
* Separately, `TextWriteStream` is the only port-authored C++ in the
  `saveStruct` path, and it has not been exercised with the real game payload
  in tests (`crates/compat/tests/save_struct.rs` drives the *plugin*
  `saveStruct2`, not the built-in `Array/Dictionary.saveStruct`). A port bug
  here is not circled by any test.

Reference behaviour: the reference's write stream is
`tTVPTextWriteStream`/`tTVPBinaryStream` (`reference/cpp/core/base/TextStream.cpp`),
whose `TJS_BS_WRITE` path writes the `FE FE <crypt>` container and never takes a
raw script mode. The port's mode parser (`:270-303`) is a reimplementation.

Fix I would apply (not applied): first get a backtrace (see §next). If the fault
is in `TextWriteStream`, harden the constructor: check `stream->ok()` before the
`write_u64le` placeholders, guard `deflateInit` failure so the 5-byte header is
never left on disk alone (or fail the native with a typed error), and add a
built-in `Array.saveStruct`/`Dictionary.saveStruct` test that round-trips a
realistic nested payload through the crypt container.

---

## C2 — unbounded recursion in the built-in text serializer  (confidence: medium)

Files:
* `crates/tjs/cpp/tjs2/tjsArray.cpp:1114-1198` — `tTJSArrayNI::SaveStructuredData`
  / `SaveStructuredDataForObject` (**identical** to
  `reference/cpp/core/tjs2/tjsArray.cpp:1114-1198` — I diffed; only cycle
  detection, no depth limit).
* `crates/tjs/cpp/tjs2/tjsDictionary.cpp:417-500` — `SaveStructuredData` /
  `tSaveStructCallback`.

Why it can SIGSEGV: the serializer recurses one native stack frame per nested
Array/Dictionary. It guards **cycles** (`stack` pointer search, `tjsArray.cpp:1160-1168`)
but has **no depth bound**. A deep, acyclic object graph recurses until the
stack guard page → SIGSEGV. On this release build the Rust runtime converts it
to `fatal runtime error: stack overflow, aborting` (I reproduced it with a
40 000-deep nested dictionary). The user's shell message
`terminated by signal SIGSEGV (Address boundary error)` is consistent with a
raw C++ overflow (the Rust handler only catches overflows in Rust frames; a C++
frame blowout is a plain SIGSEGV).

Reference: identical code, so the reference overflows too — **unless** the
reference never sees a graph that deep/hostile. A cycle the guard *misses* is
the real risk: the guard compares `clo.SelectObjectNoAddRef()`, which can differ
from the object that is actually re-entered (e.g. a closure whose `Object` is the
outer object). The port also injects extra members (C3) that add frames.

Fix I would apply: add an explicit depth counter to `SaveStructuredData` /
`SaveStructuredDataForObject` and raise the reference's `TJS_E_RECURSION` (or an
equivalent typed error) past a bound; do **not** rely on the pointer-stack cycle
test alone. This is a divergence from the reference only in the error case, and
it turns a memory fault into a script error (D11).

---

## C3 — `Dictionary` enumeration leaks ABI-attached plugin methods into the save  (confidence: high it is a real bug; medium it is the crash)

Files:
* `crates/compat/src/natives/plugins/save_struct/mod.rs:575-598` — `saveStruct2`
  / `toStructString` are registered with `register_instance_members` (the
  reference attaches them as `TJS_STATICMEMBER`,
  `reference/cpp/plugins/saveStruct.cpp:312,364-366`).
* `crates/compat/src/natives/plugins/scripts_ex/mod.rs` — same class of
  instance-member attach (`clone`, `foreach`, …).
* Serializer: vendored `tTJSDictionaryNI::SaveStructuredData`
  (`crates/tjs/cpp/tjs2/tjsDictionary.cpp:417-447`) calls `Owner->EnumMembers`,
  which therefore walks the attached methods.

Measured: serializing `a[0] = %[x:1,y:2,s:"hi"]` yields
```
"toStructString" => null /* object recursion detected */,
"saveStruct2"  => null /* object recursion detected */,
```
i.e. the port writes keys the reference does not. The reference's
`Dictionary.saveStruct` is static, so its `EnumMembers` never reports it
(`reference/cpp/core/tjs2/tjsDictionary.cpp:133-184`; the same fact is called
out in `docs/audits/object-write-dispatch.md:179`).

Why it can SIGSEGV: the attached members are `tvtObject` closures processed by
`SaveStructuredDataForObject`. For these the port currently reaches the
"recursion detected" branch, but the branch is only reachable because
`SelectObjectNoAddRef()` happens to equal the owning Dictionary. If the
attached-member marshalling ever returns a different receiver (the receiver
fallback documented in `docs/audits/object-write-dispatch.md` §1, and the
`paramMacros` bug the main agent traced at `crates/compat/src/natives/kag/mod.rs:1627-1660`),
the guard no longer matches and the serializer recurses into the method object —
an unbounded, non-cyclic-looking recursion (C2). Even without a crash this
corrupts the save format (extra `saveStruct2`/`toStructString` entries under
every nested dictionary), which is script-observable (D34).

Fix I would apply: make the serializer/Dictionary surface match the reference by
not exposing plugin class members as own instance members on builtin
`Array`/`Dictionary` (the root fix the main agent identified for `paramMacros`),
or, minimally, have `SaveStructuredDataForObject` skip ABI-attached methods
(same predicate as `scripts_ex::is_attached_method`). The save format then
matches the reference byte-for-byte.

---

## C4 — `getVisBuffer` writes through a script-supplied integer pointer  (confidence: high it is real UB; low it is *this* save crash)

File:
* `crates/compat/src/natives/sound.rs:1697`
  `let dest = value_as_i64(&raw_args[0]) as usize as *mut i16;`
* write: `crates/compat/src/natives/sound.rs:1639-1675` (`read_vis_buffer`,
  `*dest.add(index) = …`, no bound on the allocation behind `dest`).
* added by commit `2af9b9b` (previously returned `0`).

Reachability:
* `value_as_i64` (`crates/compat/src/natives/system.rs:283-291`) returns `0`
  for `VAL_OCTET`/`VAL_OBJECT`, so an Octet/object arg becomes a null pointer and
  `read_vis_buffer` returns early. A **string** is parsed as an integer, so
  `getVisBuffer("1234", n, c)` would dereference `0x4d2`.
* The only legitimate caller is the port's own `get_sample` plugin
  (`crates/compat/src/natives/plugins/get_sample/mod.rs:157-173`,
  `:274-282`), which passes `buffer.as_mut_ptr()` of a live `Vec<i16>` whose
  length equals the count it passes. That path is sound.
* I found **no** game script that calls `getVisBuffer` directly
  (`grep -rn getVisBuffer /tmp/gamext` → none; only
  `system/selectitem.tjs:1990` reads the plugin's `sampleValue`). So a plain
  save does not reach it.

Reference: `tjs_int16 *dest = (tjs_int16 *)(tjs_int64)(*param[0]);`
(`reference/cpp/core/sound/win32/WaveImpl.cpp:3517`), called by the
`getSample` plugin with a plugin-owned `short*` cast to `tTVInteger`
(`reference/cpp/plugins/getSample.cpp:20-26,85`). The reference has the same
unchecked-pointer contract, so matching it exactly means keeping the hazard.
Note the one divergence: the reference's `AsInteger` **throws** for an Octet
(`crates/tjs/cpp/tjs2/tjsVariant.h:862-877`), while the port silently yields `0`.

Fix I would apply: register the pointer the `get_sample` plugin is currently
passing as the *only* accepted `dest` for the duration of the call (per-engine
state), and reject any other value with a typed callback error (or return `0`
like the reference's layout-mismatch path). This preserves the reference-visible
behaviour for the only legitimate use, closes an arbitrary write primitive, and
fits D11. Do this regardless of whether it is the reported save crash.

---

## C5 — image encoder / `saveLayerImage`  (confidence: low)

Files: `crates/compat/src/natives/visual/layer/pixels.rs:2141-2200`,
`crates/compat/src/resource.rs:100-115`, `crates/app/src/image_codec.rs`,
`crates/platform/media/src/image_encode.rs:118-130,260-350`.

* `encode_image` validates `rgba.len() == width*height*4` (`:367-375`) and the
  BMP writer uses `checked_mul`/`checked_next_multiple_of` (`:235-255`); the
  final `out.len() != file_size` check catches writer bugs (`:340-345`).
* `layer_save_layer_image` reads `LayerInst`/bitmap through the scene and clones
  the pixels; no unsafe buffer math.
* The artifacts confirm the thumbnail (192×105 BMP) was written correctly.

Verdict: no memory-safety defect found here. Not the crash.

---

## C6 — FFI lifetime / null-storage  (confidence: low)

* `crates/compat/src/natives/plugins/save_struct/mod.rs:267-281` `output_path`
  does `unsafe { &*pointer }.game_dir()` on the attached storage pointer. The
  pointer is `Some` even if it was attached as null (`ctx.rs:88-91`), and
  `services.storage()` returns `Some(null)` in that case (`ctx.rs:106-108`), so
  `&*null` is UB. This is **not** on the game's save path (the game uses the
  built-in `saveStruct`, not the plugin's `saveStruct2`); it is a latent
  hazard in every `Storages`/plugin that dereferences the pointer the same way
  (e.g. `fstat/mod.rs:242`, `storages.rs:227`).
* The `tjs2_assign_message`/`tjs2_exec_script` context ABI and the retained-map
  lifetime are not exercised by `saveStruct` (which is pure C++). The only
  ABI-crossing call on this path is the `TextWriteStream` (C1).

Fix I would apply: make `attach_storage`/`attach_scene`/`attach_input` reject
null (or make the getters return `None` for null), so `unsafe { &*p }` has a
non-null invariant.

---

## Ranked summary

| rank | candidate | file:line | crash mechanism | confidence it is *this* crash |
|---|---|---|---|---|
| 1 | text-stream write stage | `crates/tjs/cpp/streams.cpp:534-621` | port-authored C++ in the save path; artifacts stop right after its BOM | medium-high |
| 2 | serializer recursion (no depth bound; guard can miss a receiver) | `crates/tjs/cpp/tjs2/tjsArray.cpp:1114-1198`, `tjsDictionary.cpp:417-500` | stack overflow → SIGSEGV (reproduced at depth 40 000) | medium |
| 3 | plugin methods enumerated into the save | `save_struct/mod.rs:575-598`, `scripts_ex/mod.rs` | extra object values fed to `SaveStructuredDataForObject`; format corruption, and recursion if the receiver is wrong | medium |
| 4 | `getVisBuffer` raw pointer | `crates/compat/src/natives/sound.rs:1697` | arbitrary script integer → `*mut i16` write | low for a save (no save caller), high as a real UB |
| 5 | image encoder | `image_encode.rs:118-130,260-350` | none found | low |
| 6 | null storage deref / FFI lifetime | `save_struct/mod.rs:267-281`, `ctx.rs:88-108` | `&*null` UB; not on the built-in `saveStruct` path | low |

## Next step (needed to close this)

The decisive evidence is a backtrace. Please reproduce once and run:

```
gdb -batch -ex run -ex 'thread apply all bt full' --args \
  ./target/release/krkr --base-dir $KRKR_TEST_GAME \
  --archive data.xp3 --archive patch.xp3
```

and save the game. If the top frames are in `TextWriteStream` → C1; if in
`tTJSArrayNI::SaveStructuredData`/`tTJSDictionaryNI::SaveStructuredData` → C2/C3;
if in `read_vis_buffer`/`sound_get_vis_buffer` → C4. The leftover
`save01_001.bmp`/`qsave01.bmp` (image + 5 bytes) say it is C1/C2/C3, not C4.
