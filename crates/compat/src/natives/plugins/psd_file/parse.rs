//! PSD container parser.
//!
//! Behavioral reference: `reference/cpp/plugins/psdfile/psdparse/`
//! (`psdparse.{h,cpp}`, `psddata.h`, `psdparse/psdfile.cpp`). The reference
//! uses Boost.Spirit grammars over a random-access byte iterator; this port is
//! a hand-written reader over an in-memory byte slice, which produces the same
//! structure without the parser combinator dependency.
//!
//! The parser builds the complete [`PsdFile`] model up front: header, colour
//! mode data, image resources, the layer/mask section (layer records, their
//! additional layer info and each channel's compressed bytes) and the merged
//! image data. Decoding (RLE/zip) lives in [`super::decode`].

use std::collections::BTreeMap;

use super::descriptor;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// A typed PSD parse/decode failure.
#[derive(Debug, thiserror::Error)]
pub(crate) enum PsdError {
    /// The input ended before a required field could be read.
    #[error("unexpected end of PSD data")]
    Eof,
    /// The file is structurally invalid.
    #[error("invalid PSD: {0}")]
    Invalid(String),
    /// A compressed channel could not be expanded.
    #[error("PSD decompression failed: {0}")]
    Decompress(String),
}

/// Convenience alias for parser results.
pub(crate) type Result<T> = std::result::Result<T, PsdError>;

// ---------------------------------------------------------------------------
// Endian-aware byte reader
// ---------------------------------------------------------------------------

/// A big-endian byte cursor over a bounded slice (the reference
/// `IteratorBase`).
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// Wrap `data` at offset zero.
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// The current absolute offset.
    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    /// Whether the cursor has reached the end.
    pub(crate) fn eof(&self) -> bool {
        self.pos >= self.data.len()
    }

    /// Move the cursor to an absolute offset (clamped to the data length).
    pub(crate) fn seek(&mut self, pos: usize) {
        self.pos = pos.min(self.data.len());
    }

    /// Consume `n` bytes and return them.
    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(PsdError::Eof)?;
        if end > self.data.len() {
            return Err(PsdError::Eof);
        }
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    /// Skip `n` bytes.
    pub(crate) fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }

    /// Read one unsigned byte.
    pub(crate) fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    /// Read a big-endian `u16`.
    pub(crate) fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    /// Read a big-endian `i16`.
    pub(crate) fn i16(&mut self) -> Result<i16> {
        Ok(self.u16()? as i16)
    }

    /// Read a big-endian `u32`.
    pub(crate) fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Read a big-endian `i32`.
    pub(crate) fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }

    /// Read a big-endian `u64`.
    pub(crate) fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// Read a 4-byte key as a fixed array.
    pub(crate) fn key4(&mut self) -> Result<[u8; 4]> {
        let b = self.take(4)?;
        Ok([b[0], b[1], b[2], b[3]])
    }

    /// Read the reference `getUnicodeString`: a big-endian `i32` code-unit
    /// count followed by that many UTF-16 big-endian code units.
    pub(crate) fn unicode_string(&mut self) -> Result<String> {
        let count = self.i32()?;
        if count < 0 {
            return Ok(String::new());
        }
        let mut units = Vec::with_capacity(count as usize);
        for _ in 0..count {
            units.push(self.u16()?);
        }
        Ok(String::from_utf16_lossy(&units))
    }

    /// The reference `IteratorBase::getInt16` (`psdparse.h:107-125`): `-1`
    /// when fewer than two bytes remain, otherwise the big-endian value. The
    /// cursor does not advance on a short read.
    pub(crate) fn i16_ref(&mut self) -> i16 {
        if self.data.len().saturating_sub(self.pos) < 2 {
            return -1;
        }
        let b = &self.data[self.pos..self.pos + 2];
        self.pos += 2;
        i16::from_be_bytes([b[0], b[1]])
    }

    /// The reference `IteratorBase::getInt32` (`psdparse.h:127-140`): `-1`
    /// when fewer than four bytes remain.
    pub(crate) fn i32_ref(&mut self) -> i32 {
        if self.data.len().saturating_sub(self.pos) < 4 {
            return -1;
        }
        let b = &self.data[self.pos..self.pos + 4];
        self.pos += 4;
        i32::from_be_bytes([b[0], b[1], b[2], b[3]])
    }

    /// The reference `IteratorBase::getCh` (`psdparse.h:101-105`): `-1` at
    /// the end of the data, otherwise the byte (`0..=255`).
    pub(crate) fn ch_ref(&mut self) -> i32 {
        if self.pos >= self.data.len() {
            return -1;
        }
        let value = i32::from(self.data[self.pos]);
        self.pos += 1;
        value
    }

    /// The reference `IteratorBase::getUnicodeString` (`psdparse.h:181-187`):
    /// a `getInt32` code-unit count followed by that many `getInt16` units. A
    /// short read contributes `-1` units (`0xffff`), matching the reference's
    /// `(wchar_t)-1` push.
    pub(crate) fn unicode_string_ref(&mut self) -> String {
        let count = self.i32_ref();
        if count <= 0 {
            return String::new();
        }
        let mut units = Vec::new();
        for _ in 0..count {
            units.push(self.i16_ref() as u16);
        }
        String::from_utf16_lossy(&units)
    }
}

// ---------------------------------------------------------------------------
// Enumerations
// ---------------------------------------------------------------------------

/// Layer kind (reference `psd::LayerType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LayerType {
    /// A regular pixel layer.
    Normal = 0,
    /// The hidden end-of-folder marker.
    Hidden = 1,
    /// A folder (group) layer.
    Folder = 2,
    /// An adjustment layer.
    Adjust = 3,
    /// A fill layer.
    Fill = 4,
}

impl LayerType {
    /// The reference integer value.
    pub(crate) fn as_i32(self) -> i32 {
        self as i32
    }
}

/// Photoshop blend mode (reference `psd::BlendMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlendMode {
    /// An unknown / unsupported blend key.
    Invalid = -1,
    Normal = 0,
    Dissolve = 1,
    Darken = 2,
    Multiply = 3,
    ColorBurn = 4,
    LinearBurn = 5,
    Lighten = 6,
    Screen = 7,
    ColorDodge = 8,
    LinearDodge = 9,
    Overlay = 10,
    SoftLight = 11,
    HardLight = 12,
    VividLight = 13,
    LinearLight = 14,
    PinLight = 15,
    HardMix = 16,
    Difference = 17,
    Exclusion = 18,
    Hue = 19,
    Saturation = 20,
    Color = 21,
    Luminosity = 22,
    PassThrough = 23,
    DarkerColor = 24,
    LighterColor = 25,
    Subtract = 26,
    Divide = 27,
}

impl BlendMode {
    /// Map a 4-byte blend key to its mode (reference `blendKeyToMode`).
    pub(crate) fn from_key(key: [u8; 4]) -> Self {
        match &key {
            b"norm" => Self::Normal,
            b"diss" => Self::Dissolve,
            b"dark" => Self::Darken,
            b"mul " => Self::Multiply,
            b"idiv" => Self::ColorBurn,
            b"lbrn" => Self::LinearBurn,
            b"dkCl" => Self::DarkerColor,
            b"lite" => Self::Lighten,
            b"scrn" => Self::Screen,
            b"div " => Self::ColorDodge,
            b"lddg" => Self::LinearDodge,
            b"ltCl" => Self::LighterColor,
            b"over" => Self::Overlay,
            b"sLit" => Self::SoftLight,
            b"hLit" => Self::HardLight,
            b"vLit" => Self::VividLight,
            b"lLit" => Self::LinearLight,
            b"pLit" => Self::PinLight,
            b"hMix" => Self::HardMix,
            b"diff" => Self::Difference,
            b"smud" => Self::Exclusion,
            b"fsub" => Self::Subtract,
            b"fdiv" => Self::Divide,
            b"hue " => Self::Hue,
            b"sat " => Self::Saturation,
            b"colr" => Self::Color,
            b"lum " => Self::Luminosity,
            b"pass" => Self::PassThrough,
            _ => Self::Invalid,
        }
    }

    /// The reference integer value.
    pub(crate) fn as_i32(self) -> i32 {
        self as i32
    }

    /// The KiriKiri `tTVPLayerType` this blend maps onto (reference
    /// `convBlendMode`). Unsupported blends fall back to `ltPsNormal`.
    pub(crate) fn as_tvp(self) -> i32 {
        match self {
            Self::Normal => 13,      // ltPsNormal
            Self::Darken => 25,      // ltPsDarken
            Self::Multiply => 16,    // ltPsMultiplicative
            Self::ColorBurn => 23,   // ltPsColorBurn
            Self::LinearBurn => 15,  // ltPsSubtractive
            Self::Lighten => 24,     // ltPsLighten
            Self::Screen => 17,      // ltPsScreen
            Self::ColorDodge => 21,  // ltPsColorDodge
            Self::LinearDodge => 14, // ltPsAdditive
            Self::Overlay => 18,     // ltPsOverlay
            Self::SoftLight => 20,   // ltPsSoftLight
            Self::HardLight => 19,   // ltPsHardLight
            Self::Difference => 26,  // ltPsDifference
            Self::Exclusion => 28,   // ltPsExclusion
            _ => 13,
        }
    }
}

/// How a layer image should be assembled (reference `psd::ImageMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImageMode {
    /// The image without its mask.
    Image,
    /// The mask only, rendered as grayscale.
    Mask,
    /// The image with the mask folded into the alpha channel.
    MaskedImage,
}

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

/// The PSD file header.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Header {
    pub(crate) channels: u16,
    pub(crate) height: i32,
    pub(crate) width: i32,
    pub(crate) depth: u16,
    pub(crate) mode: u16,
}

/// One layer channel's compressed payload.
#[derive(Debug, Clone)]
pub(crate) struct ChannelInfo {
    pub(crate) id: i16,
    pub(crate) length: i32,
    pub(crate) compression: u16,
    /// The bytes after the 2-byte compression id.
    pub(crate) data: Vec<u8>,
}

impl ChannelInfo {
    /// Whether this channel is a user/real user mask channel.
    pub(crate) fn is_mask_channel(&self) -> bool {
        self.id == -2 || self.id == -3
    }
}

/// A layer mask rectangle and its flags.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LayerMask {
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) top: i32,
    pub(crate) left: i32,
    pub(crate) bottom: i32,
    pub(crate) right: i32,
    pub(crate) default_color: i32,
    pub(crate) flags: i32,
    pub(crate) real_flags: i32,
    pub(crate) real_user_mask_background: i32,
    pub(crate) enclosing_top: i32,
    pub(crate) enclosing_left: i32,
    pub(crate) enclosing_bottom: i32,
    pub(crate) enclosing_right: i32,
}

/// One additional layer info block.
#[derive(Debug, Clone)]
pub(crate) struct AdditionalLayer {
    /// 0 = `8BIM`, 1 = `8B64`.
    pub(crate) sig_type: i32,
    pub(crate) key: [u8; 4],
    pub(crate) data: Vec<u8>,
}

/// A layer's parsed extra data block.
#[derive(Debug, Clone, Default)]
pub(crate) struct LayerExtra {
    pub(crate) layer_mask: LayerMask,
    pub(crate) layer_name: String,
    pub(crate) additional: Vec<AdditionalLayer>,
}

/// Per-layer layer-comp offset/visibility state (reference `LayerCompInfo`).
#[derive(Debug, Clone, Default)]
pub(crate) struct LayerCompInfo {
    pub(crate) id: i32,
    pub(crate) offset_x: i32,
    pub(crate) offset_y: i32,
    pub(crate) is_enabled: bool,
}

/// One Photoshop layer.
#[derive(Debug, Clone)]
pub(crate) struct LayerInfo {
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) top: i32,
    pub(crate) left: i32,
    pub(crate) bottom: i32,
    pub(crate) right: i32,
    pub(crate) channels: Vec<ChannelInfo>,
    pub(crate) blend_mode: BlendMode,
    pub(crate) opacity: u8,
    pub(crate) fill_opacity: i32,
    pub(crate) clipping: u8,
    pub(crate) flag: u8,
    pub(crate) extra_data: LayerExtra,
    pub(crate) layer_id: i32,
    pub(crate) layer_type: LayerType,
    pub(crate) layer_name: String,
    pub(crate) layer_name_unicode: String,
    pub(crate) layer_comps: BTreeMap<i32, LayerCompInfo>,
    /// Index into [`PsdFile::layers`] of the enclosing folder, if any.
    pub(crate) parent: Option<usize>,
}

impl LayerInfo {
    /// Reference `isVisible`.
    pub(crate) fn is_visible(&self) -> bool {
        self.flag & (1 << 1) == 0
    }

    /// Reference `isTransparencyProtected`.
    pub(crate) fn is_transparency_protected(&self) -> bool {
        self.flag & (1 << 0) != 0
    }

    /// Reference `isObsolete`.
    pub(crate) fn is_obsolete(&self) -> bool {
        self.flag & (1 << 2) != 0
    }

    /// Reference `isPixelDataIrrelevant`.
    pub(crate) fn is_pixel_data_irrelevant(&self) -> bool {
        self.flag & (1 << 4) != 0
    }
}

/// One image resource block (reference `ImageResourceInfo`).
#[derive(Debug, Clone)]
pub(crate) struct ImageResource {
    pub(crate) id: u16,
    pub(crate) data: Vec<u8>,
}

/// A parsed slice item (reference `SliceItem`).
#[derive(Debug, Clone, Default)]
pub(crate) struct SliceItem {
    pub(crate) id: i32,
    pub(crate) group_id: i32,
    pub(crate) origin: i32,
    pub(crate) associated_layer_id: i32,
    pub(crate) name: String,
    pub(crate) slice_type: i32,
    pub(crate) left: i32,
    pub(crate) top: i32,
    pub(crate) right: i32,
    pub(crate) bottom: i32,
    pub(crate) url: String,
    pub(crate) target: String,
    pub(crate) message: String,
    pub(crate) alt_tag: String,
    pub(crate) cell_text_is_html: bool,
    pub(crate) cell_text: String,
    pub(crate) horizontal_align: i32,
    pub(crate) vertical_align: i32,
    pub(crate) color_a: u8,
    pub(crate) color_r: u8,
    pub(crate) color_g: u8,
    pub(crate) color_b: u8,
}

/// The parsed slice resource (reference `SliceResource`).
#[derive(Debug, Clone, Default)]
pub(crate) struct SliceResource {
    pub(crate) bounding_left: i32,
    pub(crate) bounding_top: i32,
    pub(crate) bounding_right: i32,
    pub(crate) bounding_bottom: i32,
    pub(crate) group_name: String,
    pub(crate) slices: Vec<SliceItem>,
}

/// One guide (reference `GuideItem`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct GuideItem {
    pub(crate) location: i32,
    /// 0 = vertical, 1 = horizontal (`-1` on a short read).
    pub(crate) direction: i32,
}

/// The parsed grid/guide resource (reference `GridGuideResource`).
#[derive(Debug, Clone, Default)]
pub(crate) struct GridGuideResource {
    pub(crate) horizontal_grid: i32,
    pub(crate) vertical_grid: i32,
    pub(crate) guides: Vec<GuideItem>,
}

/// The indexed-colour table (reference `ColorTable`).
#[derive(Debug, Clone)]
pub(crate) struct ColorTable {
    /// Always 256 RGBA entries.
    pub(crate) colors: Vec<[u8; 4]>,
    pub(crate) transparency_index: i16,
    pub(crate) valid_count: i16,
}

impl Default for ColorTable {
    fn default() -> Self {
        Self {
            colors: vec![[0, 0, 0, 0]; 256],
            transparency_index: -1,
            valid_count: 0,
        }
    }
}

/// A document-level layer comp (reference `LayerComp`).
#[derive(Debug, Clone, Default)]
pub(crate) struct LayerComp {
    pub(crate) id: i32,
    pub(crate) record_visibility: bool,
    pub(crate) record_position: bool,
    pub(crate) record_appearance: bool,
    pub(crate) name: String,
    pub(crate) comment: String,
}

/// The complete parsed PSD document (reference `psd::Data`).
#[derive(Debug, Clone)]
pub(crate) struct PsdFile {
    pub(crate) header: Header,
    pub(crate) layers: Vec<LayerInfo>,
    pub(crate) slice: Option<SliceResource>,
    pub(crate) grid_guide: Option<GridGuideResource>,
    pub(crate) color_table: ColorTable,
    pub(crate) layer_comps: Vec<LayerComp>,
    pub(crate) last_applied_comp_id: i32,
    /// The merged composite image (compression id + body), if present.
    pub(crate) image_data: Option<Vec<u8>>,
}

/// Colour mode constants (reference `psd::ColorMode`).
pub(crate) const COLOR_MODE_BITMAP: u16 = 0;
pub(crate) const COLOR_MODE_GRAYSCALE: u16 = 1;
pub(crate) const COLOR_MODE_INDEXED: u16 = 2;
pub(crate) const COLOR_MODE_RGB: u16 = 3;
pub(crate) const COLOR_MODE_CMYK: u16 = 4;
pub(crate) const COLOR_MODE_MULTICHANNEL: u16 = 7;
pub(crate) const COLOR_MODE_DUOTONE: u16 = 8;
pub(crate) const COLOR_MODE_LAB: u16 = 9;

// ---------------------------------------------------------------------------
// Top-level parse
// ---------------------------------------------------------------------------

/// Parse a complete PSD document.
pub(crate) fn parse(bytes: &[u8]) -> Result<PsdFile> {
    let mut r = Reader::new(bytes);

    let signature = r.take(4)?;
    if signature != b"8BPS" {
        return Err(PsdError::Invalid("not a PSD file (bad signature)".into()));
    }
    let _version = r.u16()?;
    r.skip(6)?;
    let channels = r.u16()?;
    let height = r.u32()? as i32;
    let width = r.u32()? as i32;
    let depth = r.u16()?;
    let mode = r.u16()?;
    let header = Header {
        channels,
        height,
        width,
        depth,
        mode,
    };

    // Colour mode data.
    let color_mode_len = r.u32()? as usize;
    let color_mode = r.take(color_mode_len)?.to_vec();

    // Image resources.
    let resource_len = r.u32()? as usize;
    let resource_bytes = r.take(resource_len)?;
    let resources = parse_image_resources(resource_bytes)?;

    // Layer and mask information. A zero-length section carries no inner
    // structure at all: the reference `setLayerAndMask` skips the inner
    // parse when `size == 0` (`psdparse/psdparse.h:704-711`, inner skip at
    // `:617-630`), so an empty section is a successful parse with no layers.
    let layer_mask_len = r.u32()? as usize;
    let layers = if layer_mask_len > 0 {
        let layer_mask = r.take(layer_mask_len)?;
        parse_layer_and_mask(layer_mask)?
    } else {
        Vec::new()
    };

    // Merged image data (everything that remains).
    let image_data = if r.eof() {
        None
    } else {
        let remaining = r.take(r.data.len() - r.pos)?;
        Some(remaining.to_vec())
    };

    let mut file = PsdFile {
        header,
        layers,
        slice: None,
        grid_guide: None,
        color_table: ColorTable::default(),
        layer_comps: Vec::new(),
        last_applied_comp_id: -1,
        image_data,
    };
    file.process(&resources, &color_mode);
    Ok(file)
}

impl PsdFile {
    /// Second pass (reference `Data::processParsed`): expand the resources and
    /// the per-layer additional info into the typed structures.
    fn process(&mut self, resources: &[ImageResource], color_mode: &[u8]) {
        for resource in resources {
            match resource.id {
                1032 => self.load_grid_and_guide(&resource.data),
                1050 => self.load_slice(&resource.data),
                1046 => {
                    // Reference `loadResourceColorTableCount` reads through
                    // `IteratorBase` (`getInt16`), so a short block is `-1`.
                    let mut r = Reader::new(&resource.data);
                    self.color_table.valid_count = r.i16_ref();
                }
                1047 => {
                    // Reference `loadResourceTransparencyIndex` (`getInt16`).
                    let mut r = Reader::new(&resource.data);
                    self.color_table.transparency_index = r.i16_ref();
                }
                1065 => self.load_layer_comps(&resource.data),
                _ => {}
            }
        }

        // Indexed palette.
        if self.header.mode == COLOR_MODE_INDEXED && color_mode.len() == 768 {
            for (i, &red) in color_mode[..256].iter().enumerate() {
                self.color_table.colors[i][0] = red;
                self.color_table.colors[i][3] = 0xff;
            }
            for (i, &green) in color_mode[256..512].iter().enumerate() {
                self.color_table.colors[i][1] = green;
            }
            for (i, &blue) in color_mode[512..768].iter().enumerate() {
                self.color_table.colors[i][2] = blue;
            }
            let index = self.color_table.transparency_index;
            if (0..256).contains(&index) {
                self.color_table.colors[index as usize][3] = 0;
            }
        }

        for layer in &mut self.layers {
            layer.layer_name = layer.extra_data.layer_name.clone();
            layer.layer_type = LayerType::Normal;
            layer.layer_id = -1;
            layer.fill_opacity = 255;

            let additionals = layer.extra_data.additional.clone();
            for additional in &additionals {
                apply_additional_layer(layer, additional);
            }
        }

        // Link each layer to its enclosing folder (reference: walk from the
        // topmost record downwards, pushing/popping on the folder markers).
        let mut stack: Vec<Option<usize>> = vec![None];
        for i in (0..self.layers.len()).rev() {
            let layer_type = self.layers[i].layer_type;
            self.layers[i].parent = *stack.last().unwrap_or(&None);
            match layer_type {
                LayerType::Folder => stack.push(Some(i)),
                LayerType::Hidden => {
                    stack.pop();
                }
                _ => {}
            }
        }
    }

    fn load_grid_and_guide(&mut self, data: &[u8]) {
        let mut r = Reader::new(data);
        // Reference `loadResourceGridAndGuide`: the version is read and
        // discarded, every field comes from `IteratorBase` (a short read is
        // `-1`), a non-positive guide count yields no guides, and the
        // resource is enabled as soon as it is seen.
        let _version = r.i32_ref();
        let horizontal_grid = r.i32_ref();
        let vertical_grid = r.i32_ref();
        let guide_num = r.i32_ref();
        let mut guides = Vec::new();
        let mut i = 0;
        while i < guide_num {
            guides.push(GuideItem {
                location: r.i32_ref(),
                direction: r.ch_ref(),
            });
            i += 1;
        }
        self.grid_guide = Some(GridGuideResource {
            horizontal_grid,
            vertical_grid,
            guides,
        });
    }

    fn load_slice(&mut self, data: &[u8]) {
        let mut r = Reader::new(data);
        // Reference `loadResourceSlice`: the v6 fields all come from
        // `IteratorBase`, so a short read yields `-1`/empty instead of
        // aborting, and the resource is enabled for any v6 block. The
        // `uint8_t` colour fields store `getCh()` (so a short read is
        // `0xff`).
        let version = r.i32_ref();
        if version == 6 {
            let mut slice = SliceResource {
                bounding_left: r.i32_ref(),
                bounding_top: r.i32_ref(),
                bounding_right: r.i32_ref(),
                bounding_bottom: r.i32_ref(),
                ..Default::default()
            };
            slice.group_name = r.unicode_string_ref();
            let slice_num = r.i32_ref();
            let mut i = 0;
            while i < slice_num {
                let mut item = SliceItem {
                    id: r.i32_ref(),
                    group_id: r.i32_ref(),
                    origin: r.i32_ref(),
                    ..Default::default()
                };
                if item.origin == 1 {
                    item.associated_layer_id = r.i32_ref();
                } else {
                    item.associated_layer_id = -1;
                }
                item.name = r.unicode_string_ref();
                item.slice_type = r.i32_ref();
                item.left = r.i32_ref();
                item.top = r.i32_ref();
                item.right = r.i32_ref();
                item.bottom = r.i32_ref();
                item.url = r.unicode_string_ref();
                item.target = r.unicode_string_ref();
                item.message = r.unicode_string_ref();
                item.alt_tag = r.unicode_string_ref();
                item.cell_text_is_html = r.ch_ref() != 0;
                item.cell_text = r.unicode_string_ref();
                item.horizontal_align = r.i32_ref();
                item.vertical_align = r.i32_ref();
                item.color_a = r.ch_ref() as u8;
                item.color_r = r.ch_ref() as u8;
                item.color_g = r.ch_ref() as u8;
                item.color_b = r.ch_ref() as u8;
                slice.slices.push(item);
                i += 1;
            }
            self.slice = Some(slice);
        }
        // v7/v8 are descriptor-based; the reference parses (and discards) them.
    }

    fn load_layer_comps(&mut self, data: &[u8]) {
        let mut r = Reader::new(data);
        let Ok(version) = r.i32() else { return };
        if version != 16 {
            return;
        }
        let Ok(descriptor) = descriptor::read_descriptor(&mut r) else {
            return;
        };
        self.last_applied_comp_id = descriptor.integer("lastAppliedComp").unwrap_or(-1);
        let Some(list) = descriptor.list("list") else {
            return;
        };
        for value in list {
            let Some(comp) = value.as_descriptor() else {
                continue;
            };
            let Some(id) = comp.integer("compID") else {
                continue;
            };
            let mut layer_comp = LayerComp {
                id,
                ..Default::default()
            };
            if let Some(captured) = comp.integer("capturedInfo") {
                layer_comp.record_visibility = captured & (1 << 0) != 0;
                layer_comp.record_position = captured & (1 << 1) != 0;
                layer_comp.record_appearance = captured & (1 << 2) != 0;
            }
            if let Some(name) = comp.text("Nm  ") {
                layer_comp.name = name.to_string();
            }
            if let Some(comment) = comp.text("comment") {
                layer_comp.comment = comment.to_string();
            }
            self.layer_comps.push(layer_comp);
        }
    }
}

// ---------------------------------------------------------------------------
// Image resources
// ---------------------------------------------------------------------------

fn parse_image_resources(bytes: &[u8]) -> Result<Vec<ImageResource>> {
    let mut r = Reader::new(bytes);
    let mut resources = Vec::new();
    while !r.eof() {
        let signature = r.take(4)?;
        if signature != b"8BIM" {
            return Err(PsdError::Invalid("bad image resource signature".into()));
        }
        let id = r.u16()?;
        let name_len = r.u8()? as usize;
        let _name = r.take(name_len)?;
        r.skip((name_len + 1) % 2)?;
        let size = r.u32()? as usize;
        let data = r.take(size)?.to_vec();
        r.skip(size % 2)?;
        resources.push(ImageResource { id, data });
    }
    Ok(resources)
}

// ---------------------------------------------------------------------------
// Layer and mask section
// ---------------------------------------------------------------------------

fn parse_layer_and_mask(bytes: &[u8]) -> Result<Vec<LayerInfo>> {
    let mut r = Reader::new(bytes);

    let layer_info_len = r.u32()? as usize;
    let mut layers = Vec::new();
    if layer_info_len > 0 {
        let layer_info = r.take(layer_info_len)?;
        layers = parse_layer_info(layer_info)?;
    }

    // Global layer mask info (a size-prefixed block; parsed for structure,
    // the fields are not exposed by the plugin).
    if !r.eof() {
        let size = r.u32()? as usize;
        r.skip(size)?;
    }

    // Optional additional layer information (Lr16 / Lr32) repeats a layer
    // info block; the reference parses and discards it.
    if !r.eof() {
        let signature = r.take(4)?;
        let key = r.take(4)?;
        if signature == b"8BIM" && (key == b"Lr16" || key == b"Lr32") {
            let size = r.u32()? as usize;
            r.skip(size)?;
        }
    }

    Ok(layers)
}

fn parse_layer_info(bytes: &[u8]) -> Result<Vec<LayerInfo>> {
    let mut r = Reader::new(bytes);
    let count = r.i16()?;
    let _merged_alpha = count < 0;
    let layer_count = count.unsigned_abs() as usize;

    let mut layers = Vec::with_capacity(layer_count);
    for _ in 0..layer_count {
        let top = r.i32()?;
        let left = r.i32()?;
        let bottom = r.i32()?;
        let right = r.i32()?;
        let channel_count = r.u16()?;
        let mut channels = Vec::with_capacity(channel_count as usize);
        for _ in 0..channel_count {
            let id = r.i16()?;
            let length = r.i32()?;
            channels.push(ChannelInfo {
                id,
                length,
                compression: 0,
                data: Vec::new(),
            });
        }

        let signature = r.take(4)?;
        if signature != b"8BIM" {
            return Err(PsdError::Invalid("bad layer blend signature".into()));
        }
        let blend_mode_key = r.key4()?;
        let opacity = r.u8()?;
        let clipping = r.u8()?;
        let flag = r.u8()?;
        let _filler = r.u8()?;
        let extra_len = r.u32()? as usize;
        let extra_bytes = r.take(extra_len)?;
        let extra_data = parse_layer_extra(extra_bytes)?;

        layers.push(LayerInfo {
            width: right - left,
            height: bottom - top,
            top,
            left,
            bottom,
            right,
            channels,
            blend_mode: BlendMode::from_key(blend_mode_key),
            opacity,
            fill_opacity: 255,
            clipping,
            flag,
            extra_data,
            layer_id: -1,
            layer_type: LayerType::Normal,
            layer_name: String::new(),
            layer_name_unicode: String::new(),
            layer_comps: BTreeMap::new(),
            parent: None,
        });
    }

    // Channel image data follows every layer record, in the same order.
    for layer in &mut layers {
        for channel in &mut layer.channels {
            let compression = r.u16()?;
            let body_len = (channel.length - 2).max(0) as usize;
            channel.compression = compression;
            channel.data = r.take(body_len)?.to_vec();
        }
    }

    Ok(layers)
}

fn parse_layer_extra(bytes: &[u8]) -> Result<LayerExtra> {
    let mut r = Reader::new(bytes);
    let mut extra = LayerExtra::default();

    let mask_len = r.i32()?;
    if mask_len > 0 {
        let mask_bytes = r.take(mask_len as usize)?;
        extra.layer_mask = parse_layer_mask(mask_bytes);
    }

    let blending_len = r.i32()?;
    if blending_len > 0 {
        r.skip(blending_len as usize)?;
    }

    let name_len = r.u8()? as usize;
    let name_bytes = r.take(name_len)?;
    extra.layer_name = String::from_utf8_lossy(name_bytes).into_owned();
    let pad = (4 - ((name_len + 1) & 3)) & 3;
    r.skip(pad)?;

    while !r.eof() {
        let signature = r.take(4)?;
        let sig_type = if signature == b"8BIM" {
            0
        } else if signature == b"8B64" {
            1
        } else {
            return Err(PsdError::Invalid("bad additional layer signature".into()));
        };
        let key = r.key4()?;
        let size = r.u32()? as usize;
        let data = r.take(size)?.to_vec();
        extra.additional.push(AdditionalLayer {
            sig_type,
            key,
            data,
        });
    }

    Ok(extra)
}

fn parse_layer_mask(bytes: &[u8]) -> LayerMask {
    let mut mask = LayerMask::default();
    // The reference `LayerMaskParser` grammar requires the full 19-byte base
    // record; a shorter block fails the parse and leaves the zero-initialised
    // mask. The extra fields need the full 37-byte record. Only `top`,
    // `left`, `bottom`, `right` and `default_color` are script-observable
    // (`psdclass.cpp:317-371`).
    if bytes.len() < 19 {
        return mask;
    }
    mask.top = i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    mask.left = i32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    mask.bottom = i32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    mask.right = i32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    mask.default_color = i32::from(bytes[16]);
    mask.flags = i32::from(bytes[17]);
    // Byte 18 (`kind`) is parsed and discarded.
    mask.width = mask.right - mask.left;
    mask.height = mask.bottom - mask.top;
    if bytes.len() >= 37 {
        mask.real_flags = i32::from(bytes[19]);
        mask.real_user_mask_background = i32::from(bytes[20]);
        mask.enclosing_top = i32::from_be_bytes([bytes[21], bytes[22], bytes[23], bytes[24]]);
        mask.enclosing_left = i32::from_be_bytes([bytes[25], bytes[26], bytes[27], bytes[28]]);
        mask.enclosing_bottom = i32::from_be_bytes([bytes[29], bytes[30], bytes[31], bytes[32]]);
        mask.enclosing_right = i32::from_be_bytes([bytes[33], bytes[34], bytes[35], bytes[36]]);
    }
    mask
}

// ---------------------------------------------------------------------------
// Additional layer info
// ---------------------------------------------------------------------------

fn apply_additional_layer(layer: &mut LayerInfo, additional: &AdditionalLayer) {
    if additional.sig_type != 0 {
        return;
    }
    match &additional.key {
        b"grdm" | b"levl" | b"curv" | b"hue " | b"hue2" | b"blnc" | b"nvrt" | b"post" | b"thrs"
        | b"selc" | b"brit" | b"mixr" | b"clrL" | b"phfl" | b"blwh" | b"vibA" | b"expA" => {
            layer.layer_type = LayerType::Adjust
        }
        b"SoCo" | b"GdFl" | b"PtFl" => layer.layer_type = LayerType::Fill,
        b"lsct" | b"lsdk" => load_section_divider(layer, additional),
        b"luni" => {
            // Reference `loadLayerUnicodeName` (`getUnicodeString`).
            let mut r = Reader::new(&additional.data);
            layer.layer_name_unicode = r.unicode_string_ref();
        }
        b"lyid" => {
            // Reference `loadLayerId` (`getInt32`; `-1` on a short read).
            let mut r = Reader::new(&additional.data);
            layer.layer_id = r.i32_ref();
        }
        b"shmd" => load_layer_metadata(layer, additional),
        b"iOpa" => {
            // Reference `loadLayerFillOpacity` (`getCh`; `-1` on a short
            // read, which the reference stores in its `int` field).
            let mut r = Reader::new(&additional.data);
            layer.fill_opacity = r.ch_ref();
        }
        _ => {}
    }
}

fn load_section_divider(layer: &mut LayerInfo, additional: &AdditionalLayer) {
    let mut r = Reader::new(&additional.data);
    let divider_type = r.i32_ref();
    match divider_type {
        1 | 2 => layer.layer_type = LayerType::Folder,
        3 => layer.layer_type = LayerType::Hidden,
        _ => {}
    }
    if additional.data.len() >= 12 {
        let _signature = r.i32_ref();
        let key = r.i32_ref();
        layer.blend_mode = BlendMode::from_key(key.to_be_bytes());
        if additional.data.len() >= 16 {
            let _sub_type = r.i32_ref();
        }
    }
}

fn load_layer_metadata(layer: &mut LayerInfo, additional: &AdditionalLayer) {
    let mut r = Reader::new(&additional.data);
    let Ok(count) = r.i32() else { return };
    for _ in 0..count.max(0) {
        let Ok(_signature) = r.i32() else { break };
        let Ok(key) = r.key4() else { break };
        if r.u8().is_err() {
            break;
        }
        if r.skip(3).is_err() {
            break;
        }
        let Ok(len) = r.i32() else { break };
        let value_start = r.pos();
        if &key == b"cmls" {
            load_layer_comp_descriptor(layer, &mut r);
        }
        let end = value_start.saturating_add(len.max(0) as usize);
        if end > additional.data.len() {
            break;
        }
        r.seek(end);
    }
}

fn load_layer_comp_descriptor(layer: &mut LayerInfo, r: &mut Reader<'_>) {
    let Ok(version) = r.i32() else { return };
    if version != 16 {
        return;
    }
    let Ok(descriptor) = descriptor::read_descriptor(r) else {
        return;
    };
    let Some(settings) = descriptor.list("layerSettings") else {
        return;
    };
    let mut last_enabled = true;
    for value in settings {
        let Some(comp) = value.as_descriptor() else {
            continue;
        };
        let Some(comp_list) = comp.list("compList") else {
            continue;
        };
        let Some(id) = comp_list
            .first()
            .and_then(super::descriptor::Value::as_integer)
        else {
            continue;
        };
        let mut info = LayerCompInfo {
            id,
            ..Default::default()
        };
        if let Some(offset) = comp.descriptor("Ofst") {
            // The reference reads `Hrzn` for both axes; kept for fidelity.
            info.offset_x = offset.integer("Hrzn").unwrap_or(0);
            info.offset_y = offset.integer("Hrzn").unwrap_or(0);
        }
        match comp.boolean("enab") {
            Some(enabled) => {
                info.is_enabled = enabled;
                last_enabled = enabled;
            }
            None => info.is_enabled = last_enabled,
        }
        layer.layer_comps.insert(id, info);
    }
}

#[cfg(test)]
mod tests {
    use super::{Reader, parse_layer_mask};

    /// A short read from the reference `IteratorBase` yields `-1`/empty
    /// (`reference/cpp/plugins/psdfile/psdparse/psdparse.h:101-187`), not a
    /// zero default.
    #[test]
    fn short_reads_use_the_reference_sentinels() {
        let mut reader = Reader::new(&[0x01]);
        assert_eq!(reader.i32_ref(), -1);
        assert_eq!(reader.i16_ref(), -1);
        assert_eq!(reader.ch_ref(), 1);
        assert_eq!(reader.ch_ref(), -1);

        let mut empty = Reader::new(&[]);
        assert_eq!(empty.unicode_string_ref(), "");
    }

    /// A layer-mask block shorter than the reference grammar's 19-byte base
    /// record leaves the zero-initialised mask (`LayerMaskParser` in
    /// `reference/cpp/plugins/psdfile/psdparse/psdparse.h`).
    #[test]
    fn short_layer_mask_stays_zero() {
        let mask = parse_layer_mask(&[0u8; 4]);
        assert_eq!(mask.top, 0);
        assert_eq!(mask.default_color, 0);
    }
}
