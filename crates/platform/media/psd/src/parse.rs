//! PSD container reading.
//!
//! The reader is a hand-written cursor over an in-memory byte slice. It builds
//! the complete [`PsdFile`] model up front: header, colour mode data, image
//! resources, the layer/mask section (layer records, their additional layer
//! info and each channel's compressed bytes) and the merged image data.
//! Channel expansion lives in [`crate::decode`].
//!
//! Two reading styles coexist because the format needs both: the checked
//! accessors fail with [`PsdError::Eof`] when the input ends early, and the
//! `*_ref` accessors return the format's own short-read sentinels (`-1`, an
//! empty string) for the record fields that tolerate a truncated block.

use std::collections::BTreeMap;

use crate::descriptor::{self, Value};
use crate::error::{PsdError, PsdResult};

// ---------------------------------------------------------------------------
// Endian-aware byte reader
// ---------------------------------------------------------------------------

/// A big-endian byte cursor over a bounded slice.
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
    pub(crate) fn take(&mut self, n: usize) -> PsdResult<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(PsdError::Eof)?;
        if end > self.data.len() {
            return Err(PsdError::Eof);
        }
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    /// Skip `n` bytes.
    pub(crate) fn skip(&mut self, n: usize) -> PsdResult<()> {
        self.take(n).map(|_| ())
    }

    /// Read one unsigned byte.
    pub(crate) fn u8(&mut self) -> PsdResult<u8> {
        Ok(self.take(1)?[0])
    }

    /// Read a big-endian `u16`.
    pub(crate) fn u16(&mut self) -> PsdResult<u16> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    /// Read a big-endian `i16`.
    pub(crate) fn i16(&mut self) -> PsdResult<i16> {
        Ok(self.u16()? as i16)
    }

    /// Read a big-endian `u32`.
    pub(crate) fn u32(&mut self) -> PsdResult<u32> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Read a big-endian `i32`.
    pub(crate) fn i32(&mut self) -> PsdResult<i32> {
        Ok(self.u32()? as i32)
    }

    /// Read a big-endian `u64`.
    pub(crate) fn u64(&mut self) -> PsdResult<u64> {
        let b = self.take(8)?;
        Ok(u64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// Read a 4-byte key as a fixed array.
    pub(crate) fn key4(&mut self) -> PsdResult<[u8; 4]> {
        let b = self.take(4)?;
        Ok([b[0], b[1], b[2], b[3]])
    }

    /// Read a length-prefixed big-endian UTF-16 string: an `i32` code-unit
    /// count followed by that many units. A negative count is empty.
    pub(crate) fn unicode_string(&mut self) -> PsdResult<String> {
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

    /// A big-endian `i16` that yields `-1` when fewer than two bytes remain.
    /// The cursor does not advance on a short read.
    pub(crate) fn i16_ref(&mut self) -> i16 {
        if self.data.len().saturating_sub(self.pos) < 2 {
            return -1;
        }
        let b = &self.data[self.pos..self.pos + 2];
        self.pos += 2;
        i16::from_be_bytes([b[0], b[1]])
    }

    /// A big-endian `i32` that yields `-1` when fewer than four bytes remain.
    pub(crate) fn i32_ref(&mut self) -> i32 {
        if self.data.len().saturating_sub(self.pos) < 4 {
            return -1;
        }
        let b = &self.data[self.pos..self.pos + 4];
        self.pos += 4;
        i32::from_be_bytes([b[0], b[1], b[2], b[3]])
    }

    /// One byte as `0..=255`, or `-1` at the end of the data.
    pub(crate) fn ch_ref(&mut self) -> i32 {
        if self.pos >= self.data.len() {
            return -1;
        }
        let value = i32::from(self.data[self.pos]);
        self.pos += 1;
        value
    }

    /// A length-prefixed UTF-16 string read through the short-read sentinels: a
    /// short unit contributes `-1` (`0xffff`).
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

/// A layer's kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerType {
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
    /// The layer kind as the integer the plugin surface reports.
    pub fn as_i32(self) -> i32 {
        self as i32
    }
}

/// A Photoshop blend mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendMode {
    /// An unknown or unsupported blend key.
    Invalid = -1,
    /// `norm`.
    Normal = 0,
    /// `diss`.
    Dissolve = 1,
    /// `dark`.
    Darken = 2,
    /// `mul `.
    Multiply = 3,
    /// `idiv`.
    ColorBurn = 4,
    /// `lbrn`.
    LinearBurn = 5,
    /// `lite`.
    Lighten = 6,
    /// `scrn`.
    Screen = 7,
    /// `div `.
    ColorDodge = 8,
    /// `lddg`.
    LinearDodge = 9,
    /// `over`.
    Overlay = 10,
    /// `sLit`.
    SoftLight = 11,
    /// `hLit`.
    HardLight = 12,
    /// `vLit`.
    VividLight = 13,
    /// `lLit`.
    LinearLight = 14,
    /// `pLit`.
    PinLight = 15,
    /// `hMix`.
    HardMix = 16,
    /// `diff`.
    Difference = 17,
    /// `smud`.
    Exclusion = 18,
    /// `hue `.
    Hue = 19,
    /// `sat `.
    Saturation = 20,
    /// `colr`.
    Color = 21,
    /// `lum `.
    Luminosity = 22,
    /// `pass`.
    PassThrough = 23,
    /// `dkCl`.
    DarkerColor = 24,
    /// `ltCl`.
    LighterColor = 25,
    /// `fsub`.
    Subtract = 26,
    /// `fdiv`.
    Divide = 27,
}

impl BlendMode {
    /// Map a 4-byte blend key to its mode.
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

    /// The blend mode as the integer the plugin surface reports.
    pub fn as_i32(self) -> i32 {
        self as i32
    }
}

/// How a layer image should be assembled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageMode {
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
pub struct Header {
    /// The channel count of the merged image.
    pub channels: u16,
    /// The document height in pixels.
    pub height: i32,
    /// The document width in pixels.
    pub width: i32,
    /// Bits per channel (1, 8, 16 or 32).
    pub depth: u16,
    /// The colour mode; see the `COLOR_MODE_*` constants.
    pub mode: u16,
}

/// One layer channel's compressed payload.
#[derive(Debug, Clone)]
pub struct ChannelInfo {
    /// The channel id: 0/1/2 colour, -1 alpha, -2/-3 mask.
    pub id: i16,
    /// The declared byte length of the channel data, compression id included.
    pub length: i32,
    /// The compression id (0 raw, 1 PackBits, 2 zip, 3 zip with prediction).
    pub compression: u16,
    /// The bytes after the 2-byte compression id.
    pub data: Vec<u8>,
}

impl ChannelInfo {
    /// Whether this channel is a user or real-user mask channel.
    pub fn is_mask_channel(&self) -> bool {
        self.id == -2 || self.id == -3
    }
}

/// A layer mask rectangle and its flags.
#[derive(Debug, Clone, Copy, Default)]
pub struct LayerMask {
    /// The mask width in pixels.
    pub width: i32,
    /// The mask height in pixels.
    pub height: i32,
    /// The mask top edge.
    pub top: i32,
    /// The mask left edge.
    pub left: i32,
    /// The mask bottom edge.
    pub bottom: i32,
    /// The mask right edge.
    pub right: i32,
    /// The colour outside the mask (0 or 255).
    pub default_color: i32,
    /// The mask flags.
    pub flags: i32,
    /// The real-user-mask flags.
    pub real_flags: i32,
    /// The real-user-mask background byte.
    pub real_user_mask_background: i32,
    /// The enclosing-rectangle top edge.
    pub enclosing_top: i32,
    /// The enclosing-rectangle left edge.
    pub enclosing_left: i32,
    /// The enclosing-rectangle bottom edge.
    pub enclosing_bottom: i32,
    /// The enclosing-rectangle right edge.
    pub enclosing_right: i32,
}

/// One additional layer info block.
#[derive(Debug, Clone)]
pub struct AdditionalLayer {
    /// The signature kind: 0 for `8BIM`, 1 for `8B64`.
    pub sig_type: i32,
    /// The 4-byte info key.
    pub key: [u8; 4],
    /// The raw block data.
    pub data: Vec<u8>,
}

/// A layer's parsed extra data block.
#[derive(Debug, Clone, Default)]
pub struct LayerExtra {
    /// The layer mask record.
    pub layer_mask: LayerMask,
    /// The legacy layer name stored in the extra block.
    pub layer_name: String,
    /// The additional layer info blocks, in file order.
    pub additional: Vec<AdditionalLayer>,
}

/// Per-layer layer-comp offset and visibility state.
#[derive(Debug, Clone, Default)]
pub struct LayerCompInfo {
    /// The layer comp id.
    pub id: i32,
    /// The horizontal offset recorded by the comp.
    pub offset_x: i32,
    /// The vertical offset recorded by the comp.
    pub offset_y: i32,
    /// Whether the layer is enabled in the comp.
    pub is_enabled: bool,
}

/// One Photoshop layer.
#[derive(Debug, Clone)]
pub struct LayerInfo {
    /// The layer width in pixels.
    pub width: i32,
    /// The layer height in pixels.
    pub height: i32,
    /// The layer top edge.
    pub top: i32,
    /// The layer left edge.
    pub left: i32,
    /// The layer bottom edge.
    pub bottom: i32,
    /// The layer right edge.
    pub right: i32,
    /// The channel records, in file order.
    pub channels: Vec<ChannelInfo>,
    /// The blend mode.
    pub blend_mode: BlendMode,
    /// The layer opacity (0..=255).
    pub opacity: u8,
    /// The fill opacity (0..=255, or `-1` when the block is short).
    pub fill_opacity: i32,
    /// The clipping flags.
    pub clipping: u8,
    /// The layer flags.
    pub flag: u8,
    /// The parsed extra data block.
    pub extra_data: LayerExtra,
    /// The layer id from the `lyid` block, or `-1`.
    pub layer_id: i32,
    /// The layer kind.
    pub layer_type: LayerType,
    /// The layer name from the extra block.
    pub layer_name: String,
    /// The unicode layer name from the `luni` block.
    pub layer_name_unicode: String,
    /// The per-comp state recorded in the `shmd` metadata.
    pub layer_comps: BTreeMap<i32, LayerCompInfo>,
    /// Index into [`PsdFile::layers`] of the enclosing folder, if any.
    pub parent: Option<usize>,
}

impl LayerInfo {
    /// Whether the layer is visible.
    pub fn is_visible(&self) -> bool {
        self.flag & (1 << 1) == 0
    }

    /// Whether the layer protects its transparency.
    pub fn is_transparency_protected(&self) -> bool {
        self.flag & (1 << 0) != 0
    }

    /// Whether the layer record is obsolete.
    pub fn is_obsolete(&self) -> bool {
        self.flag & (1 << 2) != 0
    }

    /// Whether the layer carries no pixel data of its own.
    pub fn is_pixel_data_irrelevant(&self) -> bool {
        self.flag & (1 << 4) != 0
    }
}

/// One image resource block.
#[derive(Debug, Clone)]
pub struct ImageResource {
    /// The resource id.
    pub id: u16,
    /// The raw resource data.
    pub data: Vec<u8>,
}

/// A parsed slice item.
#[derive(Debug, Clone, Default)]
pub struct SliceItem {
    /// The slice id.
    pub id: i32,
    /// The group this slice belongs to.
    pub group_id: i32,
    /// The slice origin (0 layer-based, 1 user-based).
    pub origin: i32,
    /// The layer id a layer-based slice is attached to.
    pub associated_layer_id: i32,
    /// The slice name.
    pub name: String,
    /// The slice type.
    pub slice_type: i32,
    /// The slice left edge.
    pub left: i32,
    /// The slice top edge.
    pub top: i32,
    /// The slice right edge.
    pub right: i32,
    /// The slice bottom edge.
    pub bottom: i32,
    /// The slice URL.
    pub url: String,
    /// The link target.
    pub target: String,
    /// The rollover message.
    pub message: String,
    /// The alternate tag text.
    pub alt_tag: String,
    /// Whether the cell text is HTML.
    pub cell_text_is_html: bool,
    /// The cell text.
    pub cell_text: String,
    /// The horizontal alignment code.
    pub horizontal_align: i32,
    /// The vertical alignment code.
    pub vertical_align: i32,
    /// The slice colour alpha byte.
    pub color_a: u8,
    /// The slice colour red byte.
    pub color_r: u8,
    /// The slice colour green byte.
    pub color_g: u8,
    /// The slice colour blue byte.
    pub color_b: u8,
}

/// The parsed slice resource.
#[derive(Debug, Clone, Default)]
pub struct SliceResource {
    /// The bounding rectangle's left edge.
    pub bounding_left: i32,
    /// The bounding rectangle's top edge.
    pub bounding_top: i32,
    /// The bounding rectangle's right edge.
    pub bounding_right: i32,
    /// The bounding rectangle's bottom edge.
    pub bounding_bottom: i32,
    /// The slice group name.
    pub group_name: String,
    /// The slices in the group.
    pub slices: Vec<SliceItem>,
}

/// One guide.
#[derive(Debug, Clone, Copy)]
pub struct GuideItem {
    /// The guide position.
    pub location: i32,
    /// The guide direction: 0 vertical, 1 horizontal (`-1` on a short read).
    pub direction: i32,
}

/// The parsed grid and guide resource.
#[derive(Debug, Clone, Default)]
pub struct GridGuideResource {
    /// The horizontal grid spacing.
    pub horizontal_grid: i32,
    /// The vertical grid spacing.
    pub vertical_grid: i32,
    /// The guides in the document.
    pub guides: Vec<GuideItem>,
}

/// The indexed-colour table.
#[derive(Debug, Clone)]
pub struct ColorTable {
    /// The 256 RGBA palette entries.
    pub colors: Vec<[u8; 4]>,
    /// The transparent palette index, or `-1`.
    pub transparency_index: i16,
    /// The number of valid palette entries.
    pub valid_count: i16,
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

/// A document-level layer comp.
#[derive(Debug, Clone, Default)]
pub struct LayerComp {
    /// The comp id.
    pub id: i32,
    /// Whether the comp records layer visibility.
    pub record_visibility: bool,
    /// Whether the comp records layer position.
    pub record_position: bool,
    /// Whether the comp records layer appearance.
    pub record_appearance: bool,
    /// The comp name.
    pub name: String,
    /// The comp comment.
    pub comment: String,
}

/// The complete parsed PSD document.
#[derive(Debug, Clone)]
pub struct PsdFile {
    /// The file header.
    pub header: Header,
    /// The layer records, in file order.
    pub layers: Vec<LayerInfo>,
    /// The slice resource, if the file carries one.
    pub slice: Option<SliceResource>,
    /// The grid and guide resource, if the file carries one.
    pub grid_guide: Option<GridGuideResource>,
    /// The indexed-colour table.
    pub color_table: ColorTable,
    /// The document-level layer comps.
    pub layer_comps: Vec<LayerComp>,
    /// The comp id the document was last saved with, or `-1`.
    pub last_applied_comp_id: i32,
    /// The merged composite image (compression id and body), if present.
    pub image_data: Option<Vec<u8>>,
}

/// Bitmap colour mode.
pub const COLOR_MODE_BITMAP: u16 = 0;
/// Grayscale colour mode.
pub const COLOR_MODE_GRAYSCALE: u16 = 1;
/// Indexed colour mode.
pub const COLOR_MODE_INDEXED: u16 = 2;
/// RGB colour mode.
pub const COLOR_MODE_RGB: u16 = 3;
/// CMYK colour mode.
pub const COLOR_MODE_CMYK: u16 = 4;
/// Multichannel colour mode.
pub const COLOR_MODE_MULTICHANNEL: u16 = 7;
/// Duotone colour mode.
pub const COLOR_MODE_DUOTONE: u16 = 8;
/// Lab colour mode.
pub const COLOR_MODE_LAB: u16 = 9;

// ---------------------------------------------------------------------------
// Top-level parse
// ---------------------------------------------------------------------------

/// Parse a complete PSD document.
pub fn parse(bytes: &[u8]) -> PsdResult<PsdFile> {
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
    // structure at all, so an empty section is a successful parse with no
    // layers rather than a truncated record.
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
    /// Second pass: expand the resources and the per-layer additional info
    /// into the typed structures.
    fn process(&mut self, resources: &[ImageResource], color_mode: &[u8]) {
        for resource in resources {
            match resource.id {
                1032 => self.load_grid_and_guide(&resource.data),
                1050 => self.load_slice(&resource.data),
                1046 => {
                    // The palette count is read through the short-read
                    // sentinels, so a short block is `-1`.
                    let mut r = Reader::new(&resource.data);
                    self.color_table.valid_count = r.i16_ref();
                }
                1047 => {
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

        // Link each layer to its enclosing folder: walk from the topmost
        // record downwards, pushing on a folder opener and popping on its
        // hidden end marker.
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
        // The version is read and discarded, every field comes from the
        // short-read sentinels, a non-positive guide count yields no guides,
        // and the resource is enabled as soon as it is seen.
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
        // The version-6 fields all come from the short-read sentinels, so a
        // short block yields `-1`/empty instead of aborting, and the resource
        // is enabled for any version-6 block. The `uint8_t` colour fields
        // store the byte sentinel, so a short read is `0xff`.
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
        // Versions 7 and 8 are descriptor-based; their payload is consumed but
        // not interpreted.
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
        self.last_applied_comp_id = descriptor.integer("lastAppliedComp").map_or(-1, |id| id);
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

fn parse_image_resources(bytes: &[u8]) -> PsdResult<Vec<ImageResource>> {
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

fn parse_layer_and_mask(bytes: &[u8]) -> PsdResult<Vec<LayerInfo>> {
    let mut r = Reader::new(bytes);

    let layer_info_len = r.u32()? as usize;
    let mut layers = Vec::new();
    if layer_info_len > 0 {
        let layer_info = r.take(layer_info_len)?;
        layers = parse_layer_info(layer_info)?;
    }

    // Global layer mask info: a size-prefixed block that is skipped; the
    // plugin exposes none of its fields.
    if !r.eof() {
        let size = r.u32()? as usize;
        r.skip(size)?;
    }

    // An optional additional layer information block (`Lr16` / `Lr32`) repeats
    // a layer info block; it is consumed and discarded.
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

fn parse_layer_info(bytes: &[u8]) -> PsdResult<Vec<LayerInfo>> {
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

fn parse_layer_extra(bytes: &[u8]) -> PsdResult<LayerExtra> {
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
    // A block shorter than the 19-byte base record leaves the
    // zero-initialised mask; the extra fields need the full 37-byte record.
    // Only `top`, `left`, `bottom`, `right` and `default_color` are part of
    // the plugin surface.
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
            let mut r = Reader::new(&additional.data);
            layer.layer_name_unicode = r.unicode_string_ref();
        }
        b"lyid" => {
            // A short read yields `-1`.
            let mut r = Reader::new(&additional.data);
            layer.layer_id = r.i32_ref();
        }
        b"shmd" => load_layer_metadata(layer, additional),
        b"iOpa" => {
            // A short read yields `-1`, which is stored as-is.
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
        let Some(id) = comp_list.first().and_then(Value::as_integer) else {
            continue;
        };
        let mut info = LayerCompInfo {
            id,
            ..Default::default()
        };
        if let Some(offset) = comp.descriptor("Ofst") {
            // Both axes read the horizontal key.
            info.offset_x = offset.integer("Hrzn").map_or(0, |value| value);
            info.offset_y = offset.integer("Hrzn").map_or(0, |value| value);
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

    /// A short read yields the format's sentinels (`-1`, an empty string), not
    /// a zero default
    /// (`reference/cpp/plugins/psdfile/psdparse/psdparse.h:101-187`).
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

    /// A layer-mask block shorter than the 19-byte base record leaves the
    /// zero-initialised mask
    /// (`reference/cpp/plugins/psdfile/psdparse/psdparse.h` layer-mask
    /// grammar).
    #[test]
    fn short_layer_mask_stays_zero() {
        let mask = parse_layer_mask(&[0u8; 4]);
        assert_eq!(mask.top, 0);
        assert_eq!(mask.default_color, 0);
    }
}
