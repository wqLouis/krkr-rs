//! krkr-rs bottom media engine.
//!
//! Stateless `Media` object, pure `decode_image`/`probe_*` functions, and
//! explicit `AudioDecoderState`/`VideoDecoderState` values owned by the
//! middle layer. Backends: FFmpeg (Linux) and MediaCodec (Android). See
//! `docs/migration/05-media-engine.md`.
