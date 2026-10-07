//! Audio decode scaffolds.
//!
//! This module defines the final `Media` audio surface — the stateless pure
//! functions and the bottom-defined [`AudioDecoderState`] value owned by the
//! middle layer — but **no codec is wired up yet**. The symphonia/FFmpeg
//! backends arrive in a later task. Every action returns
//! [`MediaError::Unsupported`] so the middle layer can be compiled and
//! type-checked against the real signatures today.
//!
//! Nothing here stores state: [`AudioDecoderState`] is created by
//! [`open_audio`] and returned to the caller, who owns and passes it back to
//! [`read_audio`]/[`seek_audio`]. `Media` itself remains immutable.

use std::sync::Arc;

use krkr_api::error::MediaError;
use krkr_api::media::{AudioSpec, DecodedAudio};

use crate::Media;

/// Bottom-defined state for an incremental audio decode.
///
/// The middle layer owns this value and passes it back on every call; `Media`
/// never stores it. It is opaque on purpose: only this crate constructs or
/// mutates it, so the later symphonia/FFmpeg implementation can change its
/// layout without an API break.
///
/// The scaffold is uninhabited from outside the crate (external code cannot
/// construct it because it is `#[non_exhaustive]` and has no fields).
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDecoderState {}

/// Probes an audio stream for its [spec](AudioSpec) without decoding all
/// samples.
///
/// `hint` is the storage name or extension the caller used to find `bytes`;
/// it only steers backend selection and is never trusted over the content.
///
/// Scaffold: always returns [`MediaError::Unsupported`].
pub fn probe_audio(m: &Media, bytes: &[u8], hint: Option<&str>) -> Result<AudioSpec, MediaError> {
    let _ = (m, bytes, hint);
    Err(crate::unsupported("audio probe"))
}

/// Decodes a whole audio file into interleaved `f32` samples.
///
/// Scaffold: always returns [`MediaError::Unsupported`].
pub fn decode_audio(
    m: &Media,
    bytes: &[u8],
    hint: Option<&str>,
) -> Result<DecodedAudio, MediaError> {
    let _ = (m, bytes, hint);
    Err(crate::unsupported("audio decode"))
}

/// Opens an audio stream for incremental decoding.
///
/// On success returns the stream's [`AudioSpec`] plus a fresh
/// [`AudioDecoderState`] for [`read_audio`]/[`seek_audio`]. `bytes` is shared
/// by `Arc` so the caller can keep the compressed buffer alive.
///
/// Scaffold: always returns [`MediaError::Unsupported`].
pub fn open_audio(
    m: &Media,
    bytes: Arc<[u8]>,
    hint: Option<&str>,
) -> Result<(AudioSpec, AudioDecoderState), MediaError> {
    let _ = (m, bytes, hint);
    Err(crate::unsupported("streaming audio"))
}

/// Decodes the next chunk of interleaved `f32` samples into `out`.
///
/// Returns the number of samples written. [`MediaError::EndOfStream`] marks
/// the end of the stream.
///
/// Scaffold: always returns [`MediaError::Unsupported`].
pub fn read_audio(
    m: &Media,
    state: &mut AudioDecoderState,
    out: &mut [f32],
) -> Result<usize, MediaError> {
    let _ = (m, state, out);
    Err(crate::unsupported("streaming audio"))
}

/// Seeks the stream so the next [`read_audio`] starts at `frame`.
///
/// Scaffold: always returns [`MediaError::Unsupported`].
pub fn seek_audio(m: &Media, state: &mut AudioDecoderState, frame: u64) -> Result<(), MediaError> {
    let _ = (m, state, frame);
    Err(crate::unsupported("streaming audio"))
}
