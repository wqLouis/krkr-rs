//! Font loading: TTF/OTF and TTC/OTC font collections via `ab_glyph`, with
//! system font discovery via `fontdb`.
//!
//! We deliberately do **not** write a font parser or rasterizer: `ab_glyph`
//! (which wraps `ttf-parser`) parses the binary and rasterizes outlines.
//! `fontdb` finds fonts on the system and, crucially, can tell us the *family
//! name* of each face inside a `.ttc` collection — which `ab_glyph` alone
//! cannot (its `Font` trait does not expose the name table).

use std::fmt;
use std::path::Path;

use ab_glyph::{Font, FontVec, GlyphId, InvalidFont, PxScale, PxScaleFont, ScaleFont};
use fontdb::{Database, Query};

/// Errors produced while loading a font face.
#[derive(Debug)]
pub enum FontError {
    /// The font file could not be read from disk.
    Io(std::io::Error),
    /// The bytes are not a valid font, or the collection index is out of range.
    Invalid(InvalidFont),
}

impl fmt::Display for FontError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FontError::Io(e) => write!(f, "cannot read font file: {e}"),
            FontError::Invalid(e) => write!(f, "invalid font data (or bad collection index): {e}"),
        }
    }
}

impl std::error::Error for FontError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FontError::Io(e) => Some(e),
            FontError::Invalid(e) => Some(e),
        }
    }
}

impl From<std::io::Error> for FontError {
    fn from(e: std::io::Error) -> Self {
        FontError::Io(e)
    }
}

impl From<InvalidFont> for FontError {
    fn from(e: InvalidFont) -> Self {
        FontError::Invalid(e)
    }
}

/// A loaded font face: a single face selected from a TTF/OTF file or from a
/// TTC/OTC font collection.
///
/// The font bytes are owned (`ab_glyph::FontVec`), so a face can be moved
/// freely and shared across threads. `collection_index` records which face was
/// selected when the source was a collection (always 0 for plain fonts).
///
/// # Note on `ab_glyph` scaling
///
/// `ab_glyph` normalizes metrics by the font's *vertical extent*
/// (`ascent − descent`), not by `units_per_em`. For Noto Sans CJK
/// (ascent 1160 / descent 288 / upem 1000) a naive `PxScale::from(48.0)`
/// therefore renders a 33 px em. [`px_scale_for_height`] rescales so that
/// `pixel_height` means a full `units_per_em` em square in pixels — the
/// convention CJK fonts use for full-width glyph boxes.
pub struct FontFace {
    font: FontVec,
    collection_index: u32,
    family: Option<String>,
}

/// Preferred Japanese-capable CJK sans face on this machine.
pub(crate) const NOTO_SANS_CJK_JP: &str = "Noto Sans CJK JP";

/// Known paths for Noto CJK collections, used as a fallback when `fontdb`
/// cannot find the family on the system.
const KNOWN_JP_PATHS: &[&str] = &[
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/local/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
];

/// Convert a requested pixel height into an `ab_glyph` scale.
///
/// `ab_glyph` treats `PxScale.y` as the font's line height (`ascent −
/// descent`). We rescale so that `pixel_height` maps to `units_per_em`
/// pixels, i.e. a full-width CJK glyph is exactly `pixel_height` px wide and
/// the em square is exactly `pixel_height` px tall.
pub(crate) fn px_scale_for_height(font: &FontVec, pixel_height: f32) -> PxScale {
    debug_assert!(pixel_height > 0.0, "pixel height must be positive");
    let upem = font.units_per_em().unwrap_or(1000.0);
    let extent = font.height_unscaled(); // ascent − descent, positive
    let extent = if extent > 0.0 { extent } else { upem };
    PxScale::from(pixel_height * extent / upem)
}

impl FontFace {
    /// Load a face from a file.
    ///
    /// `collection_index` selects the face inside a TTC/OTC collection; pass
    /// `0` for plain TTF/OTF files. Use [`FontFace::find_jp_face_index`] or
    /// [`FontFace::discover_system_jp`] to locate the face for a given script.
    pub fn from_path(path: impl AsRef<Path>, collection_index: u32) -> Result<Self, FontError> {
        let bytes = std::fs::read(path)?;
        Self::from_memory_indexed(bytes, collection_index)
    }

    /// Load the first face (index 0) of a font from raw bytes.
    pub fn from_memory(bytes: Vec<u8>) -> Result<Self, FontError> {
        Self::from_memory_indexed(bytes, 0)
    }

    /// Load a face from raw bytes with an explicit collection index.
    pub fn from_memory_indexed(bytes: Vec<u8>, collection_index: u32) -> Result<Self, FontError> {
        let font = FontVec::try_from_vec_and_index(bytes.clone(), collection_index)?;
        let family = family_name_of(&bytes, collection_index);
        Ok(Self {
            font,
            collection_index,
            family,
        })
    }

    /// Index of this face inside its source file (0 for plain fonts).
    pub fn collection_index(&self) -> u32 {
        self.collection_index
    }

    /// The face's primary (English) family name, when it could be parsed.
    pub fn family_name(&self) -> Option<&str> {
        self.family.as_deref()
    }

    /// Underlying `ab_glyph` face (crate-internal; `ab_glyph` types are not
    /// part of the public API of this crate).
    pub(crate) fn font(&self) -> &FontVec {
        &self.font
    }

    /// Discover the system's Japanese CJK sans face ("Noto Sans CJK JP").
    ///
    /// Strategy:
    /// 1. Ask `fontdb` (system font database) for "Noto Sans CJK JP" — this
    ///    works on any OS where the family is installed under that name.
    /// 2. Fall back to well-known Noto CJK collection paths, scanning the
    ///    faces (via [`FontFace::find_jp_face_index`]) for the one whose
    ///    family name contains "JP".
    pub fn discover_system_jp() -> Option<Self> {
        let mut db = Database::new();
        db.load_system_fonts();
        let query = Query {
            families: &[fontdb::Family::Name(NOTO_SANS_CJK_JP)],
            weight: fontdb::Weight::NORMAL,
            stretch: fontdb::Stretch::Normal,
            style: fontdb::Style::Normal,
        };
        if let Some(id) = db.query(&query)
            && let Some((source, index)) = db.face_source(id)
            && let Some(bytes) = source_to_bytes(&source)
            && let Ok(face) = Self::from_memory_indexed(bytes, index)
        {
            return Some(face);
        }

        for path in KNOWN_JP_PATHS {
            let Ok(bytes) = std::fs::read(path) else {
                continue;
            };
            let Some(index) = Self::find_jp_face_index(&bytes) else {
                continue;
            };
            if let Ok(face) = Self::from_memory_indexed(bytes, index) {
                return Some(face);
            }
        }
        None
    }

    /// Scan a font collection for the index of the first face whose family
    /// name contains "JP" (e.g. "Noto Sans CJK JP" inside the shared
    /// NotoSansCJK-Regular.ttc).
    ///
    /// `ab_glyph` cannot expose family names, so this parses the name table
    /// through `fontdb`. `None` is returned when no face matches (or when the
    /// data is not a font).
    pub fn find_jp_face_index(data: &[u8]) -> Option<u32> {
        let mut db = Database::new();
        db.load_font_data(data.to_vec());
        db.faces().find_map(|face| {
            face.families
                .iter()
                .any(|(name, _)| name.contains("JP"))
                .then_some(face.index)
        })
    }
}

impl fmt::Debug for FontFace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FontFace")
            .field("collection_index", &self.collection_index)
            .field("family", &self.family)
            .finish_non_exhaustive()
    }
}

/// Resolve the glyph id for `c`, applying the missing-glyph fallback:
/// `ttf-parser` reports a missing glyph as id 0 (`.notdef`); for any char
/// except U+0000 we prefer the font's U+FFFD replacement glyph when it has
/// one, otherwise we keep `.notdef` itself (which every font must provide).
///
/// Shared by the atlas (rendering) and [`crate::measure_width`] so hit-testing
/// uses the same advances as rendering.
pub(crate) fn glyph_id_with_fallback(c: char, scaled: &PxScaleFont<&FontVec>) -> GlyphId {
    let id = scaled.glyph_id(c);
    if id == GlyphId(0) && c != '\0' {
        let replacement = scaled.glyph_id('\u{FFFD}');
        if replacement != GlyphId(0) {
            return replacement;
        }
    }
    id
}

/// Primary family name of the face at `index` inside `data`, if parseable.
fn family_name_of(data: &[u8], index: u32) -> Option<String> {
    let mut db = Database::new();
    db.load_font_data(data.to_vec());
    db.faces()
        .find(|face| face.index == index)
        .and_then(|face| face.families.first().map(|(name, _)| name.clone()))
}

/// Read the raw bytes of a `fontdb` source (either in-memory binary or a file
/// on disk).
///
/// `tvp-text` pins `fontdb` to `default-features = false, features =
/// ["std", "fs"]`, so `Source` has exactly the two variants matched here (no
/// `memmap`); enabling `fontdb/memmap` would make the compiler flag this
/// match, which is the intended signal.
fn source_to_bytes(source: &fontdb::Source) -> Option<Vec<u8>> {
    match source {
        fontdb::Source::Binary(data) => Some(data.as_ref().as_ref().to_vec()),
        fontdb::Source::File(path) => std::fs::read(path).ok(),
    }
}
