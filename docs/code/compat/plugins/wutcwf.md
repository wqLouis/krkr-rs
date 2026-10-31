# wutcwf (`wutcwf.dll`)

**DLL name:** `wutcwf.dll` — linked with `Plugins.link("wutcwf.dll")`
**Rust module:** `crates/compat/src/natives/plugins/wutcwf/`
**Reference:** `reference/cpp/plugins/wutcwf.cpp`
**Registers:** nothing — it installs a `TCWF` wave decoder on the audio seam

The DLL name is in the built-in table and the linking rules are on the
[plugin index](../plugins.md). The other decoder-only plugin is
[wuvorbis](wuvorbis.md), which fills the same chain from the host side.

## Surface

| member | kind | behaviour |
|---|---|---|
| — | — | the plugin has no script-visible class or function |

The reference registers a `tTVPWaveDecoderCreator` with the wave system
(`reference/cpp/plugins/wutcwf.cpp:359-362`); the creator opens a storage
whose bytes start with the `TCWF0\x1a` mark and decodes the stream with a
combined MS-ADPCM / IMA-ADPCM codec. It registers **no** TJS class or
function, so a script sees the decoder only through
`WaveSoundBuffer.open`.

## Behaviour

`wutcwf.dll` is a name in the built-in table
(`crates/compat/src/natives/plugins/mod.rs`), so `Plugins.link("wutcwf.dll")`
resolves to the built-in and succeeds; the DLL is not absent.

### Where the decoder lives

The compat layer never links a codec: it exposes the host decoder seam
`add_audio_decoder` (`crates/compat/src/natives/sound.rs`), and the
composition root (`crates/app/src/audio.rs`) appends the L1 `media` decoders
to it. The reference's decoder *creator* maps to that seam: `register`
installs `decode_tcwf` as the sound decoder, so `WaveSoundBuffer.open`
decodes a `TCWF0\x1a` stream exactly like the reference's creator does.

The seam is a **chain**, not a single slot: `add_audio_decoder` appends, and
`open` tries every installed decoder in order and takes the first that
succeeds. The host's `media` decoder is installed at startup
(`crates/app/src/game.rs:584` → `crates/app/src/audio.rs:315`), so it is the
first link in the chain; this plugin appends the TCWF decoder when the script
links `wutcwf.dll` (`xp3filter.dll` is the only startup module). A `.ogg`
storage is handled by the host decoder, and a `TCWF0\x1a` stream falls through
it — the host decoder cannot read that format — to the TCWF one.

### The decode

The decode is a byte-for-byte port of the reference `TCWFDecoder`:

* the 24-byte `TTCWFHeader` (`reference/cpp/plugins/wutcwf.cpp:13-21`) with
  the six-byte mark check (`:196-201`);
* per channel, a 32-byte `TTCWBlockHeader` (`:26-35`) holding
  `ms_sample0`, `ms_sample1`, `ms_idelta`, `ms_bpred`, `ima_stepindex` and
  six unexpected-peak corrections;
* the MS-ADPCM low nibble of every byte (`:270-293`), followed by the
  IMA-ADPCM high nibble of the *same* byte (`:299-337`) — the reference's
  two `for(k = 2, p = 0; …)` loops;
* the six unexpected-peak corrections applied last (`:340-353`).

The MS-ADPCM tables (`AdaptationTable`, `AdaptCoeff1`, `AdaptCoeff2`) and the
IMA-ADPCM tables (`ima_index_adjust`, `ima_step_size`) are the reference's
values (`reference/cpp/plugins/wutcwf.cpp:38-62`).

No ADPCM crate covers this stream: the reference packs the MS-ADPCM low
nibble and the IMA-ADPCM high nibble of the *same* byte (two codecs
interleaved in one stream) inside a custom 32-byte block header with
per-channel peak corrections, so the codec stays a faithful port rather than
a crate call.

The reference emits signed 16-bit samples; the decoded audio stores
interleaved `f32`, so every sample is scaled by `1 / 32768`. The stream's
sample rate, channel count and frame count come from the header.

`is_tcwf` is a public helper in the module next to the codec: the installed
closure calls it to reject a stream that does not carry the mark before the
codec runs, and it is available to a composite host decoder that wants to
dispatch on the signature.

## State and lifetime

The plugin holds no state of its own. It appends one stateless closure to
the per-engine decoder chain; the closure takes the bytes, the loop info
parsed from the `.sli` side-car and the storage-name hint, and returns a
`SoundSource` that owns the decoded samples.

## Errors

Every failure is a typed error — the codec never truncates a stream silently
and never produces silence:

| condition | error |
|---|---|
| the stream does not start with the `TCWF0\x1a` mark | `not a TCWF stream: missing the TCWF0\x1a mark` |
| the fixed header is truncated | `truncated TCWF header: {found} of {needed} bytes` |
| the stream has zero channels | `TCWF stream has zero channels` |
| the sample rate is not positive | `TCWF stream has an invalid sample rate {n} Hz` |
| the block count is not positive | `TCWF stream has an invalid block count {n}` |
| the block layout cannot describe a block | `TCWF stream has an invalid block layout: …` |
| a block runs past the end of the stream | `truncated TCWF block {n} channel {n}: offset {n}` |
| a block's MS-ADPCM predictor is out of range | `TCWF block {n} channel {n} has an invalid predictor {n}` |
| a block's IMA-ADPCM step index is out of range | `TCWF block {n} channel {n} has an invalid step index {n}` |
| a peak correction points past the block's samples | `TCWF block {n} channel {n} peak position {n} is out of range` |
| the header describes more samples than the host can address | `TCWF stream describes too many samples for this host` |

The seam reports the missing signature before the codec runs, so a non-TCWF
stream is rejected with the signature message rather than a header error.
When no decoder in the chain accepts the bytes, `open` reports **all** of
their failures joined in chain order, naming the storage
(`WaveSoundBuffer.open: cannot decode "…": …`).

Two of these are cases the reference stumbles over instead: a step index
past the 89-entry `ima_step_size` table would be indexed out of bounds
(`reference/cpp/plugins/wutcwf.cpp:302`), and a block that runs past the
stream is a short read the reference turns into a failed `ReadBlock`
(`:248-254`).

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/misc_plugins.rs` | that a `TCWF` fixture decodes through `WaveSoundBuffer.open` (frequency, channels, format and the loaded-but-stopped status), that a non-`TCWF` stream reports the typed signature error, and that the reference's class names (`Wutcwf`, `TCWFDecoder`) are not registered as globals |
| `crates/compat/tests/sound.rs` | that the plugin's decoder is on the chain after `host.install` and that `open` reports the aggregated typed failure naming the storage when nothing accepts the bytes |
| `crates/compat/tests/wuvorbis.rs` | that the TCWF decoder is the *first* chain link and that a non-TCWF storage walks past it to the host decoder |

## Known gaps

* The reference registers a wave **decoder creator** that the sound system
  consults when it opens a storage; the port installs a decoder on the
  compat audio chain, which the composition root fills with the L1 `media`
  decoders. A game that links the DLL and then opens a TCWF storage gets the
  decoded samples either way.
* The reference's decoder renders block by block and supports `SetPosition`
  (`reference/cpp/plugins/wutcwf.cpp:154-187`); the port decodes the whole
  stream when the storage is opened and materialises it, so a position
  change moves over the already-decoded PCM instead of re-seeking the byte
  stream. The decoded samples are the same.
* The reference's codec is the one shipped in a game's own DLL build. The
  port reimplements it from the reference source, so a stream that a
  specific game build decodes differently would be decoded the reference's
  way here.
