//! `wuvorbis.dll` plugin port (OGG Vorbis).
//!
//! `system/sound.tjs:4` does `Plugins.link("wuvorbis.dll")` so the sound
//! system can open `.ogg` storages. The reference plugin registers **no**
//! TJS class or function:
//!
//! * it decodes `.ogg` through the bundled libvorbisfile — the `wu_ov_*`
//!   exports are the `ov_*` API renamed, the same call pattern as the
//!   engine's in-tree `reference/cpp/core/sound/VorbisWaveDecoder.cpp`
//!   (`ov_open_callbacks` → `ov_read`, whose old-encoder warning still logs
//!   under the `wuvorbis:` prefix);
//! * it attaches to the win32 sound system as a TSS sound module
//!   (`GetModuleInstance` → `ITSSModule::GetSupportExts`/`GetMediaInstance`,
//!   declared in `reference/cpp/core/sound/win32/tvpsnd.h` and consumed by
//!   `TVPSearchAvailTSSWaveDecoder`, `WaveImpl.cpp:279`);
//! * `Query_sizeof_OggVorbis_File` and the `wu_*` symbols are that C decoder
//!   API, not script surface: the `OggVorbis` / `wuvorbis` wide strings found
//!   in the DLL are the module's metadata (`docs.old/plugin_host_exe.md` §2.4,
//!   §5.3), and nothing in `reference/` exposes an `OggVorbis` TJS type.
//!
//! # Where the decoder lives in krkr-rs
//!
//! L3 never links a codec: `compat` exposes the host decoder seam
//! [`add_audio_decoder`](crate::add_audio_decoder), and the L4 composition
//! root fills it with the L1 `media` decoder (symphonia ogg/vorbis, see
//! `crates/app/src/audio.rs`). The reference's decoder *creator* maps to
//! that seam: [`register`] installs nothing of its own because `.ogg`
//! decoding is the host's job — the plugin entry exists so
//! `Plugins.link("wuvorbis.dll")` resolves to the built-in table.
//!
//! # Proven, not asserted
//!
//! * `crates/compat/tests/wuvorbis.rs` — the link resolves builtin and is
//!   idempotent (no `wuvorbis` global, no TJS class); a
//!   `WaveSoundBuffer` opened over the committed 0.2 s / 44.1 kHz fixture
//!   decodes through the host chain installed exactly the way the
//!   composition root does; running without the host decoder, or with a
//!   corrupt `.ogg`, is a typed error, never silence.
//! * `crates/platform/media/tests/audio.rs` — the same fixture probes and decodes
//!   with `.ogg` name/bare-extension/no hints, streams frame-identically to
//!   the whole-file decode, and a truncated stream is a typed
//!   [`MediaError`](api::error::MediaError).

use crate::{CompatCtx, CompatError};

/// Register the plugin: no TJS surface, no decoder of its own.
///
/// The reference plugin registers nothing on the VM (see the module docs);
/// its `.ogg` wave-decoder creator maps to the host decoder chain the
/// composition root installs before any script runs. `Plugins.link`
/// activates this entry so the link succeeds as a built-in.
pub fn register(_ctx: &mut CompatCtx) -> Result<(), CompatError> {
    Ok(())
}
