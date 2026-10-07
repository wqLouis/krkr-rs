//! krkr-rs middle-layer API.
//!
//! This crate defines the contract between the middle layer (`engine`)
//! and the bottom core (`fs`, `media`, `render`):
//!
//! * **stateless abstraction objects** created by the bottom layer
//!   (`FileSystem`, `RenderPipeline`, `Media`, `FontSystem`, `AudioDevice`);
//! * **explicit state values** written by the bottom layer and owned/stored
//!   by the middle layer (`RenderScene`, `RenderState`, `FileState`,
//!   `AudioDecoderState`, `VideoDecoderState`, …);
//! * plain DTOs (`BlendMode`, `DecodedImage`, `AudioSpec`, `RgbaFrame`, …).
//!
//! No engine state lives here. See `docs/migration/03-middle-layer-api.md`
//! for the full design.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod blend;
pub mod error;
pub mod fs;
pub mod geometry;
pub mod glyph;
pub mod host;
pub mod input;
pub mod media;
