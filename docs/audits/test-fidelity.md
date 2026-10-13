# Test-fidelity audit

Two bugs survived the last wave because a test encoded the implementation's
assumption rather than the reference's behaviour:

* `crates/compat/tests/text.rs`'s `fefe_mode0`/`fefe_mode1` built the `FE FE`
  header as five bytes, then the decoder read the mode 0/1 payload at offset 4
  (`reference/cpp/core/base/TextStream.cpp:87-88`). The vendored `+4` is a
  reconstruction error (AGENTS §9.9): the shipped `tTVPTextReadStream` requires
  the `FF FE` BOM after the mode and starts the payload at offset 5, so the
  five-byte fixture header was right and the offset-4 decoder was wrong.
  **Resolved:** the decoder now reads offset 5 with a required BOM
  (`crates/compat/src/text.rs` `decode_container`) and the fixtures keep the
  five-byte `FE FE <mode> FF FE` header (see `docs/vm-defects.md` TS-1).
* `crates/compat/tests/xp3_filter.rs` attached L2 storage before
  `CompatHost::install`, while the composition root registered the plugin
  first, so the filter was inert in production and green in the suite
  (`docs/audits/xp3-filter.md`). **Covered by a new regression test**
  (`crates/compat/tests/xp3_filter.rs:646-710`).

This audit looks for the *same class* elsewhere. It covers the working tree at
the time of writing. It does not run the gate (AGENTS §0); every finding is
read from source and reference.

**Method.** For category 1 I enumerated every test helper that constructs
bytes whose layout the reference defines and asked whether the bytes come from
the reference's documented layout or from our reader's assumptions (a
round-trip through our own writer/reader is the highest-risk shape). For
category 2 I looked for behaviour assertions about *values* with no
`reference/cpp/...:line` nearby. For category 3 I compared each harness's
install/attach/mount order with `crates/app/src/game.rs` and the reference
startup. For category 4 I looked for assertions a wrong or stubbed path also
satisfies.

**What I could not decide** (marked NEEDS EVIDENCE below): whether a real
PSB/PSD/TLG artefact actually differs from our synthetic fixtures; whether the
`Mouse`/`Key`/`Pad` reference exists anywhere in the vendored tree (it does
not — `reference/cpp/core/msg/` contains only `CMakeLists.txt`); and whether
`testkit`'s TLG encoders are byte-identical to `SaveTLG5.cpp`/`SaveTLG6.cpp`
without diffing the Golomb "give-up" path end to end.

---

## Ranked findings

| # | Consequence | Finding | Location |
|---|---|---|---|
| F1 | Critical | No real KiriKiri-native file is decoded by the default suite; every native format is tested only via our own encoder | suite-wide |
| F2 | High | TLG5/TLG6 round-trips pin the format to our encoder | `platform/media/tests/tlg.rs` |
| F3 | High | SDS test compares two of our own decoder outputs | `platform/media/tests/tlg.rs:28` |
| F4 | High | `build_xp3` is hand-built, duplicated 4×, never checked against a reference archive; compressed index is untested | `testkit`, 3 compat copies |
| F5 | High | PSB name table is produced by a synthetic trie encoder | `compat/tests/psb_file.rs:92` |
| F6 | Medium-High | PSD slice/grid fixture is hand-built with almost no citations | `compat/tests/psd_file.rs:180` |
| F7 | Medium-High | `.tft` tests assert the port's lazy-read byte count, not reference behaviour | `compat/tests/font.rs:412,438` |
| F8 | Medium | `font_metrics_without_a_font_source` accepts either `Ok` or `Err`; it cannot fail | `compat/tests/font.rs:228` |
| F9 | Medium | The only real-game image test asserts dimensions, not pixels | `app/tests/real_game.rs:63` |
| F10 | Medium | Shared compat `Env` never runs `begin_native_timing` | `compat/tests/common/mod.rs:65` |
| F11 | Medium | Zero-citation compat value assertions (input, storages, scripts, storages_io, image_function) | several |
| F12 | Low | JSON writer bytes pinned against a stub reference (disclosed in-file) | `compat/tests/json.rs` |
| F13 | Low | `discover_archives` ordering is a D35 extension with no reference (correctly so) | `engine/tests/storage.rs:64` |

---

## A. Fixtures that construct reference-format data (category 1)

### F1 — Critical: no real native-format artefact is decoded anywhere

The only committed reference-format fixture in the tree is an OGG Vorbis file
(`crates/platform/media/tests/assets/sine_440hz_0.2s.ogg`, used by
`platform/media/tests/audio.rs:12` and `compat/tests/wuvorbis.rs:50`), and it is
an `ffmpeg` encode, not a KiriKiri artefact. There is **no** committed `.xp3`,
`.tlg`, `.tft`, `.psb`, `.psd` or `FE FE` container. Every test of those
formats feeds a fixture built in-process.

*Reference.* The on-disk layouts are defined by
`reference/cpp/core/visual/LoadTLG.cpp:457-481`,
`reference/cpp/core/visual/SaveTLG6.cpp:1024-1033`,
`reference/cpp/core/archive/xp3/XP3Archive.cpp:340-489`,
`reference/cpp/core/visual/PrerenderedFont.cpp:43-58`,
`reference/cpp/plugins/psbfile/PSBFile.cpp:328-527` and
`reference/cpp/plugins/psdfile/psdclass.cpp:220-253`.

*Assumption.* That a fixture built from our reading of those files is
equivalent to a file the reference (or a game) actually produced.

*Consequence.* Exactly the `FE FE` failure: encoder and decoder can share one
misreading and every test stays green while a real game fails. This is the
umbrella finding; F2-F7 are its instances.

*Fix.* Commit one real file per native format (a TLG5, a TLG6, an XP3 with a
compressed index, a `.tft`, a `.psb`, a `.psd`) and add decode-only tests that
assert exact pixels/values. A single small XP3 and one TLG from any KiriKiri
title would close most of it.

### F2 — High: TLG5/TLG6 round-trips pin the format to our encoder

`crates/platform/media/tests/tlg.rs:52-55` (`tlg6_rgba_roundtrip_all_filters`),
`:63-69` (RGB), `:123-129` (`tlg5_rgba_roundtrip`) and
`crates/platform/media/tests/image.rs:71` (`encode_image_round_trips_supported_formats`)
all do `encode_tlgN(...)` → `decode_image(...)` → `assert_eq!(decoded.rgba, img)`.

*Assumption.* Losslessness of our encoder/decoder pair. The encoder is a
test-only re-implementation of the reference (`crates/testkit/src/media_fixtures.rs:344`
`encode_tlg6`, `:495` `encode_tlg5`) with reference citations for the Golomb
table, colour filters and channel order. It is accurate as far as it goes, but
the decoder is also ours: a misreading shared by both — a block order, a filter
application order, a channel plane order — is invisible.

*Reference.* `reference/cpp/core/visual/LoadTLG.cpp:243-267` (bit-length
table), `:457-460` (magics); `SaveTLG5.cpp`/`SaveTLG6.cpp` are the writer
oracle.

*Consequence.* A whole class of real TLG backgrounds decodes to wrong pixels or
fails; the suite is green.

*Fix.* Decode a committed real TLG5 and TLG6 and assert the exact RGBA bytes.
Keep the round-trips as a secondary consistency check; they are not fidelity
evidence.

### F3 — High: the SDS test compares two of our own decoder outputs

`crates/platform/media/tests/tlg.rs:28-50`
(`sds_wrapper_and_raw_payload_are_equivalent`) wraps a `encode_tlg6` payload in
`sds` magic + length and asserts `img_wrapped.rgba == img_raw.rgba`.

*Assumption.* That two outputs of the same decoder agreeing proves the wrapper
is parsed. Both sides go through `decode_image`, so a decoder that ignores the
wrapper header entirely and returns the same wrong image for both also passes.

*Reference.* `reference/cpp/core/visual/LoadTLG.cpp:481-521` (the `TLG0.0 sds`
chunk walk: `tags` and the embedded TLG), `:614-638`.

*Consequence.* A broken SDS wrapper path is invisible; real games ship SDS/TLG0
payloads.

*Fix.* Assert `img_wrapped.rgba` against the known `test_image` pattern (not
against `img_raw`), and add a committed real SDS file.

### F4 — High: `build_xp3` is hand-built, duplicated four times, and one production path is untested

`crates/testkit/src/lib.rs:38` documents itself as *"Layout matches what
`fs::xp3::Xp3Archive` expects"* — that is a self-consistency claim, not a
reference claim. The same builder is copy-pasted into
`crates/compat/tests/xp3_filter.rs:27`,
`crates/compat/tests/fstat.rs:34` and
`crates/compat/tests/layer_ex_behaviour.rs:258` (the last two say "local copy
of `testkit::build_xp3`").

*What the reference actually says.* The fixture's bytes are reference-shaped:
index offset at byte 11 (`XP3Archive.cpp:340-343`), one flag byte then an
`i64` size (`:392-399`), `File`/`info`/`segm`/`adlr` chunks
(`:406-489`), a 28-byte segment record (`:451-476`), and the `info` body as
`flags:u32 org:i64 arc:i64 namelen:i16 name:u16[]` (`:424-436`). So it is not
obviously wrong. The finding is that **no test ever parses an archive the
reference produced**, and four copies can silently drift apart.

*Consequence.* An index feature the fixture never builds is untested: the
compressed index (`TVP_XP3_INDEX_ENCODE_ZLIB`, `:360-390`, and the
`TVP_XP3_INDEX_CONTINUE` multi-index chain at `:500-505`), the protected-storage
flag (`TVP_XP3_FILE_PROTECTED`, `:425-427`), and the `adlr` hash (`:483-489`) —
though the hash is only stored, never validated. A fix applied to one copy of
the builder does not reach the other three test binaries.

*Fix.* Keep one builder (`testkit`), delete the three copies, and cite the
reference chunk layout in its doc comment. Add a committed reference archive
(or a checked-in hex dump of one index block) and a test that parses it; add a
compressed-index fixture — nothing in the tree builds that path.

### F5 — High: PSB name tables are produced by a synthetic trie encoder

`crates/compat/tests/psb_file.rs:92` (`encode_names`) builds a name trie by
"reserving a 256-wide window" per node, then `:236` (`build_fixture`) puts it
in a 40-byte v2 header. The decoder (`decode_names`) walks that chain.

*Assumption.* That our synthetic trie is representative of a real PSB's name
table. Real `.psb` files are produced by external tooling, not by the
reference engine, so the only way to know the decoder handles them is a real
file. The test round-trips our own encoder through our own decoder.

*Reference.* The reader is `reference/cpp/plugins/psbfile/PSBFile.cpp:328-527`
and `PSBHeader.h:54-85`; the test cites those for the *chunk* layout but the
name-trie producer is ours.

*Consequence.* PSB drives e-mote/motion characters in many titles; a decoder
that only understands our trie fails a real `.psb` and the suite stays green.

*Fix.* **NEEDS EVIDENCE** — commit a real `.psb` and decode its names. Keep the
synthetic fixture for value-shape coverage but stop treating it as format
evidence.

### F6 — Medium-High: PSD fixture is hand-built with almost no citations

`crates/compat/tests/psd_file.rs:180` (`build_fixture`) assembles a full PSD
(header, image resources 1032/1050, layer records, RLE channels, merged
composite) from the Adobe layout, and the file has 2 `reference/cpp`
citations for 6 tests. `crates/compat/tests/psd_behaviour.rs:217`
(`layered_psd`) is the same shape but is well cited.

*Assumption.* That our hand-built PSD exercises the same record shapes the
reference plugin parses. The reference is the oracle, not Adobe.

*Reference.* `reference/cpp/plugins/psdfile/psdclass.cpp:220-253` (`getLayerInfo`),
`:168-190` (name/`checkLayerNo`), `psdparse/psdparse.h:215-219` (record layout).

*Consequence.* The slice/grid resource assertions
(`psd_file.rs:374` onward) depend on a fixture whose resource block layout is
uncited; a real PSD with a differently ordered slice descriptor could parse
wrongly.

*Fix.* Commit a real layered PSD and assert `getLayerInfo` fields and slices
against it, citing `psdclass.cpp:220-253`.

### F7 — Medium-High: `.tft` tests pin the port's lazy-read byte count

`crates/compat/tests/font.rs:412-431`
(`font_prerendered_map_reads_only_the_header`) asserts
`storage.read_bytes() == 24 * 36` after mapping 24 fonts. `:438` +
`font_prerendered_remap_reads_nothing` asserts `read_bytes() == 36` then `0`.

*Assumption.* That the port's header-only read is the behaviour to pin. The
reference reads the **whole** file at map time
(`reference/cpp/core/visual/PrerenderedFont.cpp:9-76`) and decompresses glyphs
from it in `Retrieve` (`:120-170`). The lazy read is a port optimization, not
reference behaviour.

*Consequence.* These tests institutionalise an implementation detail and never
check that the deferred glyph body is actually loadable; a lazy reader that
never retrieves glyphs would still pass. A correct change to the read strategy
breaks the tests.

*Reference.* `PrerenderedFont.cpp:9-76`, `:120-170`.

*Fix.* Assert behaviour: after `mapPrerenderedFont`, the mapped glyph renders
(or the character's `Inc`/metrics come from the `.tft`). If the lazy read is
deliberately kept, record it as a D36 approximation with its reference gap and
drop the exact byte count. The `build_tft` fixture itself
(`crates/compat/tests/font.rs:56`) matches the reference struct
(`PrerenderedFont.h:7-18`, 20-byte record) and the header
(`PrerenderedFont.cpp:43-58`) — keep it.

---

## B. Value assertions without a reference citation (category 2)

These are compat (L3/VM) tests, where D30 applies. I did not list every
uncited test; these are the ones where being wrong would be invisible.

### F11a — `compat/tests/input.rs` (0 citations / 10 tests)

`input.rs:56-130` asserts concrete values — `Mouse.getCursorX()==300`,
`getWheelRotY()==120`, `getClickCount(0)==1`, `getPressed(0)==1`,
`getRepeat(0)==1`, `getReleased(0)` lasting one frame. The vendored reference
does not contain the `Mouse`/`Key`/`Pad` native (`reference/cpp/core/msg/`
has only `CMakeLists.txt`; `DInputMgn.cpp`/`tvpinputdefs.h` cover raw input,
not the script surface).

*Is.* No reference governs the script values in-tree — that is the finding.
*Could be* a real divergence (wheel-Y vs wheel, click-count reset timing,
repeat semantics) that the suite cannot detect.

*Fix.* **NEEDS EVIDENCE** — either add the missing reference file(s) or mark
the input script surface as unverified per D36.

### F11b — `compat/tests/storages.rs:56` `string_helpers_match_reference`

Asserts `extractStorageName`, `extractStorageExt`, `extractStoragePath`,
`chopStorageExt`, `getFullPath` values with no citation. The reference is
`reference/cpp/core/base/StorageIntf.cpp`. Low blast radius, but the test's
name claims reference fidelity it cannot show.

*Fix.* Add the `reference/cpp/core/base/StorageIntf.cpp:...` citations.

### F11c — `compat/tests/scripts.rs` (0 citations / 10 tests), `compat/tests/storages_io.rs` (0 / 3), `compat/tests/image_function.rs:66`

`scripts.rs` asserts `Scripts` member values (trace-string path rewriting,
`setCallMissing`, `textEncoding`) with no citations.
`image_function.rs:66` only checks each member is `is_ok()` — a null-plugin
stub would satisfy it (the surface lock would not, because these are built-in
members, but the assertion itself is weak).
`storages_io.rs` exports/imports through `Storages` with no citation.

*Fix.* Add citations; for `image_function`'s probe, also assert each member's
documented return/raise rather than mere presence.

### F12 — Low: JSON writer bytes against a stub reference

`crates/compat/tests/json.rs:1-25` honestly documents that the vendored
reference is a stub and the exact writer byte format is unverifiable in-tree,
then `:517`/`:655` assert round-trips and exact output. This is the D36
disclosure done right; the residual risk is that the exact bytes are pinned to
our implementation. Keep, but treat the writer format as an approximation in
the subsystem status.

---

## C. Lifecycle / ordering assumptions (category 3)

### F10 — Medium: the shared compat `Env` never runs `begin_native_timing`

`crates/compat/tests/common/mod.rs:65-113` attaches scene/timer/input/storage
and calls `host.install(&engine)`, then stops. The app's composition root
continues: after install and the codec seams it calls
`vm.compat().begin_native_timing()`
(`crates/app/src/game.rs:400`), which is what runs the deferred,
service-dependent half of plugin registration
(`crates/compat/src/natives/plugins/mod.rs:206-219`, reading
`xp3filter.tjs` and installing the extraction/content seam).

*Consequence.* Every test that uses the shared `Env` exercises a lifecycle
that stops one call short of the app. Any *other* deferred plugin work is
silently inactive in most of the suite, and a test that needs it must roll its
own harness (as `xp3_filter.rs:646-710` now does). This is the same shape that
hid the XP3 filter bug, just one call later.

*Fix.* Call `host.begin_native_timing()` at the end of `Env::build` (and keep
the custom harness in `xp3_filter.rs` for the explicit install-then-attach
ordering), so the default harness matches the composition root.

*What is already consistent:* the shared `Env` attaches storage **before**
`install`, matching the current app (`game.rs:376-383`), and the new
`xp3_filter` regression test covers the worst-case install-then-attach order
with `begin_native_timing`. Audio is opened before mounting in the app
(`game.rs:341`) and the L1 audio tests open it directly; I found no test that
asserts the opposite order.

---

## D. Tests a wrong or stubbed implementation could also pass (category 4)

### F8 — Medium: `font_metrics_without_a_font_source_do_not_fabricate_a_metric`

`crates/compat/tests/font.rs:228-250` matches `Ok(Integer(width)) if width>0`
**or** any `Err` with a non-empty message. Either branch passes, so the test
cannot fail; it documents an ambiguity rather than asserting a behaviour.

*Reference.* A host with no matching face raises rather than fabricating a
metric (D11; the reference font path raises on a missing face). The host's CJK
fallback, if present, resolves.

*Fix.* Make the host deterministic (e.g. point the font resolver at an empty
directory and set no system-font dirs) and assert the reference outcome; if the
fallback is genuinely host-dependent, split into two tests with explicit
setups.

### F9 — Medium: the only real-game image test checks dimensions, not content

`crates/app/tests/real_game.rs:63-84` decodes up to 8 real `.tlg`/`.png`
images and asserts only `image.width > 0`, `image.height > 0` and
`rgba.len() == w*h*4`. A decoder returning a uniformly blank image of the
right size passes. `:44-56` likewise asserts only that `startup.tjs` is
non-empty.

*Consequence.* This is the suite's one real-artefact check and it cannot catch
a wrong TLG decode — the exact failure mode F2 cannot catch synthetically.

*Fix.* Assert non-trivial content: at least one image with non-uniform pixels,
or compare one committed game asset against a stored checksum/sample pixels
once a reference file is committed (F1).

### Also worth noting

* `crates/compat/tests/image_function.rs:66` — `is_ok()` presence checks
  (covered under F11c).
* `crates/platform/media/tests/tlg.rs:152-192` (`garbage_inputs_return_err`) asserts
  only `is_err()`; a decoder that rejected *all* input would pass. The
  round-trips (F2) are the only counterweight, and they are self-referential.
* The wide OGG frame window (`platform/media/tests/audio.rs:16`,
  `8_600..=9_100` for ~8_820 expected frames) is tolerable for a lossy
  third-party encoder, but it is a tolerance a sample-count bug could hide in.

---

## E. Positive controls (verified as faithful)

* `crates/compat/tests/text.rs:123-147` builds the correct five-byte
  `FE FE <mode> FF FE` header and cites `TextStream.cpp:82-105`; the offset-5
  BOM-checking reader and the wide code-unit script path are pinned in
  `crates/compat/src/text.rs` and `crates/tjs/src/lib.rs` — the known bug is
  closed (see `docs/vm-defects.md` TS-1).
* `crates/compat/tests/sound.rs:51-56` builds `.sli` text matching the
  reference grammar exactly (`#2.00`, case-insensitive `Label`,
  single-quoted values), verified against
  `reference/cpp/core/sound/WaveLoopManager.cpp:1287-1370` (`strncmp` version
  check, `strncasecmp`/`GetEntityToken`).
* `crates/compat/tests/psb_file.rs` and `psd_behaviour.rs` cite the reference
  at the assertion level; the residual risk is the synthetic producer, not the
  citations.
* `crates/compat/tests/xp3_filter.rs:646-710` adds a regression test for the
  exact install-then-attach ordering that hid the filter bug.

---

## F. Cheapest high-value fixes

1. **Decode a real TLG5/TLG6 in the default suite.** One small committed file
   plus one `decode_image` → exact-RGBA assertion closes the largest share of
   F1/F2/F3. This is the same lever that would have caught the `FE FE` offset
   bug that only the device logs exposed.
2. **Commit one real XP3 and parse it with `Xp3Archive`.** Add a
   compressed-index fixture (nothing builds `TVP_XP3_INDEX_ENCODE_ZLIB`).
3. **Single `build_xp3`.** Delete the three compat copies; cite
   `XP3Archive.cpp:340-489` in the one that remains. Removes the
   drift-by-copying hazard at near-zero cost.
4. **Fix the two `.tft` byte-count assertions** (`font.rs:412,438`) to assert
   behaviour, and record the lazy read as a D36 approximation if kept.
5. **Add `begin_native_timing()` to the shared compat `Env`** so the harness
   matches `game.rs:400`.
6. **Make `font_metrics_without_a_font_source` decisive.**
7. **Strengthen `real_game.rs`** to assert non-uniform pixel content, not just
   dimensions.
8. **Add citations** to `input.rs` (or mark unverified), `storages.rs:56`,
   `scripts.rs`, `storages_io.rs`, `image_function.rs:66`.

An honest short version: the suite is strong on citations and on behaviour
*once a fixture exists*, but weak on **provenance** — almost every native
format is validated against bytes we generated. The single highest-value change
is to commit real reference artefacts and decode them.

---

## Resolution (working tree)

A follow-up pass fixed the cheapest high-value findings. Citations added here
are `reference/cpp/...:line` (D30); where the reference is absent the
assertion is marked unverified and the evidence that would settle it is named
(D36). No assertion value was changed for the F11 items.

### Fixed

* **F10 — harness lifecycle.** `crates/compat/tests/common/mod.rs` now calls
  `CompatHost::begin_native_timing()` after `install`, matching the app's order
  (`crates/app/src/game.rs`: attach scene/storage → install → codec/font seams
  → tpm autoload → `begin_native_timing` → startup scripts) and running the
  deferred `xp3filter` activation. The custom `xp3_filter.rs` harness is
  unchanged (peer-owned). No existing test was expected to break; if one does,
  it is because it relied on the deferred plugin work staying inactive and
  must say so rather than have the call removed.
* **F8 — `font_metrics_without_a_font_source_do_not_fabricate_a_metric` can
  fail.** The error branch now requires the resolver's exact no-face message
  instead of any non-empty string; the success branch still requires a
  positive width.
  * Before: `Err(message) => assert!(!message.is_empty())`.
  * After: `Err(message) => assert!(message.contains("no font is available for face 'Ghost'"))`.
  The two-outcome disjunction remains because host font availability is
  genuinely host-dependent (`resolve_face` tries the registry, storage,
  system fonts, the well-known Noto CJK paths, then the engine's first face).
* **F9 — real-game image test checks content.** `crates/app/tests/real_game.rs`
  now requires at least one decoded image whose pixels are not all equal to the
  first, so a correctly sized blank decode fails. It proves spatially varying
  content, not reference-exact pixels; the latter needs a committed reference
  asset (F1).
* **F4 — one builder, compressed index.** `testkit::build_xp3` is the single
  builder, documented against the reference reader
  (`XP3Archive.cpp:340-343,392-399,424-436,451-476,483-489`; the claim was
  checked against the reader, not repeated). The compat copies in `fstat.rs`
  and `layer_ex_behaviour.rs` are deleted and call the shared builder;
  `compat` gains a `testkit` dev-dependency. A new
  `build_xp3_compressed_index` writes a `TVP_XP3_INDEX_ENCODE_ZLIB` block
  (`XP3Archive.cpp:360-390`) and `crates/platform/fs/tests/xp3.rs` parses it
  and reads both a raw and a zlib segment out of it. The peer-owned
  `xp3_filter.rs` copy is untouched.
* **F7 — `.tft` byte counts replaced by behaviour.** The two tests that
  asserted `storage.read_bytes()` now map fonts through a real face and assert
  that each mapped `.tft` measures its own `Inc` (24 fonts) and that a reused
  `.tft` serves two mappings. The lazy read is a port detail, not a parity
  claim, and is no longer pinned.
* **F11 — citations / unverified marking.** `storages.rs` cites
  `StorageIntf.cpp:775-805,809-833,837-859,863-887,1281-1292`;
  `scripts.rs` cites `ScriptMgnIntf.cpp:1314-1549`, `TextStream.cpp:418-430`,
  `plugins/windowEx.cpp:1542-1547` and `tjs2/tjsVariant.h:694-699`;
  `storages_io.rs` cites `plugins/dirlist.cpp:13-60,25-27,82` and
  `plugins/xp3filter.cpp:407-416`; `image_function.rs` cites
  `core/visual/ImageFunction.cpp:188-308,310-406,408-529,531-569,571-609,611-683,685-745,747-785,787-859,861-965,967-1078,1080-1179`.
  `input.rs` carries an explicit unverified note because
  `reference/cpp/core/msg/` contains only `CMakeLists.txt`; the missing input
  native source (or a device capture) is what would settle those values.

### Remains (needs real artefacts or a device)

* **F1/F2/F3/F5/F6** — no real KiriKiri TLG5/TLG6/SDS/PSB/PSD/XP3 artefact is
  committed; native formats are still validated against our own encoders.
  Needs one real file per format from a shipping title.
* **F11a** — `input.rs` stays unverified until the input native reference
  source exists or a device captures the real `Mouse`/`Key`/`Pad` values.
* **`image_function.rs:66`** — the member probe is still an `is_ok()` presence
  check; the surface lock, not this test, pins registration.
* **`font.rs` truncated `.tft`** — the audit suggested asserting a truncated
  file raises "where the reference raises". The reference reads the whole file
  at map time but only validates the header (`PrerenderedFont.cpp:22-58`) and
  reads the body unchecked, so a truncated body is undefined there; the port's
  first-use bounds error is a safety improvement, not a parity claim, and is
  deliberately not asserted as reference behaviour.
* **F12** — the JSON writer bytes remain pinned to our implementation
  (disclosed in-file).

`Cargo.lock` needs a `testkit` entry under `compat`; the first `cargo`
invocation of the gate updates it.
