//! krkr-rs stateless PSB and PSD format decoding.
//!
//! Two container families live here, both as plain functions over byte slices
//! with caller-owned state:
//!
//! * [`container`] (with its `decompress` helper) reads an Emote PSB/MDF
//!   document: the header, the name and string tables, the typed value tree
//!   and the raw resource chunks. Streams may be plain, zlib-wrapped (`MDF`)
//!   or an Emote LZ4 frame; the frame walk accepts exactly the streams the
//!   engine's loader accepts.
//! * [`parse`] (with its `descriptor` helper) and [`decode`] read a Photoshop
//!   PSD document: header, colour mode data, image resources, the layer/mask
//!   section, and the merged composite, then expand each channel (raw,
//!   PackBits or zip) into straight-alpha RGBA.
//!
//! # Shape of the API
//!
//! Every entry point is a free function that takes the bytes and returns the
//! parsed model or a typed error from [`error`]. Nothing is cached, nothing is
//! registered and no value outlives the call, so a caller can decode any
//! number of documents concurrently.
//!
//! # Cost model
//!
//! The metadata walk is a single serial cursor pass. Pixel work is the only
//! part worth spreading: [`decode`] expands a document's 8-bit channels on a
//! short-lived fork-join pool when the decode is large enough to pay for it,
//! and merges colour planes with runtime-dispatched SIMD. Both paths produce
//! the same bytes as the scalar path, and the thresholds only choose a
//! schedule — never the output.
//!
//! # Safety lint
//!
//! The crate is pure parsing and pixel math: `unsafe_code` is forbidden.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod container;
pub mod decode;
pub(crate) mod decompress;
pub(crate) mod descriptor;
pub mod error;
mod parallel;
pub mod parse;
mod simd;
