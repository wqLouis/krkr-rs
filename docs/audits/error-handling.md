# Error / absence / panic handling audit

- Repo: `/mnt/DATA/Document/code/krkr-rs`
- Branch: `migration/middle-layer-ash`
- Date: 2026-10-12
- Scope: every non-test Rust site matching `.unwrap_or(`, `.unwrap_or_default()`,
  `.unwrap_or_else(`, `let _ = `, `.ok()`, `if let Ok(`, `.unwrap()`, `.expect(`,
  `debug_assert` under `crates/**` (excluding `crates/*/tests/**` and
  `crates/tjs/cpp/**`), plus the `#[non_exhaustive]` question and the two
  house-style findings named in the brief.
- **This is an audit; no source was modified.** No `cargo`/`rustc`/`clippy`
  command was run (AGENTS §0). Method: a throwaway string/comment-aware scanner
  under `/tmp` (never in the repo) that strips `#[cfg(test)]` items and lists
  each site with `file:line` and its enclosing function, then manual
  classification against `reference/cpp/**`.

House rules referenced: AGENTS §4 (errors, fallbacks and "slop"), §5 (house
style), D11 (typed errors), D9 (stateless L1), D32 (no L3 stubs), D34
(script-observable semantics are exact).

---

## 1. Executive summary

The premise "~300 candidate sites" is low by ~2.7×: the non-test tree holds
**824** sites, not ~300. They are **overwhelmingly legitimate**. The real
violations cluster into **five mechanical patterns** and a handful of one-offs.
The report therefore classifies by pattern and gives **fix rules**, not 800
individual tasks.

Headline numbers (robust strip, see §2 for the count caveat):

| pattern | sites | legitimate / benign | real violation | needs a decision |
|---|---:|---:|---:|---:|
| `.unwrap_or*` family | 559 | ~500 | ~45 | ~14 |
| `.ok()` / `if let Ok(` | 117 | ~100 | ~5 | ~12 |
| `let _ =` | 111 | ~85 | ~0 | ~26 |
| `.unwrap()` / `.expect(` | 32 (39 incl. test blocks) | 28 | 0 (all documented invariants / build / test) | 4 |
| `debug_assert` | 5 | 3 | 0 | 2 |
| **total** | **824** | **~716** | **~50** | **~58** |

The five findings that matter, in order:

1. **The PSD parser silently defaults truncated reads to `0` where the
   reference returns `-1`** (`crates/compat/src/natives/plugins/psd_file/parse.rs`,
   ~35 sites). This is both a §4 `Result` fallback *and* a D34 script-observable
   divergence. Rank 1.
2. **`str::parse().unwrap_or(0.0)` is used as the TJS "string → number"
   coercion** in ~20 places. The reference `AsReal()` parses a *prefix*
   (`"12abc"` → `12`, `tjsVariant.cpp:309`); Rust `parse` requires the whole
   string and returns `0`. Script-observable. Rank 1.
3. **The PSD chained decoders return `Option` and `.ok()?` away real
   decompression errors** (`psd_file/decode.rs`), conflating "corrupt" with
   "absent". Rank 1.
4. **`engine::sound::Source::audio()` degrades a decode failure to empty
   audio with a `warn!`** (`engine/src/sound.rs:842`) — a documented-in-D11
   silent fallback (the null plugin is the only allowed exception). Rank 2.
5. **`crates/compat/src/natives/plugins/wutcwf` `debug_assert`s are safe only
   because the caller validates** (they do); the panics list is otherwise clean.

The `.unwrap_or*` family is **not** the problem it looks like: ~90 % are
`Option` defaults on `get`/`first`/`map`/`strip_prefix`/field access. The
dangerous sites are ~7 %, all reachable from a `Result`.

---

## 2. Method and caveats

The scanner strips `#[cfg(test)] mod tests { … }` with a string/comment-aware
brace matcher. A naive brace matcher over-counts because braces inside string
literals (`format!("{tag}")`) and lifetimes (`&'a str`) desynchronise it — the
brief's counts (423/84/57/113/62/57/15/24/5 = 840) match a naive matcher and
**include test-module sites**. The robust pass yields:

| pattern | naive (brief) | robust (non-test) |
|---|---:|---:|
| `.unwrap_or(` | 423 | 418 |
| `.unwrap_or_default()` | 84 | 84 |
| `.unwrap_or_else(` | 57 | 57 |
| `let _ = ` | 113 | 111 |
| `.ok()` | 62 | 60 |
| `if let Ok(` | 57 | 57 |
| `.unwrap()` | 15 | 15 |
| `.expect(` | 24 | 17 |
| `debug_assert` | 5 | 5 |
| **total** | **840** | **824** |

The 15 removed sites are 10 in `crates/android/src/state.rs` and 5 in
`compat/src/natives/plugins/save_struct/mod.rs`, all inside `#[cfg(test)]`
blocks. **Seven of the brief's "39 non-test panic sites" are test-only**; the
production panic count is **32**, of which 13 are in `crates/tjs/build.rs`
(build script) and 4 are in `crates/testkit` (dev-only), leaving **15 runtime
panic sites** (§6).

Receiver classification used a method-name heuristic
(`.map`/`.get`/`.first`/… ⇒ `Option`; `.parse`/`.metadata`/`.try_into`/… ⇒
`Result`). The heuristic cannot see types, so every `Result` row below was read
by hand. Files marked "test" below are dev crates and are out of the runtime
scope but are still counted.

**What this audit cannot determine** (see §10): whether a dropped error is
reference-faithful without reading the matching `reference/cpp/**` site;
whether an `Option`-returning parser *should* be `Result` when the reference
itself has no error path; and the exact runtime reachability of a few compat
probes (`host.member`, `update_layer`, `resolve_face`) without a trace.

---

## 3. Classification: `.unwrap_or` / `.unwrap_or_default` / `.unwrap_or_else`

559 sites. Grouped by receiver, with verdicts.

### 3.1 LEGITIMATE — `Option` defaults (the bulk, ~500)

| receiver family | representative sites | why legitimate |
|---|---|---|
| `.get(..)`/`.first()`/`.last()`/`.pop()`/`.next()`/`.find()` on slices, maps, iterators | hundreds of `arguments.get(n).map(value_as_string).unwrap_or_default()` | absent argument / missing dict key has a defined reference default |
| `Option` struct fields | `app/src/lib.rs:464 self.design_size.unwrap_or(size)`, `engine/src/sound.rs:377 self.total.unwrap_or_else(|| self.frame_count())` | field is genuinely optional |
| `String`/`&str` helpers | `compat/plugin.rs:327 name.rsplit(..).next().unwrap_or(name)`, `engine/text.rs:658 cluster.chars().next().unwrap_or('\u{FFFD}')` | fallible only for the empty input, for which the fallback is exact |
| `try_from` on values that cannot overflow | `fs/ops.rs:65 u64::try_from(bytes.len())` (64-bit), `glyph.rs:269 units_per_em().unwrap_or(1000.0)` | conversion is structurally infallible or the default is the documented reference value |
| `Option` produced by `.map(..)` | `image_function.rs` ~20 `rect.unwrap_or_else(|| full_rect(dst))` | "no rectangle argument" is a defined default |
| `lock().unwrap_or_else(PoisonError::into_inner)` | `compat/plugin.rs:217,246,278,304,313`, `compat/binding.rs:37`, `engine/src/sound.rs:460,480,512,638,657`, `testkit/vm.rs:12` | deliberate poison recovery; no `unwrap_or` on a fallible *operation* |

### 3.2 VIOLATION — `Result` receivers

Sorted by reachability. "Hidden failure" is what the fallback swallows.

| rank | sites | code shape | hidden failure | minimal correct shape |
|---|---|---|---|---|
| 1 | `psd_file/parse.rs:664-666,691-694,701,707-709,713,719-735,752,962-978,1096-1097` (~35) | `r.i32().unwrap_or(0)`, `r.u8().unwrap_or(0)`, `r.u8().map(i32::from).unwrap_or(0)`, `r.unicode_string().unwrap_or_default()` | truncated/corrupt PSD is read as `0`/`""`; the reference `getInt32`/`getCh` return **`-1`** on a short read (`psdparse.h:127-145`, `getUnicodeString` yields empty). `D34` divergence and §4 `Result` fallback. | `Reader::i32`/`u8` return `Result`; either propagate `?` (`load_slice` → `Result<()>`, `PsdError::Eof`) or add a documented `i32_ref(…) -> i32` that returns the reference `-1` sentinel. Do not keep `unwrap_or(0)`. |
| 1 | `psd_file/decode.rs:191,695,726,744` | `inflate_exact(..).ok()?`, `reader.u16().ok()?` | corrupt RLE/zip channel ⇒ `None` ⇒ caller treats it as "no merged image" instead of a decode error | make `decode_channel`/`merged_image` return `Result<_, PsdError>` and `?` the inflate error; keep `Option` only for the *reference's* "no merged image" cases |
| 1 | `psd_file/parse.rs:597,603,1000,1006,1013,1031` | `if let Ok(..) = r.i16()/r.unicode_string()/r.i32()/r.u8()/r.key4()` | same truncated-PSD read silently skipped | same as above |
| 1 | `compat/natives/sound.rs:306,307,313,314,326` | `value.trim().parse().unwrap_or(0)` in `parse_link`/`parse_label` | malformed `.sli` loop numbers become `0` instead of the reference's parse outcome; `.sli` is a game file | return `SliError` (the type already exists) or the reference sentinel; `parse_sli` already returns `Result` |
| 1 | `kag/state.rs:2356,2396` | `field.parse().unwrap_or(0)` in `decode_events` | corrupt save data silently defaults line/count instead of a typed decode failure | propagate a `KagError`/`Result` |
| 1 | `engine/src/sound.rs:842` | `self.decode_entire().unwrap_or_else(\|e\| { warn!(…); DecodedAudio::new(self.spec, Vec::new()) })` | a decoder failure returns **silent empty audio** with only a log line — a degraded substitute (D11), the null plugin is the sole exception | make `audio()` fallible (`Result<&DecodedAudio, SoundError>`) or surface the error where the phase vocoder consumes it; do not fabricate empty audio |
| 2 | `platform/fs/src/host.rs:54` | `file.metadata().map(\|m\| m.len()).unwrap_or(0)` in append-mode `open` | an I/O error reading the file size becomes offset `0`; append writes land at the wrong offset | `?` with `io_error(&display, e)` (the surrounding fn already returns `Result<FileState, FsError>`) |
| 2 | `android/src/log_file.rs:77,153` | `file.metadata().map(\|m\| m.len()).unwrap_or(0)` | log-file size accounting starts at 0 on error | acceptable for best-effort logging, but a `warn!` or `Option` would be more honest; not script-observable |
| 3 | `app/src/window_ex.rs:38-39` | `i32::try_from(surface_size.0).unwrap_or(i32::MAX)` | unrepresentable surface dimension becomes `i32::MAX` | `try_from(..)` + typed/config error (D11 bans arbitrary saturation) |
| 3 | `fs/ops.rs:46,423`; `android/host.rs:117` | `u64::try_from(..).unwrap_or(u64::MAX)` / `unwrap_or(0)` | elapsed-ms / length overflow becomes a sentinel | structurally very unlikely, but the same rule applies: `checked_*` + typed error |
| 3 | `ffmpeg.rs:484`, `mediacodec.rs:683` | `i64::try_from(at_ms.saturating_mul(1000)).unwrap_or(i64::MAX)` | a script-supplied huge seek time saturates instead of erroring | clamp to the stream spec and return `MediaError`/`NotOpen` |
| 3 | `platform/media/src/video/mediacodec.rs` (Android) | `format.i32(..).unwrap_or(0).max(0) as u32/usize` | negative/oversized format value coerced | `MediaFormat::i32` is `Option` (absent key) so the default is fine; the `as` casts are the issue. Android is the one backend D32 allows to be approximate; still prefer `try_from` |

### 3.3 NEEDS A DECISION — `Result` receivers where the reference is the tiebreaker

| sites | question | current |
|---|---|---|
| `compat/natives/sound.rs:1752`, `engine/sound.rs:1863` | `source.frame(..)?.unwrap_or((0.0,0.0))` mixes `?` (propagates) with a `None` (no frame) default — legitimate if "no frame" is silence; confirm against `read_vis_buffer` (TODO M5) | legitimate-looking |
| `platform/render/src/vulkan/device.rs:147` | `CString::new(app_name).unwrap_or_else(\|_\| c"krkr-rs")` — an interior NUL silently renames the instance; app config, not script | needs a decision |
| `compat/natives/plugins/win32_dialog/mod.rs:123` | `CString::new(text).unwrap_or_default()` — an interior NUL silently becomes the **empty** C string; the comment claims it cannot happen | needs a decision (comment may be sufficient) |
| `platform/fs/src/ops.rs:296,300` | `k.strip_prefix(&src).unwrap_or_else(\|_\| Path::new(""))` in `rename` — a non-prefix key silently maps to the root suffix | needs a decision |
| `platform/media/src/image_decode.rs:149,229` | `last_error.unwrap_or_else(..)` on `Option<String>`: the real error *is* returned in the `MediaError`, so this is an `Option` default — legitimate | legitimate |

### 3.4 The `.parse().unwrap_or(..)` coercion family (rank 1, script-observable)

These sites are all the same bug. They live in per-module `value_as_f64` /
`tjs_f64` / `value_as_i64` helpers and in member setters:

```
compat/natives/sound.rs:579                 compat/natives/video.rs:392
compat/natives/visual/timer.rs:646          compat/natives/visual/layer/util.rs:89
compat/natives/plugins/motion_player/helpers.rs:92,208
compat/natives/plugins/motion_player/player_members.rs:585
compat/natives/plugins/text_render/mod.rs:909
compat/natives/plugins/text_render/state.rs:243,253
compat/natives/plugins/layer_ex/appearance.rs:178
compat/natives/plugins/get_sample/mod.rs:183
compat/natives/image_function.rs:223,238
```

Reference: `TJSStringToReal` / `tTJSVariantString::ToReal` call
`TJSParseNumber`, which parses a **prefix** and returns `0` only when *no*
number is present (`reference/cpp/core/tjs2/tjsVariant.cpp:309-317`,
`tjsVariantString.cpp:469-477`). Rust `str::parse::<f64>()` rejects `"12abc"`,
so the port returns `0` where the reference returns `12`. Fix shape: one
reference-faithful `string_to_real(&str) -> f64` helper (the prefix scanner
already exists, fully written, at `compat/natives/visual/layer/util.rs:48`), and
route every `value_as_f64`/`tjs_f64` through it. This is a **D34** fix, not just
a §4 cleanup.

### 3.5 `unwrap_or_default` on `CString` and `env::var`

`CString::new(..).unwrap_or_default()` (`win32_dialog:123`) — see §3.3.
`std::env::var("PATH").unwrap_or_default()` (`fstat/mod.rs:1572`) — an absent
env var is a legitimate empty default.

---

## 4. Classification: `.ok()` and `if let Ok(`

117 sites. The dominant pattern is an **optional probe**: the function's
contract is `Option`, and `None` is the reference's "absent" answer.

### 4.1 LEGITIMATE (majority)

- **Font/face candidates**: `resource.rs:264,273`, `text_render/metrics.rs:82`
  — try each candidate face; first hit wins (the resolver returns
  `Option`/`Result` and the caller tries the next).
- **`scene_mut(ctx)` / `scene_ref(ctx).ok()` guards**: `video.rs:1169,1539,1581`,
  `bitmap.rs:326`, `layer/mod.rs:173,179`, `transitions.rs:777`,
  `focus.rs:72,107`, `layer_ex/draw.rs:2057,2084` — a native invoked with no
  scene is a documented no-op that matches the reference's null manager
  (`LayerTreeOwnerImpl.cpp:385-389`).
- **`RetainedValue::Object` matches** in `set_null` helpers
  (`ctx.rs:154`, `menu_item.rs:183`, `video.rs:420`, `visual/common.rs:246`,
  `window_ex/window.rs:148`, `tree_owner.rs:232`, …): best-effort "set this
  object to null"; the `Err` branch is a missing object, not a failure.
- **Existence probes whose failure surfaces later**:
  `platform/fs/src/host.rs:29` and `ops.rs:169` use
  `if let Ok(meta) = metadata(..) && meta.is_dir()` **only to reject
  directories**; the subsequent `OpenOptions::open` / `remove_file` returns the
  real error via `map_err`. Dropping the probe error is correct here.
- **Encoding fallback**: `compat/text.rs:55 if let Ok(text) = from_utf8(bytes)`
  — the non-UTF-8 branch is the codepage path.
- **Optional metadata**: `fstat/mod.rs:946,958,322-324` `.metadata().ok()`,
  `.modified().ok()` — stat fields are optional.
- **`i32::try_from(..).ok()?`** on pixel coordinates (`pixels.rs:1851`) and
  frame indices (`engine/sound.rs:725,781`) — out-of-range ⇒ `None` is the
  contract.
- **`app/window_ex.rs:26 std::env::var(name).ok()`** — optional env var.
- `android/input.rs:104` — `i32::try_from` + `mouse_button_from_android`; the
  `None` arm logs a warning (good).

### 4.2 VIOLATION / NEEDS DECISION

| rank | sites | hidden failure | fix shape |
|---|---|---|---|
| 1 | `psd_file/decode.rs:191,695,726,744` | see §3.2 | `Result` |
| 1 | `kag/state.rs:1617,2309,2310,2422` | save-data integer/splice parse failures become `None` and are indistinguishable from "field absent" | `Result<Option<..>>` per AGENTS §4 |
| 2 | `compat/natives/sound.rs:1266` | `.sli` read error conflated with "no side-car"; a permission/I-O error is silently treated as no loop info | keep optional, but log or distinguish `NotFound` from other `FsError` |
| 2 | `fstat/mod.rs:946,958` | `entry.metadata().ok()` — a real I/O error yields a missing mtime | distinguish `NotFound`; low impact |
| 2 | `resource.rs:225,233` | `let _ = self.resolve_face(ctx, DEFAULT_FACE)` — a font-mapping failure is dropped | surface or record the error |
| 2 | `visual/layer/mod.rs:342` | `let _ = scene.update_layer(id, …)` — a missing layer silently no-ops | decide reference behaviour; if the reference raises, propagate |
| 3 | `compat/natives/sound.rs:1236` | `let _ = object.buffer.poll(channel)` — a poll error is dropped | surface `SoundError` |
| 3 | `visual/layer/pixels.rs:1685,1783` | `main_bitmap(..).ok()…map_or(0, …)` — a missing bitmap yields pixel `0` | likely reference; confirm |
| 3 | `engine/src/storage.rs:593` | `strip_prefix(&self.game_dir).ok()?` — a path outside the game dir yields `None` | contract is relative names; confirm |

Note: `image_decode.rs:140` `if let Ok(format) = image::guess_format(..)` and
the `Err(_) => {}` fall-through after a failed TLG probe are **legitimate**: the
final `Err(MediaError::Decode)` still fires with the best message.

---

## 5. Classification: `let _ =`

111 sites. The brief calls this out; the finding is that **almost none of them
are §4 violations**, because §4 targets `let _ = <fallible result>;` and most
sites discard a void or already-`?`-propagated value.

### 5.1 LEGITIMATE

- **`?` still present**: `psd_file/descriptor.rs:136,140,141,146,147,153,154,162,218,219`
  and `text_render/layout.rs:95,114,118,124` all read `let _ = f()?;` — the
  `?` propagates the error; only the successful value is discarded.
- **Infallible writes**: `write!(out, …)` into `String` (`state.rs:250`,
  `psb_file/mod.rs:196`).
- **Cleanup / best-effort**: `let _ = self.codec.stop()` (`mediacodec.rs:357`),
  `let _ = device.device_wait_idle()` (`vulkan/mod.rs:416,490,905,962`),
  `let _ = file.flush()` / `remove_file(&temp)` (`android/log_file.rs`,
  `android/state.rs:175,180,186`, `tjs/build.rs`), `let _ = env.exception_clear()`
  (`android/jni_fields.rs:109`).
- **Already-checked/unused parameters**: `let _ = m;` (`platform/media/src/*`),
  `let _ = fs;` (`render/glyph.rs`), `let _ = engine;` — suppress unused
  parameters; not fallible. (Minor §5 smell; deleting the parameter is better.)
- **`OnceLock::set` / `try_init`**: `state.rs:120`, `app/main.rs:324` — the
  `Err` means "already set", not an error.
- **`take_*` request drains**: `app/game.rs:97-99` return `Vec`.
- **Reference-documented ignored callbacks**: `kag/mod.rs:246,264,297`
  (`onLabel`/`onScript`/`onScenarioLoaded`) carry the comment "a handler error
  is ignored, matching the reference" (`KAGParser.cpp:1163-1170`). This is
  **LEGITIMATE** and is the model for the other `call_member` sites.
- **`OnceLock` table cache**: `extrans_ripple.rs:384` — see §6.

### 5.2 NEEDS A DECISION

| sites | hidden failure | fix shape |
|---|---|---|
| `compat/natives/{async_trigger,cdda_midi,menu_item,sound,timer,visual/window,visual/timer}.rs` `let _ = host.set_member(event, "target", …)` (~8) | a failure to set the event's `target` is dropped; if the event object is not a plain object this is a TJS type error | if the reference cannot fail here, document it; otherwise propagate |
| `compat/natives/kag/mod.rs` `onLabel`/`onScript` | **legitimate** (reference-ignored) — leave | — |
| `compat/resource.rs:225,233` | dropped font-mapping error | surface |
| `visual/layer/mod.rs:342` | dropped `update_layer` error | surface |
| `sound.rs:1236` | dropped `poll` error | surface |
| `layer_ex/movie.rs:239,366` | `let _ = slot.overlay.stop()` — dropped stop error | cleanup; likely fine |
| `layer_ex/gdiplus.rs:324` | `let _ = ctx.delete_member(...)` | cleanup |
| `ffmpeg.rs:368,524` | `let _ = decoder.send_eof()` at EOF | likely harmless; decide |
| `null_plugin.rs:103,177` | `set_call_missing` / `set_default_member` results dropped; D14 requires a `warn!` per call, which is present elsewhere | confirm |

The `let _ = set_member` cluster is the only sizable mechanical cleanup; it is
not a reachable panic and is low priority.

---

## 6. Panic / `expect` site verdicts

The brief lists 39 panic sites. **7 are inside `#[cfg(test)]` blocks** — all
in `crates/android/src/state.rs` (the other 8 removed test sites are
non-panic `.unwrap_or`/`let _`/`.ok()` calls in `android/src/state.rs` and
`compat/natives/plugins/save_struct/mod.rs`).
Of the remaining 32, 13 are in `crates/tjs/build.rs` (build script, not
runtime) and 4 are in `crates/testkit` (dev helper). **15 runtime sites** are
listed below; all were read in context.

| site | verdict | reasoning |
|---|---|---|
| `android/src/gesture.rs:231` `.expect("checked just above")` | **LEGITIMATE** | `is_some_and(contact.id==id)` immediately precedes `self.primary.take()`; `&mut self`, no intervening call |
| `compat/src/ctx.rs:207` `.expect("state is keyed by TypeId")` | **LEGITIMATE** | `AttachmentState` is keyed by `TypeId` and entries are never removed/replaced; `downcast_mut::<T>` for the very `T` used as key cannot fail |
| `compat/src/text.rs:116-117` `.expect("8 header bytes")` | **LEGITIMATE** | `bytes.len() < CONTAINER_ZLIB_HEADER` (== 21) is rejected above; `bytes[5..13]`/`[13..21]` are in range for every input |
| `compat/src/natives/sound.rs:526` `.expect("spawn_channel returns a live id")` | **LEGITIMATE** | id is returned by `mixer.spawn_channel()` and used immediately; internal invariant |
| `compat/src/natives/kag/state.rs:1284` `.last().expect("checked non-empty")` | **LEGITIMATE** | guarded by `if self.macro_args.is_empty() { return … }` |
| `compat/src/natives/kag/state.rs:1404,1407` `storage.expect("checked")`, `label.expect("checked")` | **LEGITIMATE** | both are `.expect` after `is_some_and(\|s\| !s.is_empty())`; the bool/`Option` correlation is local and immediate |
| `engine/src/trans/extrans_ripple.rs:384` `.expect("table cache initialised above")` | **LEGITIMATE** | reached only in the `TABLE_CACHE.set(..) == Ok(())` arm; `OnceLock::get` cannot be `None` after a successful `set` |
| `platform/fs/src/lib.rs:220` `.expect("MemoryStore mutex poisoned…")` | **NEEDS A DECISION** | deliberate and documented: poisoning is treated as a bug. `MemoryStore` ops have no indexing/panic path (checked), so it is unreachable from malformed input. A typed `FsError::Poisoned` would satisfy D11 better; `FileSystem::memory()` is used by `app/src/lib.rs:311` (headless) and tests |
| `platform/fs/src/xp3.rs:713` `.expect("length checked by the caller")` | **LEGITIMATE (fragile)** | `prefix::<N>` is private; every call site takes its slice from `split_chunk_header`, which returns only when `data.len() >= 12`, and `split_at(4)` before `prefix(size)`. Correct today; the comment is the only guard |
| `platform/media/src/video/ffmpeg.rs:337` `.expect("resampler")` | **LEGITIMATE** | same `if resampler.is_none()` block assigns `resampler = Some(converter)`; the `expect` runs after that assignment |
| `platform/media/src/video/ffmpeg.rs:577` `.expect("scaler was just built")` | **LEGITIMATE** | `rebuild` is `true` when `state.scaler` is `None`, so the block above always sets `Some` before the `expect` |
| `platform/platform/src/linux.rs:183` `.expect("window exists after creation")` | **LEGITIMATE** | `self.window = Some(PlatformWindow{..})` immediately above, same function |
| `platform/media/src/tlg.rs:168` `.unwrap()` on `try_into()` | **LEGITIMATE (infallible)** | `.get(off..off+4).ok_or(..)?` guarantees exactly 4 bytes; `[u8;4]::try_from(&[u8])` cannot fail. `map_err` would be tidier, but there is no reachable panic |
| `crates/tjs/build.rs` (13 sites) | **LEGITIMATE (build-time)** | `env::var_os(..).unwrap()`, `create_dir_all(..)`. A failure should abort the build; not runtime |
| `crates/testkit/src/lib.rs:47,48`, `media_fixtures.rs:30,158` | **LEGITIMATE (dev crate)** | test fixture builders |

**Conclusion:** no panic site is reachable from malformed game input. The two
worth a follow-up are the deliberate `fs::lock` panic (policy choice) and the
`prefix` helper (defence-in-depth: it could return `Result`). There is no
`expect` on a decoder/`Option` chain.

There is, however, one **`debug_assert` data-integrity** question — see §8.

---

## 7. `#[non_exhaustive]` and wildcard `_ =>` arms

**Question answered: no `#[non_exhaustive]` enum that L3 matches
variant-by-variant exists.**

- `crates/compat` declares exactly two `#[non_exhaustive]` enums:
  `SliError` (`natives/sound.rs:171`) and `PrerenderedFontError`
  (`resource.rs:673`). Both are `pub(crate)` `thiserror` error enums; they are
  not used as control-flow discriminants, so the attribute is correct and
  harmless.
- Every other `#[non_exhaustive]` is in L1/L2 (`api/src/error.rs`, `config.rs`,
  `fs/src/lib.rs`, `media/**`, `engine/**`, `platform/**`). Compat only
  **constructs** the engine enums (`TransKind::Wave` etc. in
  `visual/layer/transitions.rs`, `LoopCondition` in `sound.rs`) and never
  `match`es them — verified by grep for `match` on `TransKind`, `LoopCondition`,
  `ScalingMode`, `StartupScript`, `ScrollFrom`, `ScrollStay`, `PlayStatus`,
  `PeriodReason` (zero hits).

`_ =>` arms in compat number **224**. They are overwhelmingly on non-enum
scrutinees and are fine:

- `match v.ty { VAL_INTEGER => …, _ => … }` — integer type tag, FFI (≈60).
- `match arguments.get(n)`, `match host.member(..)`, `match a.get(n)` — `Option`
  / `Result` (≈80).
- `match value.as_str()` / `to_ascii_lowercase().as_str()` — strings (≈30).
- `match (x, y)` tuples, `match digit`, `match depth`/`mode`/`channel.id`
  integers — decoders.

A small set matches a **local enum** with `_ =>`, which AGENTS §4 says should
be exhaustive so a new variant is a compile error. None is a live bug; they are
low-priority hardening:

| site | enum | arm |
|---|---|---|
| `plugins/motion_player/psb.rs:58,66,77,88,96,104` | `PsbValue` | `_ => None/false` |
| `plugins/psd_file/descriptor.rs:48,56` | `DescriptorValue` | `_ => None` |
| `plugins/psd_file/parse.rs:236,263` | `Key`/id enum | `_ => Self::Invalid` / `13` |
| `plugins/text_render/state.rs:244,254` | `TjsValue` | `_ => 0 / 0.0` |
| `natives/visual/layer/blend.rs:103` | `&str`/id | `_ => BlendMode::SourceOver` |

`TjsValue` is a public, non-`#[non_exhaustive]` FFI mirror in `tjs/src/lib.rs:620`
with a deliberately closed variant set, so adding a variant would be a
compiler-visible change to the binding anyway. To comply with §4, replace the
`_` arms on `PsbValue`/`DescriptorValue` with explicit variant lists.

---

## 8. `debug_assert` (5 sites)

| site | condition | verdict |
|---|---|---|
| `platform/render/src/glyph.rs:222` | `coverage.len() == width*height` | **LEGITIMATE** — internal rasterizer invariant; no unsafe/indexing relies on it in release |
| `platform/render/src/glyph.rs:300` | `size_px > 0.0` | **NEEDS DECISION** — `size_px` derives from a script-settable font size; release proceeds with a non-positive scale. Not a memory-safety issue |
| `compat/natives/kag/state.rs:1728` | `self.macros.len() == n` (from save data) | **NEEDS DECISION** — a save-file integrity field; the map is not indexed by `n`, so release is safe. Informational only |
| `compat/natives/plugins/wutcwf/mod.rs:305,306` | `block.len() == bytesperblock`, `samples.len() == samplesperblock*stride` | **LEGITIMATE (safe because of adjacent validation)** — `TcwfHeader::parse` rejects `bytes_per_block < BLOCK_HEADER_SIZE (32)` and the samples/layout relation; `decode_tcwf` slices exactly `bytes_per_block` and allocates exactly `samples_per_block*channels`, and checks the block count against the bytes present. The asserts are redundant, not the only guard |

No `debug_assert` is the sole guard for a release-visible data-integrity
condition.

---

## 9. Two house-style findings (confirmed)

### 9.1 `crates/engine/src/video.rs:477` — `set_play_rate` is a pure field assignment

```rust
pub fn play_rate(&self) -> f64 { self.play_rate }          // :471
pub fn set_play_rate(&mut self, rate: f64) { self.play_rate = rate; }  // :477
```

The field is private (`video.rs:230 play_rate: f64`). AGENTS §5: "A setter
whose body is only `self.field = arg` must be a public field. A getter for a
public field is deleted." **Confirmed violation.** Fix: make `play_rate` a
public field and delete both `play_rate()` and `set_play_rate`. The doc comment
("The reference stores whatever it is given and never validates it") supports a
public field. Internal writers at `video.rs:315,377,395,410,686` assign it
directly already.

### 9.2 `crates/compat/src/natives/cdda_midi.rs:42` — dead no-op seam

```rust
pub fn set_disc_audio_decoder(_services: &CompatServices, _decoder: AudioDecoder) {}
```

Re-exported from `compat/src/lib.rs:65`, has **no callers**. TODO M5b (P2,
D32) already says: "Delete the function and the re-export." **Confirmed.** It is
a D32 violation (a stub in L3 that no script observes, but `_services`/`_decoder`
are dead). Note the surrounding `CDDASoundBuffer`/`MIDISoundBuffer` classes are
**not** stubs: the reference itself compiles the playback surface out
(`ENABLE_CDDA`/`TVP_ENABLE_MIDI` undefined) and the port matches the disabled
build. Only the seam is dead.

---

## 10. Enforcement recommendation

Mirror `scripts/check-stateless.sh`: a repo guard that fails without a
compiler. The semantics split cleanly.

### 10.1 Mechanically checkable (grep + allowlist)

A proposed `scripts/check-error-handling.sh`:

1. **`Result`-receiver fallbacks.** Grep for
   `\.unwrap_or(_default|_else)?\(` whose receiver chain contains a
   known-`Result` method: `parse`, `metadata`, `read`, `write`, `read_dir`,
   `try_into`, `CString::new`, `status`, `send_eof`, `poll`, `decode`,
   `inflate`, `decode_entire`, `lock`, `strip_prefix` (on `Path`),
   plus the crate's own `Reader::{i32,u8,u16,u32,u64,unicode_string}`, `integer`.
   Fail unless the line carries a `// fallback: <reason>` marker or the
   `file:line` is in an allowlist (like `native_parity_allow.txt`). This alone
   catches the PSD parser and `.sli` families.
2. **`.parse().unwrap_or(`** — an unconditional fail (always a coercion bug
   after §3.4), suggesting `string_to_real`.
3. **`let _ = <name>`** where `<name>` is in a fallible-call list — accept
   `let _ = X?;` and an allowlist for cleanup (`stop`, `wait_idle`, `flush`,
   `remove_file`).
4. **`#[non_exhaustive]` in `crates/compat/**`** — allow only the two error
   enums by name; fail on any new one.
5. **`debug_assert` in `crates/**` decoders** — warn-only list.
6. **`Result→Option` parsers**: fail when a `pub(crate) fn … -> Option<…>` in
   `*decode*.rs`/`*parse*.rs` calls `.ok()?` on an inflate/decode.

### 10.2 Needs a human (cannot be mechanised)

- Whether a dropped error is **reference-faithful** (`onLabel`/`onScript` are,
  the `.sli` numbers are not). Every allowlist entry needs a
  `reference/cpp/...:line` justification, exactly as tests do (D30).
- Whether an `Option`-returning parser *should* become `Result` when the
  reference has no error path (the PSD `getInt32` `-1` sentinel is the
  counter-example: the fix is a sentinel, not a typed error).
- Wildcard arms on local enums (semantic).
- `try_from(..).unwrap_or(MAX)` saturation where the input is structurally
  bounded.

### 10.3 Would a curated `[workspace.lints.clippy]` set help?

**Mostly no — it would flood.** None of the lints targets the actual bug class:

- `clippy::unwrap_used` / `expect_used` — 47 sites, almost all documented
  invariants; enabling workspace-wide just forces allows.
- `clippy::indexing_slicing` — thousands of hits in decoders; useless.
- `clippy::wildcard_enum_match_arm` (restriction) — 224 arms, ~all on integers
  and `Option`; flood.
- `clippy::panic_in_result_fn` / `unwrap_in_result` (restriction) — would catch
  a few `expect`s inside `Result`-returning decoders; small win, but these are
  restriction lints and must be opted in per-module, not workspace-wide.
- There is **no** clippy lint for `unwrap_or` on a `Result`, nor for `.ok()`
  that drops an error.

Recommendation: **do not** enable a broad clippy set. Enable at most
`clippy::panic_in_result_fn` and `clippy::unwrap_in_result` **scoped** to the
decoder/parser modules (`platform/fs/src/xp3.rs`, `platform/media/src/tlg.rs`,
`compat/natives/plugins/**`), and rely on `check-error-handling.sh` +
allowlist for the rest. The `surface_lock.py` model (snapshot + reviewed diff)
fits the allowlist.

---

## 11. What I could not determine

1. **Reference behaviour of the PSD numeric sentinels.** I confirmed the
   reference `IteratorBase::getInt32`/`getCh` return `-1` on a short read
   (`psdparse.h:127-145`) and `getUnicodeString` yields empty, which makes the
   `unwrap_or(0)` sites a divergence — but I did not verify whether the port's
   `PsdError` plumbing can thread a `Result` through `load_slice`/`process`
   without changing the reference's "return true" contract, so the fix shape
   (sentinel vs `?`) needs the implementer's judgement.
2. **The `.sli` reference error contract.** `parse_sli` returns `SliError` only
   for unsupported formats; whether a non-numeric `from=` field must throw
   `TVPInvalidLoopInformation` (`WaveImpl.cpp:3077-3080`) or default to `0` was
   not read line-by-line.
3. **Exact reachability of the compat probes** (`host.member`, `update_layer`,
   `resolve_face`, `buffer.poll`) from a script path versus an internal
   invalid-state path — classification is based on the local contract, not a
   dynamic trace.
4. **`try_from(...).unwrap_or(MAX)` reachability.** `surface_size`, `at_ms`,
   `bytes.len()` are structurally bounded in practice; I did not prove the
   bounds.
5. **Android MediaCodec `as` casts.** The Android backend cannot be built on
   this host (no NDK) and is the one backend D32 allows to be approximate; the
   `format.i32(..).unwrap_or(0).max(0) as u32` sites were classified from the
   source only, not run.
6. **Whether `.map_err`-based propagation would change error *strings*** that a
   script can observe (D34) for the `.sli`/PSD fixes.

---

## 12. The guard, and what it found that this audit did not

`scripts/check-error-handling.sh` enforces §4 mechanically (a bash wrapper over a
python3 scanner; no compiler), with `scripts/error_handling_allow.txt` holding
the sites this audit classified as legitimate. It over-reports by design — a
receiver that is an `Option` looks like one that is a `Result` to a scanner — and
the allowlist is where that is resolved. Against the tree it found, beyond this
audit's list:

* `crates/compat/src/natives/storages.rs` — `Path::strip_prefix(..).unwrap_or_else(|_| normalize_storage_name(name))`: a key without the expected prefix silently falls back to the original name.
* `crates/compat/src/natives/visual/layer/mod.rs` — `i32::try_from(w/h).unwrap_or(i32::MAX)`: an out-of-range size becomes a saturation cap.
* `crates/platform/media/src/video/ffmpeg.rs` — `u16::try_from(channels).unwrap_or(0)`: an impossible channel count becomes zero.
* `crates/platform/media/src/video/ffmpeg.rs` — `u64::try_from(ms).unwrap_or(0)`: a negative timestamp becomes zero.
* `crates/platform/media/src/video/mediacodec.rs` — the same `u64::try_from(pts_ms).unwrap_or(0)` shape.

It also reports nine sites this audit ranked but did not put in the in-flight
set (`crates/android/src/host.rs`, `crates/android/src/log_file.rs`,
`crates/app/src/window_ex.rs`, `crates/platform/fs/src/ops.rs`,
`crates/platform/media/src/video/{ffmpeg,mediacodec}.rs`) and thirty sites this
audit marked *needs a decision*, which are not allowlisted because "needs a
decision" is not "legitimate". Both groups are tracked in `TODO.md` §14. The
guard joins the gate only when its output is clean.
