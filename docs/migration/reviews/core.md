# Core layer code review — `api`, `fs`, `media`

Scope: `crates/platform/api/`, `crates/platform/fs/`, `crates/platform/media/`.
Read against `docs/migration/02-target-architecture.md` (D9/D10, layer/system
boundaries) and `docs/migration/03-middle-layer-api.md` (the contract), plus
`05-media-engine.md` and `06-file-system.md`.

Method: source read-only. No build/test/cargo was run. Severity:

| Severity | Meaning |
|----------|---------|
| **Critical** | Can panic/abort the process or perform an out-of-bounds/incorrect memory access from a normal public call. |
| **High** | Memory/CPU amplification or silent data corruption on realistic untrusted input; or a D9/contract violation with functional consequences. |
| **Medium** | Correctness edge cases, unnecessary cost on hot paths, API/consistency drift that will bite later. |
| **Low** | Local idiom/robustness issues; worth fixing opportunistically. |
| **Nit** | Style/consistency only. |

Overall the three crates are in good shape structurally: no `unsafe` anywhere
(`#![forbid(unsafe_code)]` in all three lib roots), typed `thiserror` error
values, D9 state separation applied to `Media`/`FileState`, and genuinely
careful numeric code in `tlg.rs`/`yuv.rs`. The problems are concentrated in
untrusted-length arithmetic on the write/decode paths and in a handful of
consistency/dependency issues.

---

## 1. Consistency

### 1.1 Naming and construction surface — Low

The contract consistently spells constructors `create_*(config)`
(`03-middle-layer-api.md:79`, `:289`, `:316`), and the crates do provide them:

- `fs`: `create_file_system(config)` (`crates/platform/fs/src/lib.rs:159`)
- `media`: `create_media(config)` (`crates/platform/media/src/lib.rs:97`)

but each crate also exposes a second, non-contract constructor style:
`FileSystem::host`/`memory`/`from_config` (`lib.rs:92`, `:102`, `:111`) and
`Media::new`/`with_config` (`media/src/lib.rs:80`, `:85`).

Pick one canonical spelling per object (`create_*` per the contract) and make
the ergonomic constructors private or clearly secondary. Two public ways to
build the same service fragment the API surface and invites drift.

`Media::config()` returning `&MediaConfig` (`media/src/lib.rs:92`) is
inconsistent with the rest of the contract, where capability/config queries
are free functions (`capabilities(m: &Media)`); minor, but if `Media` is a
"stateless service", prefer `capabilities(&Media)`-style free functions
throughout, or methods throughout — not a mix.

### 1.2 Module layout — Low

`media` is cleanly split (`audio`, `image_decode`, `tlg`, `video`,
`yuv`), matching `05-media-engine.md §2`. `fs` is a single 931-line
`lib.rs` that mixes the public surface, both backend internals, the in-memory
store, and platform `#[cfg]` shims (`crates/platform/fs/src/lib.rs:1-931`). This is
the largest single file in the reviewed set and the least navigable. Split it
into `lib.rs` + `host.rs` + `memory.rs` (or `backend/{host,memory}.rs`) as the
media crate does. Not functional, but it will matter as the surface grows.

### 1.3 Error types — Medium/Low

Good: every domain error lives in one place, `crates/platform/api/src/error.rs`,
uses `thiserror`, and derives `Clone, Debug, PartialEq, Eq`. Errors carry
owned, backend-agnostic strings, matching `03-middle-layer-api.md §0.3(8)`.

Issues:

- **No `#[non_exhaustive]`** on `FsError` (`error.rs:12`), `MediaError` (`:67`),
  `RenderError` (`:113`), `GlyphError` (`:166`), `AudioError` (`:201`), nor on
  the public config enums/structs (`FsConfig` at `fs/src/lib.rs:63`,
  `MediaConfig` at `media/src/lib.rs:58`). Adding a variant/field is
  therefore a breaking change for every matcher. Add `#[non_exhaustive]`
  before the API freezes.
- **`MediaError::Decode.format` has two meanings.** In `image_decode.rs` it is
  the resolved format name (`"tlg"`, `format_name(...)`, `image_decode.rs:110`,
  `:224`); in `audio.rs` it is the *call-site hint/display name*
  (`name.to_string()` where `name = display_name(hint)`, i.e. `"<audio>"` or
  `"a.wav"` — `media/src/audio.rs:203`, `:246`). The same field cannot be
  both. Standardize on the detected codec/container name and put the hint in
  `message`.
- `FsError::Io` is overloaded for both OS failures and illegal-state calls:
  `read_at`/`write_at` return `Io` for "not opened for reading/writing"
  (`fs/src/lib.rs:503`, `:545`). `MediaError` has a proper `InvalidState`;
  `FsError` has none. Add an `InvalidState`/`InvalidArgument` variant and use
  it, so L2 can distinguish a bad call from a disk error.
- `backend_mismatch()` returns `Unsupported` (`fs/src/lib.rs:920`).
  A `FileState` from a different `FileSystem` is an argument/state error, not
  a backend capability error.

### 1.4 Doc-comment style and lints — Low

Doc style is consistent and thorough: `//!` module docs, `///` on items,
`#![warn(missing_docs)]` in all three lib roots, and `#` sections in the media
modules. Good.

However lint policy is copy-pasted per crate and only `warn`-level. The
workspace root has no `[workspace.lints]` table (`Cargo.toml:1-60`). Consider a
shared `[workspace.lints.rust]` / `[workspace.lints.clippy]` and
`lints.workspace = true` in each crate, at least
`missing_docs = "warn"`, `unsafe_code = "forbid"`. This also makes the "CI
greps krkr-core for forbidden constructs" acceptance check
(`02-target-architecture.md §9.3`) easy to pair with real lints.

### 1.5 Dependencies — Medium

Unused dependencies are declared in `Cargo.toml`:

- `crates/platform/fs/Cargo.toml:10` `thiserror` — never referenced in `src/`
  (errors are defined in `api`).
- `crates/platform/fs/Cargo.toml:11` `log` — never referenced in `src/`.
- `crates/platform/media/Cargo.toml:10` `thiserror` — never referenced in `src/`.
- `crates/platform/media/Cargo.toml:12` `flate2` — never referenced in `src/`
  (`grep flate2 crates/platform/media/src` returns nothing).

`media` also pulls `symphonia` twice in the tree: `media` requests
`0.6` (`Cargo.toml:13`) while `rodio` (via the legacy `tvp-sound`) still pulls
`symphonia 0.5.5` (`Cargo.lock:2109` and `:2124`). That is mostly a legacy-crate
problem, but the duplicate versions inflate build time/binary size; a
workspace-level pin (`workspace.dependencies`) would make the intent explicit.

### 1.6 Stateless object / explicit state (D9) uniformity — Low (design tension)

D9 compliance is generally strong:

- `Media` stores only `MediaConfig` and has no `&mut self` and no globals
  (`media/src/lib.rs:69-95`); decoder state is passed by `&mut` and owned
  by the caller.
- `FileSystem` host backend is pure configuration (`fs/src/lib.rs:60-171`);
  `FileState` holds handle + cursor (`:183-206`).
- The TLG Golomb/leading-zero tables are `const` compile-time tables, not
  `LazyLock` (`media/src/tlg.rs:834-889`) — exactly the D9 intent.

The one literal exception is the in-memory backend: `FileSystem` embeds
`Arc<Mutex<MemoryStore>>` (`fs/src/lib.rs:52-55`, `:81-84`, `:104`). This is
mutable state reachable from the object and shared across clones, so "the object
remembers nothing" is false for that backend, and `FileSystem` is `Sync` by
virtue of a lock. The design docs call it a "resource, not per-file state"
(`03-middle-layer-api.md §1`), and an in-memory root must store bytes somewhere,
so this is defensible — but it is the kind of exception the D9 guard
(`02-target-architecture.md §9.3`) would flag. Document it as an explicit,
sanctioned exception in the lint allowlist, or (cleaner) make the memory
backing store an explicit value the caller supplies to `FileSystem::memory`,
so the object again holds only a handle. Right now the exception is only hinted
at in code comments.

Separately, the contract promises `InputSource`, `HostWindow`, `Clock`,
`LogSink` (`03-middle-layer-api.md:423`, `:427`, `:442`, `:446`) but
`api/src/host.rs` defines only DTOs — no traits. That may be intentional
for this milestone; if so, note it, otherwise the host boundary is missing its
abstraction.

### 1.7 Parameter ordering — Nit

Ordering is consistent: the service object first, then state/inputs
(`read_at(fs, file, offset, buf)`, `decode_image(m, bytes, hint)`,
`yuv420_to_rgba(yuv, layout)`), matching the contract. The `let _ = m;`
lines (`media/src/lib.rs:118`, `image_decode.rs:122`, `:197`,
`audio.rs:283`, `:305`, `:356`, `:544`) are a smell that the `Media` receiver
is currently unused. Prefer naming it `_media: &Media` so it reads as
deliberate, or drop the receiver from the currently-stateless functions and add
it when a backend needs it.

### 1.8 Already good

- Error location centralization and the `error.rs` layout.
- The `DirEntry` sort + "no `.`/`..`" guarantees (`fs/src/lib.rs:270-343`).
- `KeyState`'s allocation-free bitmap and documented 256-code cap
  (`api/src/host.rs:14-104`).
- `PixelRegion` using saturating edge arithmetic (`geometry.rs:157-231`).
- `strip_sds`/`read_dims` using checked conversions and rejecting negatives
  (`tlg.rs:146-178`) — this is the pattern `decode_tlg5/6` should follow too.

---

## 2. Idiomatic Rust (API Guidelines / standard practice)

### 2.1 `unwrap` / `expect` / `panic!` / `unreachable!` — Low

No `unwrap()` on user data. The three occurrences are provably guarded, but
library code should still avoid the panic-shaped forms:

- `crates/platform/media/src/tlg.rs:176` — `try_into().unwrap()` on a slice that
  `bytes.get(off..off + 4)` already proved is 4 bytes. Fine logically, but the
  `.get(...).ok_or(...)?` immediately above can carry the exact-size guarantee:
  use `let b: [u8; 4] = bytes[off..off + 4].try_into().map_err(|_| "unexpected end of data")?;`
  or `.expect("4-byte slice")`.
- `crates/platform/fs/src/lib.rs:450` — `guard.files.remove(&src).expect("checked file exists")`.
  Replace with `if let Some(bytes) = guard.files.remove(&src)` to remove the
  panic entirely.
- `crates/platform/media/src/tlg.rs:572` — `unreachable!()` in `color_filter` is
  guarded by the `code >= 16` check at `:504`, so it is correct; a `debug_assert!`
  plus a defined fallback would be more defensive, but this is acceptable.

### 2.2 `as` casts that should be `From`/`TryFrom` — Medium

Widening casts that should be `u64::from`/`usize::from`:

- `geometry.rs:180` `(self.width as u64) * (self.height as u64)`
- `fs/src/lib.rs:237` `d.as_millis() as u64` (u128→u64 wrap after
  ~584 million years; harmless but sloppy)
- `fs/src/lib.rs:256`, `:587`, `:809` `v.len() as u64` → `u64::try_from`/`as` is fine but `u64::from` is available only for `usize` on 64-bit; prefer `u64::try_from(v.len()).unwrap_or(u64::MAX)`.
- `audio.rs:430` `(written / channels) as u64`, `:484` `channels().count() as u16`.

Narrowing/truncating casts that are actual bugs or need checking:

- `fs/src/lib.rs:516` `offset as usize` (read_at) — truncates on 32-bit.
- `fs/src/lib.rs:560` `offset as usize` (write_at) — truncates on 32-bit
  and drives `Vec::resize`, see §4.1.
- `fs/src/lib.rs:635` `target as u64` (seek) — `i128`→`u64` wraps; see §4.1.
- `media/src/tlg.rs:206` `read_i32(raw, 20)? as usize` (`blockheight`) —
  negative becomes `usize::MAX`; see §4.1.
- `media/src/tlg.rs:321` `read_i32(raw, pos)? as usize` (`ft_size`) —
  same pattern; see §4.1.
- `media/src/audio.rs:225` `channels().count() as u16` — a >65535-channel
  file truncates silently (unlikely but trivially checkable with `u16::try_from`).

`media/src/tlg.rs:148-152` (`read_dims`) already shows the right pattern
(`usize::try_from(width).map_err(...)`); the decode paths bypass it with raw
`as usize`. Reuse `read_dims` in `decode_tlg5`/`decode_tlg6`.

### 2.3 Unnecessary clones / allocations — Medium

- `crates/platform/fs/src/lib.rs:405` — `copy` clones the whole file payload
  (`guard.files.get(&src).cloned()`) then inserts it. Use
  `if let Some(bytes) = guard.files.remove(&src) { ... insert(dst, bytes) }`
  (check `src == dst` / `dst` dir first, then move). For a multi-MB copy this
  doubles peak memory and a full memcpy for no reason.
- `crates/platform/media/src/audio.rs:284` and `:312` — `probe_audio`/`decode_audio`
  do `Arc::from(bytes)`, which **copies the entire compressed buffer** into a
  new allocation, contradicting the module doc "the caller's buffer is shared
  by refcount so multi-MB entries are never copied" (`audio.rs:176-178`).
  `open_setup` only needs a `MediaSourceStream`; it can take `&[u8]` and build
  `Cursor::new(bytes)` directly (the `Arc` is only needed by the streaming
  state for reseeks).
- `crates/platform/media/src/image_decode.rs:47` — `hint.to_ascii_lowercase()`
  allocates a `String` per probe/decode call. Use `eq_ignore_ascii_case` /
  `ends_with` on a lowercased comparison, or at minimum `make_ascii_lowercase`
  on a stack `[u8; N]`.
- `crates/platform/media/src/image_decode.rs:57` and `audio.rs:150` — the identical
  `extension_of` helper allocates a `String` each call and is duplicated
  verbatim across modules. Move one copy to a shared private module and return
  `Option<&str>` where possible.
- `crates/platform/media/src/lib.rs:117-131` — `capabilities()` allocates three
  `Vec<String>` every call from `&'static [&'static str]` tables. Either return
  `Vec<&'static str>` (change the DTO) or have L2 cache the report; it is a
  cold call, but the allocation is pure waste.
- `crates/platform/media/src/audio.rs:165-167` — `is_opus` scans the *entire*
  buffer with `bytes.windows(8).any(...)`. Ogg `OpusHead` lives in the first
  Ogg page; scan only the first ~1 KiB (or the first page), both for speed and
  to avoid false positives on payload that happens to contain the string.
- `crates/platform/media/src/audio.rs:369` — `let name = display_name(...).to_string();`
  per `decode_next_packet` call allocates a `String` in the decode loop; only
  needed on the error path, so format lazily.

### 2.4 Params, derives, public-API breadth

- **No `&String`/`&Vec` parameters** anywhere — good. `&str`, `&[u8]`,
  `&[u32]`, `Option<&str>` are used consistently.
- `open_setup(bytes: &Arc<[u8]>)` (`audio.rs:183`) takes `&Arc`; taking the
  `Arc` by value or a `&[u8]` would be more idiomatic. Low.
- Derives are appropriate: `RectF`/`Affine2`/`Transform2D` correctly omit `Eq`
  (floats) while `PixelRegion`/`BlendMode`/`OpenMode` derive `Eq + Hash`;
  `GlyphBitmap`/`DecodedAudio` correctly omit `Eq`. `Media`/`MediaConfig`
  deriving `PartialEq, Eq` is unnecessary for a service object (Nit).
- `AudioDecoderState` has a manual `Debug` that hides codec internals and no
  `Clone` (`audio.rs:154-166`) — correct for a resource handle.
- Public API breadth: `media` exposes both `pub mod tlg;` and a root
  re-export, so `decode_tlg_with_info`, `has_tlg_magic`, and the whole TLG
  module are part of the public surface (`media/src/lib.rs:39-43`, `:45-48`).
  These are implementation details of the image dispatcher, not contract API.
  Make `tlg` `pub(crate)` and re-export only `decode_image`/`probe_image` (and
  `yuv` if L2/MovieCodec needs it). Medium — minimal API is a stated goal.

### 2.5 Iterator vs index loops — Nit

The index loops in `tlg.rs`/`yuv.rs` are justified (bit-exact port,
performance, non-linear indexing); converting them to iterators would hurt.
Everything else uses iterators (`KeyState::iter_down`, `MediaCapabilities`).
No action.

### 2.6 Good

- `#![forbid(unsafe_code)]` in all three roots.
- `const fn` geometry (`geometry.rs:31-96`, `:120-190`) is a nice touch.
- `yuv420_to_rgba` uses a bounds-checked `get()` so short buffers degrade to
  black instead of panicking (`yuv.rs:98-108`, `:186-188`).
- `AudioDecoderState`'s doc claim that it is `Send + Sync` is actually true:
  symphonia 0.6's `FormatReader` and `AudioDecoder` are both
  `Send + Sync` (`symphonia-core-0.6.1/src/formats/mod.rs:551`,
  `src/codecs/audio.rs:251`), so the L2 worker-pool plan holds.

---

## 3. Elegance / optimization opportunities (ranked)

1. **Bound `write_at`'s target length before resize** — `fs/src/lib.rs:560-568`.
   Both a correctness and a performance issue; see §4.1. Highest value because
   it turns a process abort into a typed `FsError`.
2. **Validate TLG5 `blockheight`** — `media/src/tlg.rs:206-214`. One
   comparison (`blockheight <= height`) removes an overflow/OOM class. See §4.1.
3. **Stop copying the whole file in probe/decode audio** — `audio.rs:284`,
   `audio.rs:312`, and make `open_setup` take `&[u8]`. Removes one full-buffer
   allocation + memcpy per whole-file decode, and fixes the doc contradiction.
4. **Check out the TLG `ft_size` conversion** — `tlg.rs:321-327`; use
   `usize::try_from` + `checked_add` for `pos + ft_size`.
5. **Avoid the `copy` payload clone** — `fs/src/lib.rs:405`; move instead
   of clone.
6. **Allocation-free extension/hint handling** — `image_decode.rs:47`,
   `:57`, `audio.rs:150`; dedupe the two `extension_of`s.
7. **Make `capabilities()` allocation-free / cached** — `media/src/lib.rs:117`.
8. **Fix `seek`'s `i128 -> u64` cast** — `fs/src/lib.rs:635`.
9. **Restrict `is_opus` to the Ogg header** — `audio.rs:165`.
10. **Use `read_dims` in `decode_tlg5/6`** instead of raw `as usize` —
    `tlg.rs:204-205`, `:307-308`.
11. **`Affine2::inverse` relative epsilon** — `geometry.rs:355`: comparing
    `det.abs() <= f32::EPSILON` is scale-dependent; a matrix with scale `1e-3`
    has `det = 1e-6` and is rejected as singular. Use a relative check tied to
    the matrix magnitude (or test `1/det` finiteness). Low but real.
12. **Narrow the public `tlg` module** — `media/src/lib.rs:41`.
13. **Split `fs/src/lib.rs`** into backend modules.

---

## 4. Correctness smells

### 4.1 Integer overflow / truncation

**Critical — `write_at` unbounded/overflowing allocation (in-memory backend)**
`crates/platform/fs/src/lib.rs:538-568`:

```rust
let start = offset as usize;
if start > bytes.len() { bytes.resize(start, 0); }
let end = start + buf.len();
if end > bytes.len() { bytes.resize(end, 0); }
bytes[start..end].copy_from_slice(buf);
```

- `offset` is a caller-controlled `u64`. On 64-bit, `offset = u64::MAX` makes
  `start = usize::MAX`; `bytes.resize(usize::MAX, 0)` panics with a capacity
  overflow (or aborts on allocation failure) from a public function.
- `start + buf.len()` can overflow (`:564`), then `bytes[start..end]` panics
  with a slice-index error in release mode.
- On 32-bit, `offset as usize` silently truncates, so `write_at(fs, f, 0x1_0000_0000, ...)`
  writes at offset 0.

The host backend is safe because `FileExt::write_at` takes `u64`. The in-memory
backend must match: `let start = usize::try_from(offset).map_err(...)?;` plus
`start.checked_add(buf.len())`, and reject offsets beyond a sane policy cap
(or return `FsError::Io`/`Unsupported` rather than trying to materialize an
enormous zero fill). Note this also means the memory backend cannot emulate a
sparse file the way the host backend can.

**High — TLG5 `blockheight` unchecked**
`crates/platform/media/src/tlg.rs:206-219`:

```rust
let blockheight = read_i32(raw, 20)? as usize;
if blockheight == 0 { return Err(...); }
let blockcount = (height - 1) / blockheight + 1;
...
let block_elems = blockheight * width;
```

`check_dims` (`:180-192`) validates width/height but not `blockheight`. A
negative `blockheight` becomes `usize::MAX`, making `blockheight * width`
overflow (panic in debug, wrap in release) and `vec![vec![0u8; block_elems]; colors]`
try to allocate an impossible buffer. Even a large-but-positive `blockheight`
(e.g. `1 << 30`) produces an outsized allocation. Fix: after `check_dims`,
require `blockheight != 0 && blockheight <= height` (then
`blockheight * width <= MAX_PIXEL_BYTES / 4` is already guaranteed).

**High — TLG6 `ft_size` unchecked**
`crates/platform/media/src/tlg.rs:321-327`: `read_i32(raw, pos)? as usize`
followed by `raw.get(pos..pos + ft_size)`. A negative `ft_size` becomes huge
and `pos + ft_size` overflows (panic), and a large positive one drives
`Vec::with_capacity(x_block_count * y_block_count)` at `:330` only after the
slice check; still, use `usize::try_from` and `checked_add`.

**Medium — `seek` wraps past `u64::MAX`**
`crates/platform/fs/src/lib.rs:624-636`: `target` is computed in `i128` and the
negative case is checked, but `target as u64` silently wraps if
`file_size + off` exceeds `u64::MAX` (e.g. `SeekFrom::End(i64::MAX)` on a large
file). Use `u64::try_from(target).map_err(...)`.

**Medium — `read_seq` cursor overflow** `fs/src/lib.rs:616`
`file.cursor += n as u64` can overflow only if the cursor was already wrapped
by `seek`; fix `seek` and this is moot.

**Low — `channel count as u16`** `audio.rs:225`, `:484`;
`(seconds * rate).round() as u64` `audio.rs:234` saturates silently (Rust
float→int casts saturate) — acceptable but worth an explicit bound.

### 4.2 Unbounded allocations from untrusted sizes

**High — image decode relies solely on `image`'s default cap; config flag is dead**
`crates/platform/media/src/image_decode.rs:210`, `:221` call
`image::load_from_memory_with_format`, and `:173` builds an `ImageReader`
without setting `Limits`. `MediaConfig::allow_large_images`
(`media/src/lib.rs:58-63`) is documented as "reserved / ignored", and it
is never read (`grep allow_large_images` finds only the declaration and the
test). The `image` crate 0.25 does apply a default `max_alloc` of 512 MiB with
no width/height limit (`image-0.25.10/src/io/limits.rs:49-55`), so this is not
literally unbounded — but for a game-asset decoder it is inconsistent with the
TLG decoder's explicit 256 MiB / 32768-px cap (`tlg.rs:51-52`) and the flag
promises behavior it does not deliver. Either wire `allow_large_images` through
to `ImageReader::limits(...)` (and `probe_image`'s `into_dimensions`), or
remove the dead field and state the real fixed cap. Also note `to_rgba8()`
(`image_decode.rs:104-109`) allocates a second buffer, so peak is up to ~2× the
decoder cap.

**Medium — `yuv420_to_rgba` allocation** `yuv.rs:135`
`vec![0u8; width*height*4]` is `saturating_mul`-guarded, so it cannot wrap, but
a malformed `Yuv420Layout` could still request a very large buffer. The layout
is produced internally from codec output, so this is lower risk than the image
path; consider a max-pixels check or returning `Result`.

### 4.3 Resource lifetimes — Good

- `FileState` owns its `File`/store handle; dropping it closes/releases, and
  the memory handle keeps the `Arc<Mutex<MemoryStore>>` alive (`fs/src/lib.rs:188-205`).
- `AudioDecoderState` owns the boxed format reader/decoder; drop releases the
  codec, matching `05-media-engine.md §6`.
- No `Arc` cycles, no callbacks held across the boundary.

### 4.4 Thread safety — Mostly good

- `FileSystem` is `Send + Sync` (host `File` and `Arc<Mutex<...>>` are), so it
  can be shared by the L2 worker pool.
- The lock discipline avoids re-entrancy: every helper that needs the store
  takes `&MemoryStore` rather than re-locking (`ensure_memory_parent`,
  `is_dir`, `parent_is` — `fs/src/lib.rs:205`, `:855`, `:876`). Good.
- `lock()` recovers from poisoning (`:927-931`), so one panicking reader does
  not brick the store. Note the trade-off: after a poison, a partially applied
  `write_at`/`rename` may be visible, because the `Vec`/`BTreeMap` operations
  are not transactional. Acceptable, but worth a comment.
- `MemoryStore` guards are held across large `.cloned()` copies
  (`copy`, `:405`) and whole-directory scans (`list`, `:319-343`); on a big
  store this serializes otherwise-independent operations. Moving instead of
  cloning helps here too.
- `AudioDecoderState` is `Send + Sync` only because symphonia's traits are;
  the doc says so and this is verified above. It is **not** safe to call
  `read_audio` concurrently on one state (needs `&mut`), which the type system
  enforces.

### 4.5 Other smells

- `exists()` maps *all* `stat` failures (including permission/IO) to `false`
  (`fs/src/lib.rs:268-270`). Documented, but L2 cannot distinguish
  "missing" from "denied"; consider `try_exists`-style `Result<bool>` if that
  matters.
- `resolve_host` passes absolute paths straight through, so L1 does not sandbox
  relative/absolute escapes (`fs/src/lib.rs:642-655`). Documented as L2's
  responsibility; flag it for the security review of the storage namespace.
- `io_error` only maps `NotFound`/`PermissionDenied`/`AlreadyExists`
  (`:901-918`); `ErrorKind::IsADirectory`/`NotADirectory` (stable since Rust
  1.83) fall through to generic `Io`, discarding the typed variants that exist.
- `micros_to_millis` truncates toward zero for negative values rather than
  flooring (`yuv.rs:180-182`); fine for non-negative PTS but worth a note if
  negative timestamps ever appear.
- `VideoDecoderState` is an empty `#[non_exhaustive]` struct
  (`video.rs:33`); correct as a scaffold, but `#[derive(Clone, PartialEq, Eq)]`
  on an uninhabited API type is noise — drop the derives until it has fields.

---

## 5. Top 10 recommended changes

1. **Fix `write_at` (memory backend) offset handling** — `crates/platform/fs/src/lib.rs:560-567`.
   `usize::try_from`, `checked_add`, and a size policy. Prevents a
   panic/abort and the 32-bit truncation write. *(Critical)*
2. **Validate TLG5 `blockheight`** — `crates/platform/media/src/tlg.rs:206-214`.
   Require `blockheight <= height`. *(High)*
3. **Check TLG6 `ft_size` with `usize::try_from` + `checked_add`**
   — `crates/platform/media/src/tlg.rs:321-327`. *(High)*
4. **Bound image decoding consistently and wire `allow_large_images`** —
   `crates/platform/media/src/image_decode.rs:173`, `:210`, `:221`; either
   implement the flag via `ImageReader::limits(...)` or delete it and declare
   the real cap. *(High)*
5. **Remove the whole-buffer copies in `probe_audio`/`decode_audio`** and make
   `open_setup` take `&[u8]` — `crates/platform/media/src/audio.rs:183`, `:284`,
   `:312`. *(Medium)*
6. **Use `u64::try_from` for `seek`'s target** — `crates/platform/fs/src/lib.rs:635`. *(Medium)*
7. **Remove unused dependencies** (`thiserror`, `log` in `fs`;
   `thiserror`, `flate2` in `media`) — `crates/platform/fs/Cargo.toml:10-11`,
   `crates/platform/media/Cargo.toml:10,12`. *(Medium)*
8. **Move instead of clone in `copy`** — `crates/platform/fs/src/lib.rs:405`. *(Medium)*
9. **Consistency pass:** `#[non_exhaustive]` on error/config enums, unify
   `MediaError::Decode.format` semantics, add `FsError::InvalidState`, and
   provide the promised host traits or note their deferral —
   `crates/platform/api/src/error.rs:12-232`, `crates/platform/fs/src/lib.rs:63`,
   `crates/platform/media/src/lib.rs:58`, `crates/platform/api/src/host.rs`. *(Medium)*
10. **Deduplicate and de-allocate the hint/extension helpers, shrink the public
    TLG surface, and split `fs/src/lib.rs`** —
    `crates/platform/media/src/image_decode.rs:47,57`, `crates/platform/media/src/audio.rs:150`,
    `crates/platform/media/src/lib.rs:41`, `crates/platform/fs/src/lib.rs`. *(Low–Medium)*
