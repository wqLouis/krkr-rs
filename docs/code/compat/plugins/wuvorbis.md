# wuvorbis (`wuvorbis.dll`)

**DLL name:** `wuvorbis.dll` — linked with `Plugins.link("wuvorbis.dll")`
**Rust module:** `crates/compat/src/natives/plugins/wuvorbis/`
**Reference:** no `reference/cpp` source — the reference is the shipped binary; the decoder's call pattern is documented in `reference/cpp/core/sound/VorbisWaveDecoder.cpp`
**Registers:** nothing — the `.ogg` decoder lives in the host audio chain

## Surface

| member | kind | behaviour |
|---|---|---|
| — | — | the plugin has no script-visible class or function |

`system/sound.tjs:4` does `Plugins.link("wuvorbis.dll")` so the sound system
can open `.ogg` storages. The reference plugin registers **no** TJS class or
function:

* it decodes `.ogg` through the bundled libvorbisfile — the `wu_ov_*` exports
  are the `ov_*` API renamed, the same call pattern as the engine's in-tree
  `reference/cpp/core/sound/VorbisWaveDecoder.cpp` (`ov_open_callbacks` →
  `ov_read`, whose old-encoder warning still logs under the `wuvorbis:`
  prefix);
* it attaches to the win32 sound system as a TSS sound module
  (`GetModuleInstance` → `ITSSModule::GetSupportExts` /
  `GetMediaInstance`, declared in `reference/cpp/core/sound/win32/tvpsnd.h`
  and consumed by `TVPSearchAvailTSSWaveDecoder`,
  `reference/cpp/core/sound/WaveImpl.cpp:279`);
* `Query_sizeof_OggVorbis_File` and the `wu_*` symbols are that C decoder API,
  not script surface: the `OggVorbis` / `wuvorbis` wide strings found in the
  DLL are the module's metadata (`docs.old/plugin_host_exe.md` §2.4, §5.3), and
  nothing in `reference/` exposes an `OggVorbis` TJS type.

## Behaviour

`Plugins.link("wuvorbis.dll")` resolves to the built-in table and succeeds,
so the game is not routed to the null plugin object.

The compat layer never links a codec: it exposes the host decoder seam
`add_audio_decoder`, and the composition root (`crates/app/src/audio.rs`)
fills it with the L1 `media` decoders. The reference's decoder *creator*
maps to that seam, so `.ogg` decoding is the host's job and the plugin entry
itself installs nothing.

The result is that a `WaveSoundBuffer` opened over an `.ogg` storage decodes
through the host chain, exactly as on the reference, and the game sees the
same samples.

## State and lifetime

No state inside the plugin: the decoder lives in the host chain the
composition root installs before any script runs.

## Errors

| condition | outcome |
|---|---|
| the host decoder chain is not installed | typed error — `WaveSoundBuffer.open` fails instead of returning silence |
| the `.ogg` data is corrupt or truncated | the decoder's typed error (`api::error::MediaError`), never silence |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/wuvorbis.rs` | the link resolves to the built-in table and is idempotent (no `wuvorbis` null global, no TJS class); a `WaveSoundBuffer` over the committed 0.2 s / 44.1 kHz fixture decodes through the host chain installed the way the composition root installs it; running without the host decoder is a typed error; a corrupt `.ogg` reports the decoder's typed error |
| `crates/platform/media/tests/audio.rs` | the same fixture probes and decodes with the `.ogg` name, the bare extension and no hints; a stream decodes frame-identically to the whole-file decode; a truncated stream is a typed `MediaError` |
| `crates/compat/tests/plugin.rs` | that `Plugins.link("wuvorbis.dll")` resolves to the built-in table |
| `crates/compat/tests/null_plugin.rs` | that the link does not install a null object |

## Known gaps

* The reference's decoder is a separate DLL with its own libvorbisfile build.
  The port uses the host's L1 `media` decoder, so the exact decoder version
  differs from a game's bundled build. The decoded PCM for a well-formed
  stream is the same; an encoder-specific quirk of the reference build would
  not be reproduced.
* The reference attaches a TSS sound module, which is a Windows-only sound
  backend concept. There is no script-visible counterpart.
