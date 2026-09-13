//! tvp-text — text rendering core for krkr-rs.
//!
//! Pure-CPU glyph rasterization + CJK text layout, the foundation for KAG
//! message text (milestone 3B). No Bevy, no TJS2 natives; integration with
//! the scene ([`FontState`]) and the KAG natives happens in a later step.
//!
//! # Crates
//!
//! - [`atlas`] / [`GlyphAtlas`]: rasterizes glyphs for a face at a pixel
//!   height into a packed RGBA grid, caching slots per `char`.
//! - [`font`] / [`FontFace`]: loads TTF/OTF and TTC/OTC collections via
//!   `ab_glyph` (the font *parser/rasterizer* is deliberately reused, not
//!   reimplemented), with `fontdb`-based system discovery and TTC face
//!   selection.
//! - [`layout`]: line breaking, wrapping and alignment; returns placed glyphs
//!   with atlas UVs plus the total block height for layer sizing.
//! - [`measure`]: pixel width of a string for hit-testing / centering.
//!
//! # Conventions
//!
//! - Text arrives as Rust `String` (UTF-8) — decoding was done by the stream
//!   layer (`tvp-util`'s `Encoding`), this crate works on `&str`/`char` only.
//! - Coordinates are **y-down** pixels, origin at the top-left of the text
//!   block; see [`layout`] for the full convention list.
//! - `pixel_height` means the em size in px (KAG `FontState.height`): a
//!   full-width CJK glyph is `height × height` px. [`FontFace`] documents the
//!   `ab_glyph` rescaling that makes this true.
//! - Atlas pixels are straight white RGBA with coverage alpha, ready to be
//!   tinted by the text color downstream.
//!
//! # Example
//!
//! ```no_run
//! # use tvp_text::*;
//! // Load a face; see `FontFace::discover_system_jp` for automatic discovery.
//! let face = FontFace::from_path("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 0)
//!     .expect("font file");
//! let mut atlas = GlyphAtlas::with_default_width(face, 32);
//! let result = layout(
//!     "日本語テスト",
//!     320.0,
//!     32.0,
//!     &mut atlas,
//!     &LayoutOptions { wrap: true, ..Default::default() },
//! );
//! assert_eq!(result.runs.len(), 1);
//! assert!(result.total_height > 0.0);
//! ```
//!
//! [`FontState`]: https://docs.rs/tvp-visual/latest/tvp_visual/scene/struct.FontState.html

pub mod atlas;
pub mod font;
pub mod font_config;
pub mod layout;
pub mod measure;
pub mod prerendered;

pub use atlas::{GlyphAtlas, GlyphSlot, with_cached_atlas, with_cached_atlas_styled};
pub use font::{FaceRequest, FontError, FontFace, font_config, resolve_face, set_font_config};
pub use font_config::{FontConfig, FontConfigError, FontEntry};
pub use layout::{Align, GlyphRun, LayoutOptions, PlacedGlyph, TextLayout, layout};
pub use measure::measure_width;
pub use prerendered::{
    PrerenderedFont, PrerenderedFontError, PrerenderedGlyph, PrerenderedKey,
    clear_prerendered_fonts, map_prerendered_font, prerendered_font,
};
