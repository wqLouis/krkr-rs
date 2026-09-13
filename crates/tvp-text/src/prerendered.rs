//! TVP **pre-rendered fonts** (`.tft`).
//!
//! Games built with the original TVP authoring tools can ship per-character
//! bitmap fonts and map a `(face, height, bold, italic, angle)` combination to
//! one via `Font.mapPrerenderedFont(file)`. At draw time the reference engine
//! looks the mapped font up by those properties and copies each character's
//! pre-rendered coverage bitmap instead of rasterizing an outline
//! (`reference/cpp/core/visual/PrerenderedFont.cpp`,
//! `LayerBitmapImpl.cpp`).
//!
//! # File layout (little-endian)
//!
//! ```text
//! offset 0   : "TVP pre-rendered font\x1a"   (22 bytes)
//! offset 22  : version (0 or 1)
//! offset 23  : 2 (16-bit Unicode)
//! offset 24  : u32 index count
//! offset 28  : u32 absolute offset of the sorted u16 character index
//! offset 32  : u32 absolute offset of the character-item array
//! ```
//!
//! Each character item is 20 packed bytes: `u32 data_offset`, `u16 width`,
//! `u16 height`, `i16 origin_x`, `i16 origin_y`, `i16 inc_x`, `i16 inc_y`,
//! `i16 inc`, `u16 reserved`. The bitmap at `data_offset` is RLE-compressed
//! row-major coverage (0..64), which [`PrerenderedFont::rasterize`] expands and
//! upscales to 0..255 exactly like the reference `TVPUpscale65_255`.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

/// The 22-byte `.tft` magic (including the trailing `0x1a`).
const MAGIC: &[u8; 22] = b"TVP pre-rendered font\x1a";
/// Fixed header size: magic + version + unicode flag + 3 × u32.
const HEADER_LEN: usize = 36;
/// Packed `tTVPPrerenderedCharacterItem` size.
const ITEM_LEN: usize = 20;

/// Errors produced while parsing a `.tft`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrerenderedFontError {
    /// The data is shorter than the fixed header.
    TooShort,
    /// The file did not start with the `.tft` magic.
    BadMagic,
    /// The version byte is not one this port understands.
    BadVersion(u8),
    /// The file is not a 16-bit Unicode pre-rendered font.
    NotUnicode,
    /// The declared index offsets/sizes fall outside the file.
    BadOffset,
}

impl fmt::Display for PrerenderedFontError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PrerenderedFontError::TooShort => write!(f, "prerendered font is truncated"),
            PrerenderedFontError::BadMagic => write!(f, "not a TVP pre-rendered font"),
            PrerenderedFontError::BadVersion(v) => {
                write!(f, "unsupported prerendered font version {v}")
            }
            PrerenderedFontError::NotUnicode => {
                write!(f, "prerendered font is not 16-bit Unicode")
            }
            PrerenderedFontError::BadOffset => write!(f, "prerendered font index is out of range"),
        }
    }
}

impl std::error::Error for PrerenderedFontError {}

/// One pre-rendered character: the location and metrics of its coverage
/// bitmap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrerenderedGlyph {
    /// Absolute offset of the compressed coverage bitmap.
    pub offset: u32,
    /// Bitmap width in pixels.
    pub width: u16,
    /// Bitmap height in pixels.
    pub height: u16,
    /// Left edge relative to the pen position.
    pub origin_x: i16,
    /// Distance from the glyph top to the baseline (reference `OriginY`).
    pub origin_y: i16,
    /// Horizontal advance, in the reference's cell metrics.
    pub inc_x: i16,
    /// Vertical advance (0 for horizontal text).
    pub inc_y: i16,
    /// Character advance; falls back to `inc_x` when zero.
    pub inc: i16,
}

impl PrerenderedGlyph {
    /// Horizontal advance in pixels.
    ///
    /// Prefers the reference `CellIncX` (what `TVPGetCharacter` uses to
    /// advance the pen while drawing, `LayerBitmapImpl.cpp:283`), falling back
    /// to the dedicated `Inc` field. The two differ only for the rare no-ink
    /// `∥` (U+2225) glyph in a few shipped `.tft` files; preferring `IncX`
    /// keeps the drawn glyph positions identical to the reference.
    pub fn advance(&self) -> i32 {
        if self.inc_x != 0 {
            self.inc_x as i32
        } else {
            self.inc as i32
        }
    }
}

/// Hands out [`PrerenderedFont::id`] values.
static NEXT_PRERENDERED_ID: AtomicU64 = AtomicU64::new(1);

/// A parsed `.tft` pre-rendered font. Owns its bytes so it can be shared
/// process-globally behind an `Arc`.
pub struct PrerenderedFont {
    id: u64,
    data: Vec<u8>,
    version: u8,
    count: usize,
    ch_index: usize,
    index: usize,
}

impl PrerenderedFont {
    /// Parse a `.tft` from raw bytes.
    pub fn from_bytes(data: Vec<u8>) -> Result<Self, PrerenderedFontError> {
        if data.len() < HEADER_LEN {
            return Err(PrerenderedFontError::TooShort);
        }
        if &data[..MAGIC.len()] != MAGIC {
            return Err(PrerenderedFontError::BadMagic);
        }
        let version = data[22];
        if version > 1 {
            return Err(PrerenderedFontError::BadVersion(version));
        }
        if data[23] != 2 {
            return Err(PrerenderedFontError::NotUnicode);
        }
        let count = u32::from_le_bytes(data[24..28].try_into().unwrap()) as usize;
        let ch_index = u32::from_le_bytes(data[28..32].try_into().unwrap()) as usize;
        let index = u32::from_le_bytes(data[32..36].try_into().unwrap()) as usize;
        let ch_end = ch_index
            .checked_add(count.saturating_mul(2))
            .ok_or(PrerenderedFontError::BadOffset)?;
        let idx_end = index
            .checked_add(count.saturating_mul(ITEM_LEN))
            .ok_or(PrerenderedFontError::BadOffset)?;
        if ch_end > data.len() || idx_end > data.len() {
            return Err(PrerenderedFontError::BadOffset);
        }
        Ok(Self {
            id: NEXT_PRERENDERED_ID.fetch_add(1, Ordering::Relaxed),
            data,
            version,
            count,
            ch_index,
            index,
        })
    }

    /// Process-unique id (used by caches).
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Number of characters the font provides.
    pub fn glyph_count(&self) -> usize {
        self.count
    }

    /// The file format version (0 or 1).
    pub fn version(&self) -> u8 {
        self.version
    }

    fn char_at(&self, i: usize) -> u16 {
        let at = self.ch_index + i * 2;
        u16::from_le_bytes(self.data[at..at + 2].try_into().unwrap())
    }

    fn item_at(&self, i: usize) -> PrerenderedGlyph {
        let at = self.index + i * ITEM_LEN;
        let item = &self.data[at..at + ITEM_LEN];
        PrerenderedGlyph {
            offset: u32::from_le_bytes(item[0..4].try_into().unwrap()),
            width: u16::from_le_bytes(item[4..6].try_into().unwrap()),
            height: u16::from_le_bytes(item[6..8].try_into().unwrap()),
            origin_x: i16::from_le_bytes(item[8..10].try_into().unwrap()),
            origin_y: i16::from_le_bytes(item[10..12].try_into().unwrap()),
            inc_x: i16::from_le_bytes(item[12..14].try_into().unwrap()),
            inc_y: i16::from_le_bytes(item[14..16].try_into().unwrap()),
            inc: i16::from_le_bytes(item[16..18].try_into().unwrap()),
        }
    }

    /// Look up a character's glyph (the character index is sorted, so this is
    /// a binary search). Characters outside the BMP are never present.
    pub fn find(&self, ch: char) -> Option<PrerenderedGlyph> {
        let code = u32::from(ch);
        if code > 0xFFFF {
            return None;
        }
        let target = code as u16;
        let (mut lo, mut hi) = (0usize, self.count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let c = self.char_at(mid);
            match c.cmp(&target) {
                std::cmp::Ordering::Equal => return Some(self.item_at(mid)),
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        None
    }

    /// Decompress a glyph's coverage bitmap into a `width * height` buffer of
    /// 0..255 alpha values.
    ///
    /// The stored values are 0..64. Version 1 uses `len = byte - 0x40` run
    /// markers for bytes `>= 0x41`; version 0 uses `0x41` followed by a length
    /// byte. Both repeat the previously output byte. The result is then
    /// upscaled by four with saturation (the reference `TVPUpscale65_255`).
    pub fn rasterize(&self, glyph: &PrerenderedGlyph) -> Vec<u8> {
        let total = glyph.width as usize * glyph.height as usize;
        let mut out = Vec::with_capacity(total);
        if total == 0 {
            return out;
        }
        let mut p = glyph.offset as usize;
        while out.len() < total {
            let Some(&byte) = self.data.get(p) else {
                break;
            };
            if self.version == 0 {
                if byte == 0x41 {
                    p += 1;
                    let len = self.data.get(p).copied().unwrap_or(0) as usize;
                    p += 1;
                    let last = out.last().copied().unwrap_or(0);
                    let len = len.min(total - out.len());
                    out.extend(std::iter::repeat_n(last, len));
                } else {
                    out.push(byte);
                    p += 1;
                }
            } else if byte >= 0x41 {
                let len = (byte - 0x40) as usize;
                p += 1;
                let last = out.last().copied().unwrap_or(0);
                let len = len.min(total - out.len());
                out.extend(std::iter::repeat_n(last, len));
            } else {
                out.push(byte);
                p += 1;
            }
        }
        out.resize(total, 0);
        for a in out.iter_mut() {
            *a = a.saturating_mul(4);
        }
        out
    }
}

impl fmt::Debug for PrerenderedFont {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrerenderedFont")
            .field("id", &self.id)
            .field("version", &self.version)
            .field("glyph_count", &self.count)
            .finish_non_exhaustive()
    }
}

/// The properties a pre-rendered font is mapped to (the reference
/// `tTVPFont` minus `Face`'s full-text identity: `face`, `height`, `bold`,
/// `italic`, and `angle` in tenths of a degree).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PrerenderedKey {
    /// Requested face name.
    pub face: String,
    /// Requested pixel height.
    pub height: i32,
    /// Bold flag.
    pub bold: bool,
    /// Italic flag.
    pub italic: bool,
    /// Rotation in tenths of a degree (the reference `Font.Angle`).
    pub angle: i32,
}

/// Process-global `(face, height, style) → pre-rendered font` registry,
/// mirroring the reference `TVPPrerenderedFontMapVector`.
static REGISTRY: LazyLock<Mutex<HashMap<PrerenderedKey, Arc<PrerenderedFont>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Map a font-property combination to a pre-rendered font.
pub fn map_prerendered_font(key: PrerenderedKey, font: Arc<PrerenderedFont>) {
    REGISTRY
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(key, font);
}

/// Look up the pre-rendered font mapped to `key`, if any.
pub fn prerendered_font(key: &PrerenderedKey) -> Option<Arc<PrerenderedFont>> {
    REGISTRY
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(key)
        .cloned()
}

/// Remove all mapped pre-rendered fonts (used by tests).
pub fn clear_prerendered_fonts() {
    REGISTRY
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal version-1 `.tft` with one glyph for `ch`.
    fn build_tft(ch: char, w: u16, h: u16, coverage: &[u8]) -> Vec<u8> {
        build_tft_version(ch, w, h, coverage, 1)
    }

    /// Build a minimal `.tft` of the given version with one glyph for `ch`.
    fn build_tft_version(ch: char, w: u16, h: u16, coverage: &[u8], version: u8) -> Vec<u8> {
        let ch_index = HEADER_LEN + coverage.len();
        let index = ch_index + 2;
        let mut data = vec![0u8; index + ITEM_LEN];
        data[..22].copy_from_slice(MAGIC);
        data[22] = version;
        data[23] = 2; // 16-bit unicode
        data[24..28].copy_from_slice(&1u32.to_le_bytes());
        data[28..32].copy_from_slice(&(ch_index as u32).to_le_bytes());
        data[32..36].copy_from_slice(&(index as u32).to_le_bytes());
        data[HEADER_LEN..HEADER_LEN + coverage.len()].copy_from_slice(coverage);
        data[ch_index..ch_index + 2].copy_from_slice(&(ch as u16).to_le_bytes());
        let item = &mut data[index..index + ITEM_LEN];
        item[0..4].copy_from_slice(&(HEADER_LEN as u32).to_le_bytes());
        item[4..6].copy_from_slice(&w.to_le_bytes());
        item[6..8].copy_from_slice(&h.to_le_bytes());
        item[8..10].copy_from_slice(&0i16.to_le_bytes()); // origin_x
        item[10..12].copy_from_slice(&(h as i16).to_le_bytes()); // origin_y
        item[12..14].copy_from_slice(&(w as i16).to_le_bytes()); // inc_x
        item[16..18].copy_from_slice(&(w as i16).to_le_bytes()); // inc
        data
    }

    /// Build a version-1 `.tft` from a sorted list of `(char, coverage)`,
    /// all with the given bitmap size and advance.
    fn build_multi(glyphs: &[(char, u8)], w: u16, h: u16, advance: i16) -> Vec<u8> {
        let count = glyphs.len();
        let mut bitmaps = Vec::new();
        for &(_, cov) in glyphs {
            bitmaps.push(cov);
        }
        let ch_index = HEADER_LEN + bitmaps.len();
        let index = ch_index + count * 2;
        let mut data = vec![0u8; index + count * ITEM_LEN];
        data[..22].copy_from_slice(MAGIC);
        data[22] = 1;
        data[23] = 2;
        data[24..28].copy_from_slice(&(count as u32).to_le_bytes());
        data[28..32].copy_from_slice(&(ch_index as u32).to_le_bytes());
        data[32..36].copy_from_slice(&(index as u32).to_le_bytes());
        data[HEADER_LEN..HEADER_LEN + bitmaps.len()].copy_from_slice(&bitmaps);
        for (i, &(ch, _)) in glyphs.iter().enumerate() {
            data[ch_index + i * 2..ch_index + i * 2 + 2]
                .copy_from_slice(&(ch as u16).to_le_bytes());
            // Each 1-byte bitmap starts at HEADER_LEN + i.
            let item = &mut data[index + i * ITEM_LEN..index + (i + 1) * ITEM_LEN];
            item[0..4].copy_from_slice(&((HEADER_LEN + i) as u32).to_le_bytes());
            item[4..6].copy_from_slice(&w.to_le_bytes());
            item[6..8].copy_from_slice(&h.to_le_bytes());
            item[10..12].copy_from_slice(&(h as i16).to_le_bytes());
            item[12..14].copy_from_slice(&advance.to_le_bytes());
            item[16..18].copy_from_slice(&advance.to_le_bytes());
        }
        data
    }

    #[test]
    fn parses_and_rasterizes_a_glyph() {
        // Literal coverage 0,1,2,3 → ×4 → 0,4,8,12.
        let bytes = build_tft('A', 2, 2, &[0, 1, 2, 3]);
        let font = PrerenderedFont::from_bytes(bytes).expect("valid tft");
        assert_eq!(font.glyph_count(), 1);
        assert_eq!(font.version(), 1);
        let glyph = font.find('A').expect("glyph present");
        assert_eq!((glyph.width, glyph.height), (2, 2));
        assert_eq!(glyph.advance(), 2);
        assert_eq!(font.rasterize(&glyph), vec![0, 4, 8, 12]);
        assert!(font.find('B').is_none());
        assert!(font.find('\u{1F600}').is_none(), "outside the BMP");
    }

    #[test]
    fn decodes_run_length_markers() {
        // version 1: byte >= 0x41 means "repeat previous (byte - 0x40) times".
        // [5, 0x41] → 5 then (0x41-0x40)=1 repeat of 5 → [5,5]; ×4 → [20,20].
        let bytes = build_tft('x', 2, 1, &[5, 0x41]);
        let font = PrerenderedFont::from_bytes(bytes).unwrap();
        let glyph = font.find('x').unwrap();
        assert_eq!(font.rasterize(&glyph), vec![20, 20]);
    }

    #[test]
    fn version0_uses_0x41_length_marker() {
        // version 0: 0x41 then a length byte repeats the previous value.
        // [2, 0x41, 3] → 2, then 3 repeats of 2 → [2,2,2,2]; ×4 → [8,8,8,8].
        let bytes = build_tft_version('y', 4, 1, &[2, 0x41, 3], 0);
        let font = PrerenderedFont::from_bytes(bytes).unwrap();
        assert_eq!(font.version(), 0);
        let glyph = font.find('y').unwrap();
        assert_eq!(font.rasterize(&glyph), vec![8, 8, 8, 8]);
    }

    #[test]
    fn binary_search_finds_each_glyph_and_rejects_absent() {
        // A sorted index must be searched correctly at both ends and in the
        // middle (the reference uses a half-open binary search).
        let glyphs = [('A', 1u8), ('B', 2), ('C', 3), ('Z', 4)];
        let bytes = build_multi(&glyphs, 1, 1, 7);
        let font = PrerenderedFont::from_bytes(bytes).unwrap();
        assert_eq!(font.glyph_count(), 4);
        for &(ch, cov) in &glyphs {
            let g = font.find(ch).unwrap_or_else(|| panic!("{ch:?} missing"));
            assert_eq!(g.advance(), 7);
            assert_eq!(font.rasterize(&g), vec![cov.saturating_mul(4)]);
        }
        assert!(font.find('D').is_none());
        assert!(font.find('0').is_none());
        assert!(font.find('\u{100}').is_none());
    }

    /// Build a version-1 `.tft` with a 1×1 glyph whose `IncX` and `Inc`
    /// differ (a few shipped fonts do this for the no-ink `∥` U+2225).
    fn build_tft_divergent_advance(ch: char, inc_x: i16, inc: i16) -> Vec<u8> {
        let mut data = build_tft_version(ch, 1, 1, &[1], 1);
        let index = HEADER_LEN + 1 + 2;
        let item = &mut data[index..index + ITEM_LEN];
        item[12..14].copy_from_slice(&inc_x.to_le_bytes()); // inc_x
        item[16..18].copy_from_slice(&inc.to_le_bytes()); // inc
        data
    }

    #[test]
    fn advance_prefers_inc_x_over_inc() {
        // Draw metrics (`CellIncX`) win; the dedicated `Inc` is the fallback.
        let font = PrerenderedFont::from_bytes(build_tft_divergent_advance('∥', 4, 15)).unwrap();
        let glyph = font.find('∥').unwrap();
        assert_eq!((glyph.inc_x, glyph.inc), (4, 15));
        assert_eq!(glyph.advance(), 4);

        let font = PrerenderedFont::from_bytes(build_tft_divergent_advance('x', 0, 9)).unwrap();
        assert_eq!(font.find('x').unwrap().advance(), 9);
    }

    #[test]
    fn rejects_malformed_files() {
        assert_eq!(
            PrerenderedFont::from_bytes(vec![0; 4]).unwrap_err(),
            PrerenderedFontError::TooShort
        );
        let mut bytes = build_tft('A', 1, 1, &[0]);
        bytes[0] = b'X';
        assert_eq!(
            PrerenderedFont::from_bytes(bytes).unwrap_err(),
            PrerenderedFontError::BadMagic
        );
        let mut bytes = build_tft('A', 1, 1, &[0]);
        bytes[23] = 1;
        assert_eq!(
            PrerenderedFont::from_bytes(bytes).unwrap_err(),
            PrerenderedFontError::NotUnicode
        );
    }

    /// Validate the parser against a real `.tft` supplied by the caller
    /// (e.g. a game file). Set `KRKR_RS_TEST_TFT=/path/to/font.tft` to run;
    /// otherwise this is a no-op so CI stays hermetic.
    #[test]
    fn real_tft_parses_when_available() {
        let Some(path) = std::env::var_os("KRKR_RS_TEST_TFT") else {
            return;
        };
        let data = std::fs::read(path).expect("read tft");
        let font = PrerenderedFont::from_bytes(data).expect("parse tft");
        assert!(font.glyph_count() > 0);
        let glyph = font.find('あ').expect("あ must be present");
        assert!(glyph.width > 0 && glyph.height > 0);
        let coverage = font.rasterize(&glyph);
        assert_eq!(coverage.len(), glyph.width as usize * glyph.height as usize);
        assert!(coverage.iter().any(|&a| a > 0), "ink expected");
    }

    #[test]
    fn registry_round_trips() {
        clear_prerendered_fonts();
        let key = PrerenderedKey {
            face: "テスト".into(),
            height: 30,
            bold: false,
            italic: false,
            angle: 0,
        };
        assert!(prerendered_font(&key).is_none());
        let font = Arc::new(PrerenderedFont::from_bytes(build_tft('あ', 1, 1, &[1])).unwrap());
        map_prerendered_font(key.clone(), font.clone());
        assert!(Arc::ptr_eq(&prerendered_font(&key).unwrap(), &font));
        clear_prerendered_fonts();
        assert!(prerendered_font(&key).is_none());
    }
}
