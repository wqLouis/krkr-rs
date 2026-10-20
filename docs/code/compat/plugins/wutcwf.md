# wutcwf (`wutcwf.dll`)

**DLL name:** `wutcwf.dll` — linked with `Plugins.link("wutcwf.dll")`
**Rust module:** `crates/compat/src/natives/plugins/wutcwf/`
**Reference:** `reference/cpp/plugins/wutcwf.cpp`
**Registers:** nothing — it installs a `TCWF` wave decoder on the audio seam

## Surface

| member | kind | behaviour |
|---|---|---|
| — | — | the plugin has no script-visible class or function |

The reference registers a `tTVPWaveDecoderCreator` with the wave system; the
creator opens a storage whose bytes start with the `TCWF0\x1a` mark and
decodes the stream with a combined MS-ADPCM / IMA-ADPCM codec. It registers
**no** TJS class or function.

## Behaviour

`Plugins.link("wutcwf.dll")` resolves to the built-in table and succeeds, so
a game that links the DLL is not routed to the null plugin object.

### Where the decoder lives

The compat layer never links a codec: it exposes the host decoder seam
`add_audio_decoder`, and the composition root (`crates/app/src/audio.rs`)
composes the L1 `media` decoders behind it. The reference's decoder
*creator* maps to that seam: `register` installs `decode_tcwf` as the sound
decoder, so `WaveSoundBuffer.open` decodes a `TCWF0\x1a` stream exactly like
the reference's creator does. `is_tcwf` is exposed so a future composite
host decoder can dispatch on the signature before falling back to the L1
codecs.

### The decode

The decode is a byte-for-byte port of the reference `TCWFDecoder`:

* the 24-byte `TTCWFHeader`;
* per channel, a 32-byte `TTCWBlockHeader` (`ms_sample0`, `ms_sample1`,
  `ms_idelta`, `ms_bpred`, `ima_stepindex` and six unexpected-peak
  corrections);
* the MS-ADPCM low nibble of every byte followed by the IMA-ADPCM high
  nibble of the same byte, as the reference's two `for(k = 2, p = 0; …)`
  loops;
* the six unexpected-peak corrections applied last.

The MS-ADPCM tables (`AdaptationTable`, `AdaptCoeff1`, `AdaptCoeff2`) and the
IMA-ADPCM tables (`ima_index_adjust`, `ima_step_size`) are the reference's
values.

No ADPCM crate covers this stream: the reference packs the MS-ADPCM low
nibble and the IMA-ADPCM high nibble of the *same* byte (two codecs
interleaved in one stream) inside a custom 32-byte block header with
per-channel peak corrections, so the codec stays a faithful port rather than
a crate call.

The reference emits signed 16-bit samples; `DecodedAudio` stores interleaved
`f32`, so every sample is scaled by `1 / 32768`.

## State and lifetime

The plugin holds no state. It installs one decoder closure in the per-engine
audio decoder seam; the closure is stateless and takes the bytes, the loop
info and the name hint.

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

The seam reports the missing signature before the codec runs, so a non-TCWF
stream is rejected with the signature message rather than a header error.

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/misc_plugins.rs` | that a `TCWF` fixture decodes through `WaveSoundBuffer.open`, and that a non-`TCWF` stream reports the typed signature error |
| `crates/compat/tests/plugin.rs` | that `Plugins.link("wutcwf.dll")` resolves to the built-in table |

## Known gaps

* The reference registers a wave **decoder creator** with the sound system;
  the port installs a decoder on the compat audio seam, which the
  composition root fills with the L1 `media` decoders. A game that links the
  DLL and then opens a `.wutc` storage gets the decoded samples either way.
* The reference's codec is the one shipped in the game's DLL build. The port
  reimplements it from the reference source, so a stream that a specific game
  build decodes differently would be decoded the reference's way here.
