//! The layer-system state model.
//!
//! This is the state half of the middle layer: the logical `Scene` value and
//! its windows, layers, bitmaps and fonts, plus every CPU operation on them.
//! The caller owns the [`Scene`]; there are no globals, no locks and no
//! hidden caches. Everything is id-based, and operations on an unknown id are
//! no-ops.
//!
//! The module is deliberately backend-free and Bevy-free. It knows nothing
//! about drawing, script objects or native registration: the L1
//! `RenderPipeline` consumes a projected scene tree elsewhere, and the L3
//! `compat` native adapter calls into these operations. The one renderer
//! object it names is the effect pipeline handle a layer's custom-shader draw
//! binds ([`Scene::effect_pipelines`]); the handle is a value the projection
//! passes on, not a service the scene calls.

use std::sync::Arc;

use api::blend::BlendMode;
use api::config::CacheLimit;
use api::geometry::PixelRegion;
use api::media::DecodedImage;

use crate::hasher::{Map, Set, map, set};
use crate::imagedecode::{ImageOutcome, ImageSizeError, PendingImage};
use crate::shader::{EffectDraw, EffectPipelines};
use crate::trans::Trans;

/// One logical window.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct WindowState {
    /// Unique window id, stable for the lifetime of the scene.
    pub id: u32,
    /// Human-readable window title (used for the host window title).
    pub title: String,
    /// Window client size in pixels (`Window.width`/`innerWidth`). It is not
    /// the logical screen: that is the primary layer's rect.
    pub inner_size: (u32, u32),
    /// Whether the host shows the window.
    pub visible: bool,
    /// Window opacity in `0.0..=1.0`.
    pub opacity: f32,
    /// The layer scripts use as `window.primaryLayer`.
    pub primary_layer: Option<u32>,
    /// The focused layer of the window's **input domain** — the domain a
    /// window-level focus read/write and all input events bind to. It is the
    /// focus of [`Self::focus_domains`]'s first entry, kept in step by
    /// [`WindowState::sync_focused_layer`].
    pub focused_layer: Option<u32>,
    /// Focus domain ids in registration order: one per parentless root
    /// layer, in the order the roots were created. Index 0 is the window's
    /// input domain (`reference/cpp/core/visual/LayerTreeOwnerImpl.h:21`
    /// `PrimaryLayerManagerIndex`, pushed in creation order by
    /// `RegisterLayerManager`, `reference/cpp/core/visual/LayerTreeOwnerImpl.cpp:93-98`).
    pub focus_domains: Vec<u32>,
    /// The focused layer of each registered domain, when one is set. A
    /// domain absent from the map has no focus, exactly like a manager whose
    /// `FocusedLayer` is null (`reference/cpp/core/visual/LayerManager.h:483-490`).
    pub domain_focus: Map<u32, u32>,
    /// Render order, back -> front.
    pub layers: Vec<u32>,
}

impl WindowState {
    /// The domain the window's input and `focusedLayer` reads/writes bind
    /// to: the first-registered root's domain. `None` while the window has
    /// no parentless layer, like the reference's null `GetLayerManagerAt(0)`
    /// (`reference/cpp/core/visual/LayerTreeOwnerImpl.cpp:24-28`).
    fn input_domain(&self) -> Option<u32> {
        self.focus_domains.first().copied()
    }

    /// Refresh [`Self::focused_layer`] from the input domain's slot. Every
    /// focus mutation goes through here so the window-level mirror and the
    /// per-domain map cannot disagree.
    fn sync_focused_layer(&mut self) {
        self.focused_layer = self
            .input_domain()
            .and_then(|domain| self.domain_focus.get(&domain).copied());
    }
}

/// Clamp a `u32` pixel dimension to a non-negative `i32` (a plain `as i32`
/// would wrap for sizes above `i32::MAX`).
fn dim_i32(v: u32) -> i32 {
    v.min(i32::MAX as u32) as i32
}

/// A script colour (`0xAARRGGBB`) to straight-alpha RGBA.
fn argb_to_rgba(color: i64) -> [u8; 4] {
    let value = color as u32;
    [
        (value >> 16) as u8,
        (value >> 8) as u8,
        value as u8,
        (value >> 24) as u8,
    ]
}

/// One logical layer (sprite surface) attached to a window.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct LayerState {
    /// Unique layer id, stable for the lifetime of the scene.
    pub id: u32,
    /// Owning window id.
    pub window: u32,
    /// Parent layer id, or None to attach directly to the window.
    pub parent: Option<u32>,
    /// Focus domain id: the id of the parentless root whose focus manager
    /// this layer belongs to. A parentless layer founds its own domain (its
    /// own id); a child inherits its parent's. Fixed at creation, because
    /// the reference assigns the manager once and never reassigns it
    /// (`reference/cpp/core/visual/LayerIntf.cpp:449-473`); all focus moves
    /// and searches stay inside one domain
    /// (`reference/cpp/core/visual/LayerManager.h:483-490`).
    pub domain: u32,
    /// Direct child layer ids in maintained (back → front sibling) order.
    pub children: Vec<u32>,
    /// Bitmap id shown on this layer, if any.
    pub bitmap: Option<u32>,
    /// Position + size in window coordinates.
    pub rect: Rect,
    /// Whether this layer (ignoring ancestors) is visible.
    pub visible: bool,
    /// Layer opacity in `0.0..=1.0`.
    pub opacity: f32,
    /// Generic blend behaviour ([`BlendMode::SourceOver`] by default). This is
    /// the render pipeline's complete blend vocabulary, so the `RenderScene`
    /// projection does not lose `Layer.type` updates. L3 owns the native
    /// `tTVPLayerType` numbering and maps it onto these modes.
    pub blend: BlendMode,
    /// Sibling sort key: the sibling position in relative order mode, or the
    /// stored absolute index while the parent is in absolute order mode.
    pub z_order: i32,
    /// Image placement inside the layer (reference `ImageLeft`/`ImageTop`,
    /// typically ≤ 0) and the drawn image size (`ImageWidth`/`ImageHeight`,
    /// may exceed the layer rect — the layer clips it). The game's buttons
    /// use a sprite sheet here: `setImagePos(-frameW*n, 0)` selects a frame.
    pub image_left: i32,
    /// Reference `ImageTop`; see `image_left`.
    pub image_top: i32,
    /// Reference `ImageWidth` (drawn image width; may exceed the layer rect).
    pub image_width: u32,
    /// Reference `ImageHeight` (drawn image height).
    pub image_height: u32,
    /// Reference `ProvinceImage` (`LayerIntf.cpp:2359` `AllocateProvinceImage`):
    /// an 8-bit plane (`province_width * province_height` bytes, one byte per
    /// pixel) backing `getProvincePixel`/`setProvincePixel` and the
    /// `provinceImageBuffer*` properties. `None` until `setProvincePixel` or a
    /// `provinceImageBufferForWrite` read allocates it. The plane is layer
    /// owned (the reference allocates a fresh `tTVPBaseBitmap(..., 8)`), so it
    /// is not shared through `bitmap`.
    pub province: Option<Box<[u8]>>,
    /// Province plane width in pixels (0 when absent).
    pub province_width: u32,
    /// Province plane height in pixels (0 when absent).
    pub province_height: u32,
    /// Alpha threshold for `htMask` hit-testing; `≤0` accepts any pixel.
    pub hit_threshold: i32,
    /// Hit-test mode (reference `tTVPHitType`): `htMask=0` (per-pixel
    /// threshold) or `htProvince=1` (non-transparent province). Stored so
    /// `SelectItemBase` can set it; the render input bridge and
    /// [`Scene::layer_at`] read it during hit-testing.
    pub hit_type: i32,
    /// Mouse-cursor id (reference `tTVPCursorType`, e.g. `crDefault=0`,
    /// `crHandPoint=-21`). Stored; the desktop cursor is not yet driven.
    pub cursor: i32,
    /// Draw face (`dfBoth=0`, `dfMain=1`, `dfMask=2`, `dfProvince=3`,
    /// `dfAddAlpha=4`). Stored; the projection always draws the main image.
    pub face: i32,
    /// Reference `ClipRect` set by `Layer.setClip(left, top, width, height)`:
    /// a half-open `[x, x+w) x [y, y+h)` region in image pixels. `None` means
    /// the whole main image (the reference resets/initializes it to the image
    /// size). Pixel operations (`colorRect`, `colorize`, `noise`, ...) clip
    /// against it exactly like the reference.
    pub clip: Option<Rect>,
    /// `holdAlpha` — keep the alpha when drawing. Stored only for now.
    pub hold_alpha: bool,
    /// Reference `Focusable`: whether the layer may receive focus.
    pub focusable: bool,
    /// Reference `JoinFocusChain` (`LayerIntf.h:527`, default true): whether
    /// the layer joins the window's focus chain (`nextFocusable`/
    /// `prevFocusable`).
    pub join_focus_chain: bool,
    /// Reference `FocusWork` (`LayerIntf.h:528`): the transient layer selected
    /// by the last `nextFocusable`/`prevFocusable` search. The native
    /// `onSearch*Focusable`/`onBeforeFocus` handlers may redirect it.
    pub focus_work: Option<u32>,
    /// Reference `Enabled`: whether the layer's input/attention is active.
    pub enabled: bool,
    /// Reference `Name` (script `layer.name`).
    pub name: String,
    /// Reference `AttentionLeft`/`AttentionTop` (attention anchor point).
    pub attention_left: i32,
    /// Reference `AttentionTop`; see `attention_left`.
    pub attention_top: i32,
    /// Reference `UseAttention`: whether the layer participates in the
    /// attention/hint system.
    pub use_attention: bool,
    /// Reference `Cached`: request the layer be rendered to a cache.
    pub cached: bool,
    /// Reference `CacheEnabledCount` (`LayerIntf.h:722`): the number of
    /// active consumers that require this layer's whole subtree rendered
    /// into [`Self::cache_bitmap`]. Nonzero makes the projection draw the
    /// cache in place of the layer's own image and descendants.
    pub cache_enabled_count: u32,
    /// The bitmap a whole-subtree transition draws its frame into. `None`
    /// until the transition first needs one; freed with the last
    /// [`Scene::dec_cache_enabled_count`]. It is distinct from
    /// [`Self::cache_content`] so the blend never overwrites the content it
    /// reads.
    pub cache_bitmap: Option<u32>,
    /// The composited subtree content while `cache_enabled_count` is nonzero
    /// (the reference `CacheBitmap` returned by `Complete`). `None` until
    /// the layer first needs one; freed with the last
    /// [`Scene::dec_cache_enabled_count`].
    pub cache_content: Option<u32>,
    /// The image drawn in place of this layer's own image while a transition
    /// blends it with another layer's image without consuming the subtree.
    /// The projection draws it at the main image's placement and the layer's
    /// children keep drawing live, so it is never part of a cached subtree.
    /// `None` until the transition first needs one; the transition clears it
    /// when it ends ([`Scene::set_transition_self_image`]).
    pub transition_self: Option<u32>,
    /// The frame a running transition draws, when the host composites it
    /// instead of blending [`Self::cache_bitmap`] or [`Self::transition_self`]
    /// pixel by pixel. [`Scene::composite_transition_frames`] selects which of
    /// the two carries the frame; the projection draws the two source images
    /// this describes in place of the layer's image. `None` while no
    /// composited transition runs ([`Scene::set_transition_frame`]).
    pub transition_frame: Option<TransitionFrame>,
    /// The custom-shader draw a running transition composites, when the
    /// backend can run effect pipelines. The projection draws its passes in
    /// place of the layer's image; the layer keeps its bitmap untouched, so
    /// the transition costs no per-tick pixel work. `None` while no such
    /// transition runs ([`Scene::set_effect`]).
    pub effect: Option<EffectDraw>,
    /// Reference `CacheRecalcRegion`: the subtree content must be
    /// recomposited before it is used again. Set for this layer and every
    /// caching ancestor whenever the layer changes, and cleared when the
    /// content is recomposited.
    pub cache_recalc: bool,
    /// Sum of the source images' [`BitmapState::content_generation`] when
    /// [`Self::cache_content`] was last composited. A change to a drawn
    /// image that did not go through a layer accessor is detected by
    /// comparing this value with the current sum.
    pub cache_source_generation: u64,
    /// Reference `Hint` (script `layer.hint`) and the hint-system flags.
    pub hint: String,
    /// Reference `ShowParentHint`: inherit the parent's hint.
    pub show_parent_hint: bool,
    /// Reference `IgnoreHintSensing`: opt out of hint sensing.
    pub ignore_hint_sensing: bool,
    /// Reference `ImeMode` (stored) and `NeutralColor` (ARGB, stored).
    pub ime_mode: i32,
    /// Reference `NeutralColor` (ARGB, stored).
    pub neutral_color: i64,
    /// Whether this layer orders its children by their stored absolute index
    /// (`absolute`) instead of their maintained sibling position.
    pub absolute_order_mode: bool,
    /// Reference `CallOnPaint`; see `pending_paint`.
    pub call_on_paint: bool,
    /// Reference `HoldAlpha`-adjacent `ImageModified` flag: set by pixel
    /// operations, clearable by script.
    pub image_modified: bool,
    /// Font id (into [`Scene::fonts`]) backing `layer.font`, or `None` until
    /// the script reads `layer.font` (which lazily allocates one). `drawText`
    /// resolves the requested face from here.
    pub font_id: Option<u32>,
    /// Whether this layer is its window's `primary_layer`.
    pub is_primary: bool,
    /// Reference `tTJSNI_BaseLayer::CallOnPaint`: set by the script-visible
    /// `update()` (and `loadImages`/`setSize*`, which call it) and cleared by
    /// the engine once it dispatches the layer's `onPaint` handler. The
    /// game's `AffineLayer.onPaint` copies its hidden inner `_image` layer's
    /// bitmap onto the visible outer layer, so this flag is what makes the
    /// intro logo and every `Sprite`/`AffineLayer` render.
    pub pending_paint: bool,
    /// Reference `tTJSNI_BaseLayer::Shutdown` (`LayerIntf.cpp:516`). A shut-down
    /// layer is inert — hit-testing, input dispatch, `onPaint` polling and
    /// rendering must not touch it.
    ///
    /// [`Scene::invalidate_layer`] marks the layer here **before** unregistering
    /// it (the record then leaves [`Scene::layers`] immediately, so a stale id
    /// becomes a typed miss); the flag also survives on any record a caller
    /// marks directly, and every consumer — [`Scene::node_visible`],
    /// [`Scene::layer_at`], [`Scene::window_layer_order`] — skips it.
    pub shutdown: bool,
}

impl LayerState {
    /// The image this layer draws in place of its main image: the transition
    /// self image while a transition blends it with another layer's image,
    /// otherwise the main image.
    pub fn drawn_image(&self) -> Option<u32> {
        self.transition_self.or(self.bitmap)
    }
}

/// One source image of a [`TransitionFrame`].
///
/// The rectangles are in the frame's pixel space: `dst` is the rectangle the
/// source occupies inside the frame and `src` is the sub-rectangle of
/// `bitmap` it samples. The two have the same size.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct TransitionSource {
    /// Bitmap the source samples.
    pub bitmap: u32,
    /// Sub-rectangle of `bitmap` to sample, in bitmap pixels.
    pub src: Rect,
    /// Rectangle the source occupies inside the frame, in frame pixels.
    pub dst: Rect,
    /// Opacity of the source inside the frame, `0.0..=1.0`. A source with
    /// zero opacity draws nothing; a source at full opacity is an independent
    /// placement rather than a fade over the source drawn before it.
    pub alpha: f32,
}

impl TransitionSource {
    /// Builds one source draw of a transition frame.
    pub fn new(bitmap: u32, src: Rect, dst: Rect, alpha: f32) -> Self {
        Self {
            bitmap,
            src,
            dst,
            alpha,
        }
    }
}

/// The current frame of a running transition, drawn by the projection in
/// place of a display bitmap.
///
/// The frame composites two source images: [`Self::src1`] is drawn first and
/// [`Self::src2`] over it at its own alpha. A whole-subtree transition covers
/// the layer's rect; a transition without children stands in for the layer's
/// own image at the main image's placement.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct TransitionFrame {
    /// Frame size in pixels — the coordinate space of both sources' `dst`.
    pub size: (u32, u32),
    /// Whether the frame covers the layer's whole rect instead of being placed
    /// like the layer's own image.
    pub covers_layer: bool,
    /// The image the frame starts from.
    pub src1: TransitionSource,
    /// The image composited over [`Self::src1`] as the transition advances.
    pub src2: TransitionSource,
}

impl TransitionFrame {
    /// Builds a transition frame of `size` pixels.
    pub fn new(
        size: (u32, u32),
        covers_layer: bool,
        src1: TransitionSource,
        src2: TransitionSource,
    ) -> Self {
        Self {
            size,
            covers_layer,
            src1,
            src2,
        }
    }
}

/// One decoded bitmap (RGBA8, straight alpha).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct BitmapState {
    /// Unique bitmap id, stable for the lifetime of the scene.
    pub id: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Straight-alpha RGBA8 pixels, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
    /// Storage name it was loaded from, if any.
    pub name: Option<String>,
    /// Set when contents changed; the L1 pipeline re-uploads on true.
    pub dirty: bool,
    /// The part of the bitmap that changed since the last upload.
    ///
    /// `None` means every pixel changed and the upload sends them all;
    /// `Some(region)` means only that rectangle changed, and the upload
    /// sends its pixels alone. The region matters only while the bitmap is
    /// dirty.
    pub dirty_region: Option<PixelRegion>,
    /// True when a script `Bitmap` object owns this id (created through the
    /// `Bitmap` natives). A layer's MainImage can reference such a bitmap, so
    /// destroying the layer must not free it while the script object can
    /// still read it.
    pub script_owned: bool,
    /// Monotonic pixel-change counter, bumped whenever the pixels may have
    /// changed (a mutable access or [`BitmapState::mark_dirty`]). A
    /// composited subtree cache records the sum over its source images so an
    /// image edit that did not go through a layer accessor still invalidates
    /// the cache.
    pub content_generation: u64,
    /// The decode whose pixels have not landed yet, or `None` when the
    /// bitmap holds its pixels.
    ///
    /// A pending bitmap carries the probed dimensions and no pixels: every
    /// read of the pixels waits for the decode through
    /// [`Scene::resolve_bitmap`].
    pub pending: Option<PendingImage>,
}

impl BitmapState {
    /// Create an owned bitmap with the given dimensions and straight-alpha
    /// RGBA8 pixels. Starts clean, unnamed and script-owned by the caller.
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        Self {
            id: 0,
            width,
            height,
            rgba,
            name: None,
            dirty: false,
            dirty_region: None,
            script_owned: false,
            content_generation: 0,
            pending: None,
        }
    }

    /// Mark pixels as changed. Native drawing operations use this instead of
    /// relying on a renderer-specific upload mechanism.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
        self.dirty_region = None;
        self.content_generation = self.content_generation.wrapping_add(1);
    }

    /// Mark a rectangle of the bitmap as changed.
    ///
    /// The rectangle is clipped to the bitmap's bounds; one that covers no
    /// pixel marks nothing. The marked region accumulates: a second
    /// rectangle widens it to the union of the two. A bitmap that is already
    /// fully dirty stays fully dirty, and a clean bitmap starts a new
    /// region with this rectangle.
    pub fn mark_dirty_rect(&mut self, rect: PixelRegion) {
        let whole = PixelRegion::new(0, 0, self.width, self.height);
        let Some(clipped) = rect.intersection(&whole) else {
            return;
        };
        let region = match self.dirty_region {
            Some(current) => current.union(&clipped),
            // A clean bitmap has no region yet, so the rectangle starts one.
            None if !self.dirty => clipped,
            // A dirty bitmap without a region is fully dirty.
            None => whole,
        };
        self.dirty = true;
        // A region that covers the whole bitmap is the same as no region.
        self.dirty_region = if region == whole { None } else { Some(region) };
        self.content_generation = self.content_generation.wrapping_add(1);
    }

    /// Return the byte offset of a pixel, or `None` outside the bitmap or
    /// when the offset is not representable.
    pub fn pixel_offset(&self, x: u32, y: u32) -> Option<usize> {
        if x >= self.width || y >= self.height {
            return None;
        }
        usize::try_from(y)
            .ok()?
            .checked_mul(usize::try_from(self.width).ok()?)?
            .checked_add(usize::try_from(x).ok()?)?
            .checked_mul(4)
    }
}

/// The number of bytes an image of `width` x `height` RGBA8 pixels holds.
///
/// `None` when the count is not representable, so a caller can refuse the
/// size instead of wrapping it.
fn image_pixel_bytes(width: u32, height: u32) -> Option<usize> {
    usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)
        .and_then(|pixels| pixels.checked_mul(4))
}

/// Whether a bitmap's buffer covers every pixel its declared size names.
///
/// An empty buffer is never an addressable image: `Vec::new().as_ptr()` is a
/// dangling one-byte pointer, so a bitmap that holds no pixel has no address.
pub(crate) fn bitmap_holds_pixels(bitmap: &BitmapState) -> bool {
    image_pixel_bytes(bitmap.width, bitmap.height)
        .is_some_and(|bytes| bytes > 0 && bytes <= bitmap.rgba.len())
}

/// Byte offset of `(x, y)` in a plane whose rows are `pitch` bytes wide.
///
/// `None` when the row computation is not representable, so a declared plane
/// size can never wrap its way into a valid index.
fn plane_offset(pitch: u32, x: u32, y: u32) -> Option<usize> {
    usize::try_from(y)
        .ok()?
        .checked_mul(usize::try_from(pitch).ok()?)?
        .checked_add(usize::try_from(x).ok()?)
}

/// A fully transparent image of `width` x `height` pixels.
///
/// It stands in for an image whose decode failed or was cancelled: the
/// bitmap keeps the size the header probe reported, and every pixel is
/// transparent. `None` when the image cannot be allocated.
fn blank_transparent(width: u32, height: u32) -> Option<Vec<u8>> {
    let count = image_pixel_bytes(width, height)?;
    let mut blank = Vec::new();
    if blank.try_reserve_exact(count).is_err() {
        return None;
    }
    blank.resize(count, 0);
    Some(blank)
}

/// Installs a blank transparent image of the bitmap's own size.
///
/// The bitmap keeps its size and its pixels become transparent. A blank that
/// cannot be allocated leaves the bitmap without pixels, which the frame's
/// upload reports as an inconsistent bitmap.
fn install_blank(bitmap: &mut BitmapState) {
    let width = bitmap.width;
    let height = bitmap.height;
    match blank_transparent(width, height) {
        Some(blank) => bitmap.rgba = blank,
        None => {
            log::error!("engine: a blank {width}x{height} image could not be allocated");
            bitmap.rgba = Vec::new();
        }
    }
}

/// One font face used by `Layer.drawText` (rasterized by the L1 `FontSystem`).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct FontState {
    /// Unique font id, stable for the lifetime of the scene.
    pub id: u32,
    /// Requested face/family name.
    pub face: String,
    /// Requested height in pixels.
    pub height: i32,
    /// Text color (RGBA, straight alpha).
    pub color: [u8; 4],
    /// Bold style flag.
    pub bold: bool,
    /// Italic style flag.
    pub italic: bool,
    /// Strikeout style flag.
    pub strikeout: bool,
    /// Underline style flag.
    pub underline: bool,
    /// Rotation in the reference's tenths-of-a-degree units (game code does
    /// `font.angle \ 10`).
    pub angle: f64,
}

/// Rectangle in window coordinates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Rect {
    /// Left edge in window coordinates.
    pub x: i32,
    /// Top edge in window coordinates.
    pub y: i32,
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
}

impl Rect {
    /// Create a rectangle from its edges and size, in window coordinates.
    pub fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }
}

/// The image-load variant a [`DecodedImageKey`] identifies.
///
/// The reference's `tTVPGraphicsSearchData` carries a `tTVPGraphicLoadMode`
/// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1257-1264`); the port
/// performs two of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DecodedImageMode {
    /// A full-colour RGBA load.
    Full,
    /// A province plane read from the decoded image's red channel.
    Province,
}

/// The key identifying one entry in the scene's decoded-image cache.
///
/// Mirrors the reference `tTVPGraphicsSearchData {Name, KeyIdx, Mode, DesW,
/// DesH}` (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1257-1264`); the
/// name is normalized by [`DecodedImageKey::new`] the same way the storage
/// layer normalizes it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DecodedImageKey {
    /// Normalized storage name.
    pub name: String,
    /// Colour-key index.
    pub color_key: u32,
    /// Load variant.
    pub mode: DecodedImageMode,
    /// Desired width; `0` keeps the image's own size.
    pub desired_width: u32,
    /// Desired height; `0` keeps the image's own size.
    pub desired_height: u32,
}

impl DecodedImageKey {
    /// Build a key, normalizing `name` with the storage layer's rule
    /// (lowercased, `\` collapsed to `/`).
    pub fn new(
        name: &str,
        color_key: u32,
        mode: DecodedImageMode,
        desired_width: u32,
        desired_height: u32,
    ) -> Self {
        Self {
            name: crate::normalize_in_archive_name(name),
            color_key,
            mode,
            desired_width,
            desired_height,
        }
    }
}

/// The scene's decoded-image cache: the entries, the byte accounting, the
/// least-recently-used order and the byte limit.
///
/// The limit counts the decoded bytes (`Width * Height * PixelSize`,
/// `reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1305-1310`). The cache is
/// disabled by default, matching the reference's default
/// (`:1433-1434`).
#[derive(Debug)]
struct DecodedImageCache {
    /// Cached images by key.
    entries: crate::hasher::Map<DecodedImageKey, DecodedImage>,
    /// Recency order: index 0 is the least recently used, the back is the
    /// most recently used (the reference's additional order, whose `NFirst`
    /// is most recent: `reference/cpp/core/tjs2/tjsHashSearch.h:513-527`).
    order: Vec<DecodedImageKey>,
    /// Sum of the cached images' `rgba` byte counts.
    total_bytes: u64,
    /// Cache bound.
    limit: CacheLimit,
}

impl Default for DecodedImageCache {
    fn default() -> Self {
        Self {
            entries: crate::hasher::map(),
            order: Vec::new(),
            total_bytes: 0,
            limit: CacheLimit::Disabled,
        }
    }
}

impl DecodedImageCache {
    /// Whether the cache stores entries.
    fn enabled(&self) -> bool {
        self.limit.is_enabled()
    }

    /// Look `key` up, mark it most recently used and return an owned copy of
    /// the pixels.
    fn take_copy(&mut self, key: &DecodedImageKey) -> Option<DecodedImage> {
        let image = self.entries.get(key)?.clone();
        self.order.retain(|entry| entry != key);
        self.order.push(key.clone());
        Some(image)
    }

    /// Drop least-recently-used entries until the total is within the limit
    /// (the reference `TVPCheckGraphicCacheLimit`,
    /// `reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1441-1454`).
    fn evict_to_limit(&mut self) {
        let Some(limit) = self.limit.option() else {
            return;
        };
        if limit == 0 {
            return;
        }
        while self.total_bytes > limit {
            let Some(key) = self.order.first().cloned() else {
                break;
            };
            self.order.remove(0);
            if let Some(image) = self.entries.remove(&key) {
                self.total_bytes = self.total_bytes.saturating_sub(image.rgba.len() as u64);
            }
        }
    }

    /// Store `image` under `key`. The limit is checked before the insert, so
    /// the total may exceed it by the new entry's size (the reference
    /// `TVPCheckGraphicCacheLimit` call order,
    /// `reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1952-1960`).
    fn insert(&mut self, key: DecodedImageKey, image: DecodedImage) {
        self.evict_to_limit();
        if let Some(previous) = self.entries.remove(&key) {
            self.total_bytes = self.total_bytes.saturating_sub(previous.rgba.len() as u64);
            self.order.retain(|entry| entry != &key);
        }
        self.total_bytes = self.total_bytes.saturating_add(image.rgba.len() as u64);
        self.entries.insert(key.clone(), image);
        self.order.push(key);
    }
}

/// The whole logical scene.
#[derive(Debug)]
#[non_exhaustive]
pub struct Scene {
    /// All windows, in creation order. Callers may edit this directly (the
    /// id lookups fall back to a linear scan).
    pub windows: Vec<WindowState>,
    /// All layers, in creation order. Callers may edit this directly.
    pub layers: Vec<LayerState>,
    /// All bitmaps, in creation order. Callers may edit this directly.
    pub bitmaps: Vec<BitmapState>,
    /// All fonts, in creation order. Callers may edit this directly.
    pub fonts: Vec<FontState>,
    next_window: u32,
    next_layer: u32,
    next_bitmap: u32,
    next_font: u32,
    /// `window.id` → index into `windows`, for O(1) [`Scene::window`] /
    /// [`Scene::window_mut`]. Repaired on the scene API's own removals and
    /// verified defensively: callers can still mutate the public `windows`
    /// vec directly, in which case the lookup falls back to a linear scan
    /// (and still returns the correct result).
    window_index: crate::hasher::Map<u32, usize>,
    /// `layer.id` → index into `layers`. Rebuilt whenever a layer is
    /// removed; verified defensively like [`Scene::window_index`].
    layer_index: crate::hasher::Map<u32, usize>,
    /// `bitmap.id` → index into `bitmaps`.
    bitmap_index: crate::hasher::Map<u32, usize>,
    /// `font.id` → index into `fonts`.
    font_index: crate::hasher::Map<u32, usize>,
    /// Whether a running transition is drawn from
    /// [`LayerState::transition_frame`] instead of a CPU-blended display
    /// bitmap. A host that draws the projected scene turns it on; the
    /// headless CPU path leaves it off and blends the display bitmap itself.
    pub composite_transition_frames: bool,
    /// The backend handles of the programs the layers' custom-shader draws
    /// name.
    ///
    /// The engine fills the table when it creates the pipelines for the
    /// programs it ships; a table without an entry means the backend cannot
    /// run effect pipelines, and a transition that needs one blends on the CPU.
    pub effect_pipelines: EffectPipelines,
    /// Decoded-image cache (the reference graphic cache).
    image_cache: DecodedImageCache,
    /// Monotonic counter bumped by every change to the tree's structure (see
    /// [`Scene::structure_revision`]).
    structure_revision: u64,
    /// Monotonic counter bumped by every bitmap content change (see
    /// [`Scene::content_revision`]).
    content_revision: u64,
    /// Whether the scene changed since the last presented frame.
    ///
    /// The flag is set by every change that can alter the picture, and
    /// cleared by the engine only after a frame the display accepted: a
    /// frame that was skipped leaves the previous frame's content on screen,
    /// and the change must be drawn again.
    dirty: bool,
}

impl Default for Scene {
    fn default() -> Self {
        Self {
            windows: Vec::new(),
            layers: Vec::new(),
            bitmaps: Vec::new(),
            fonts: Vec::new(),
            next_window: 0,
            next_layer: 0,
            next_bitmap: 0,
            next_font: 0,
            window_index: crate::hasher::map(),
            layer_index: crate::hasher::map(),
            bitmap_index: crate::hasher::map(),
            font_index: crate::hasher::map(),
            composite_transition_frames: false,
            effect_pipelines: EffectPipelines::new(),
            image_cache: DecodedImageCache::default(),
            structure_revision: 0,
            content_revision: 0,
            dirty: false,
        }
    }
}

impl Scene {
    /// Monotonic counter bumped by every change to the tree's structure:
    /// window/layer creation and removal, sibling order, parenting, the
    /// geometry and draw properties the projection reads, and bitmap
    /// creation, removal and resize. The projection compares it across frames
    /// to skip an unchanged rebuild; a bitmap content change does not touch
    /// it. [`Scene::set_bitmap_clean`] deliberately does not bump: clearing a
    /// renderer-internal dirty flag is not a scene change.
    pub fn structure_revision(&self) -> u64 {
        self.structure_revision
    }

    /// Monotonic counter bumped by every bitmap content change, including a
    /// resize; a change to the layer tree does not move it. The subtree-cache
    /// pass compares it (with [`Scene::structure_revision`]) to decide
    /// whether any composited content can be stale.
    pub fn content_revision(&self) -> u64 {
        self.content_revision
    }

    /// Whether the scene changed since the last presented frame.
    ///
    /// A change that can alter the picture sets the flag, so a frame with
    /// nothing to show is not drawn again. The flag is cleared only after a
    /// frame the display accepted: a frame that was skipped leaves the
    /// previous frame's content on screen, and the change must be drawn
    /// again.
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// Record that the scene changed.
    ///
    /// The engine calls this for a change it made outside the scene, and
    /// the scene calls it for a change that does not move a revision.
    pub(crate) fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Clear the change flag.
    ///
    /// Only the engine clears it, and only after a frame the display
    /// accepted.
    pub(crate) fn clear_dirty(&mut self) {
        self.dirty = false;
    }

    /// Bump [`Scene::structure_revision`]. Over-approximating (bumping on a
    /// `*_mut` accessor that ends up not writing) is safe: it only costs a
    /// resync.
    fn touch_structure(&mut self) {
        self.structure_revision = self.structure_revision.wrapping_add(1);
        self.mark_dirty();
    }

    /// Bump [`Scene::content_revision`].
    fn touch_content(&mut self) {
        self.content_revision = self.content_revision.wrapping_add(1);
        self.mark_dirty();
    }

    /// Create a window and return its new id. Starts visible, fully opaque
    /// and without a primary layer (the first layer added becomes primary).
    pub fn add_window(&mut self, title: impl Into<String>, inner_size: (u32, u32)) -> u32 {
        let id = self.next_window;
        self.next_window += 1;
        self.window_index.insert(id, self.windows.len());
        self.windows.push(WindowState {
            id,
            title: title.into(),
            inner_size,
            visible: true,
            opacity: 1.0,
            primary_layer: None,
            focused_layer: None,
            focus_domains: Vec::new(),
            domain_focus: map(),
            layers: Vec::new(),
        });
        self.touch_structure();
        id
    }

    /// Look up a window by id. Uses the id→index map, falling back to a
    /// linear scan if callers edited [`Scene::windows`] directly.
    pub fn window(&self, id: u32) -> Option<&WindowState> {
        if let Some(index) = self.window_index.get(&id).copied()
            && let Some(window) = self.windows.get(index)
            && window.id == id
        {
            return Some(window);
        }
        self.windows.iter().find(|w| w.id == id)
    }

    /// Mutably look up a window by id, bumping [`Scene::structure_revision`].
    /// A window mutation can change the root render order, so the subtree
    /// caches of its layers are marked stale.
    pub fn window_mut(&mut self, id: u32) -> Option<&mut WindowState> {
        self.invalidate_window_cache(id);
        self.touch_structure();
        let index = self
            .window_index
            .get(&id)
            .copied()
            .filter(|&index| self.windows.get(index).is_some_and(|w| w.id == id));
        match index {
            Some(index) => self.windows.get_mut(index),
            None => self.windows.iter_mut().find(|w| w.id == id),
        }
    }

    /// Destroy a window and **every** layer belonging to it — the reference
    /// `tTJSNI_BaseWindow::Invalidate` (`WindowIntf.cpp:175`) followed by the
    /// script object's destruction. Each layer's whole subtree is removed
    /// through [`Scene::destroy_layer`], so the layers' owned bitmaps and
    /// fonts are released and any surviving sibling references are cleaned
    /// up. Returns true when the window was present.
    ///
    /// This is the L2 primitive behind `Window.close()`; the reference keeps
    /// no window list beyond the native objects, but the scene owns the
    /// records, so removing them here keeps the model from leaking.
    pub fn remove_window(&mut self, id: u32) -> bool {
        if self.window(id).is_none() {
            return false;
        }
        // Collect first: `destroy_layer` mutates the layer table.
        let layer_ids: Vec<u32> = self
            .layers
            .iter()
            .filter(|layer| layer.window == id)
            .map(|layer| layer.id)
            .collect();
        for layer_id in layer_ids {
            self.destroy_layer(layer_id);
        }
        self.windows.retain(|window| window.id != id);
        self.window_index.clear();
        for (index, window) in self.windows.iter().enumerate() {
            self.window_index.insert(window.id, index);
        }
        self.touch_structure();
        true
    }

    /// Add a layer. `parent == None` attaches to the window directly.
    pub fn add_layer(&mut self, window: u32, parent: Option<u32>) -> u32 {
        let id = self.add_detached_layer();
        self.attach_layer(id, window, parent);
        id
    }

    /// Allocate a layer with the reference `CreateNativeInstance` defaults
    /// (default rect, no owner, manager or parent) but leave it detached from
    /// every window, parent and focus domain. [`Scene::attach_layer`] is the
    /// later `Construct` join.
    ///
    /// A detached layer is reachable through [`Scene::layer`] and
    /// [`Scene::layers`], so its properties can be read and written, but it
    /// is not in any `window.layers` list or child list, so it is not
    /// rendered, hit-tested or focus-searched until it is attached.
    pub fn add_detached_layer(&mut self) -> u32 {
        let id = self.next_layer;
        self.next_layer += 1;
        self.layer_index.insert(id, self.layers.len());
        self.layers.push(Self::detached_layer_state(id));
        self.touch_structure();
        id
    }

    /// The field initializers a freshly allocated layer is born with. The
    /// rect and main image are completed by the caller.
    fn detached_layer_state(id: u32) -> LayerState {
        LayerState {
            id,
            // No window yet; [`Scene::attach_layer`] assigns the real one.
            // `u32::MAX` cannot be a window id (`next_window` counts up from
            // 0), so window-scoped lookups never match a detached layer.
            window: u32::MAX,
            parent: None,
            domain: id,
            children: Vec::new(),
            bitmap: None,
            rect: Rect::default(),
            visible: false,
            opacity: 1.0,
            blend: BlendMode::SourceOver,
            z_order: 0,
            image_left: 0,
            image_top: 0,
            image_width: 0,
            image_height: 0,
            province: None,
            province_width: 0,
            province_height: 0,
            hit_threshold: 16,
            hit_type: 0,
            cursor: 0,
            face: 0,
            clip: None,
            hold_alpha: false,
            focusable: false,
            join_focus_chain: true,
            focus_work: None,
            enabled: true,
            name: String::new(),
            attention_left: 0,
            attention_top: 0,
            use_attention: false,
            cached: false,
            cache_enabled_count: 0,
            cache_bitmap: None,
            cache_content: None,
            transition_self: None,
            transition_frame: None,
            effect: None,
            cache_recalc: false,
            cache_source_generation: 0,
            hint: String::new(),
            show_parent_hint: false,
            ignore_hint_sensing: false,
            ime_mode: 0,
            // Transparent white, the reference `NeutralColor`/`TransparentColor`
            // initializer (`LayerIntf.cpp:334`), encoded `0xAARRGGBB`.
            neutral_color: 0x00ff_ffff,
            absolute_order_mode: false,
            call_on_paint: false,
            image_modified: false,
            font_id: None,
            is_primary: false,
            pending_paint: false,
            shutdown: false,
        }
    }

    /// Attach a layer allocated by [`Scene::add_detached_layer`] to `window`
    /// and `parent`, assigning its focus domain and primary slot — the
    /// reference `Construct` join. An unknown id is ignored.
    pub fn attach_layer(&mut self, id: u32, window: u32, parent: Option<u32>) {
        if self.layer(id).is_none() {
            return;
        }
        // A parent belongs to the same window; invalid/cross-window ids fall
        // back to a window-attached layer rather than creating an unreachable
        // subtree.
        let parent = parent.filter(|parent_id| {
            self.layer(*parent_id)
                .is_some_and(|layer| layer.window == window)
        });
        let primary = self.window(window).and_then(|w| w.primary_layer).is_none();
        // A parentless layer founds its own focus domain; a child inherits its
        // parent's. The domain id is the founding root's own layer id, so
        // `id == domain` identifies it.
        let domain = parent
            .and_then(|parent| self.layer(parent).map(|layer| layer.domain))
            .unwrap_or(id);
        if let Some(layer) = self.layer_mut(id) {
            layer.window = window;
            layer.parent = parent;
            layer.domain = domain;
        }
        // First layer of a window becomes its primary layer, which the
        // reference `tTVPLayerManager::AttachPrimary` forces visible (a new
        // layer otherwise defaults to hidden).
        if primary {
            if let Some(w) = self.window_mut(window) {
                w.primary_layer = Some(id);
            }
            if let Some(l) = self.layer_mut(id) {
                l.is_primary = true;
                l.visible = true;
            }
        }
        // A new child is appended at the front-most slot.
        self.assign_front_order(id);
        match parent {
            Some(p) => {
                if let Some(l) = self.layer_mut(p) {
                    l.children.push(id);
                }
            }
            None => {
                if let Some(w) = self.window_mut(window) {
                    w.layers.push(id);
                    // Register the new manager at the end of the window's list.
                    w.focus_domains.push(domain);
                    w.sync_focused_layer();
                }
            }
        }
        self.touch_structure();
    }

    /// Give `id` the sort key of the front-most slot among its current
    /// siblings: one past the largest sibling key, or zero for the first
    /// child. Call it after the layer's parent/window is set but before the
    /// layer is pushed into the sibling list, so the layer itself does not
    /// contribute a stale key.
    pub fn assign_front_order(&mut self, id: u32) {
        let Some(layer) = self.layer(id) else {
            return;
        };
        let parent = layer.parent;
        let window = layer.window;
        let last = match parent {
            Some(parent) => self.layer(parent).and_then(|parent| {
                parent
                    .children
                    .iter()
                    .filter_map(|child| self.layer(*child).map(|child| child.z_order))
                    .max()
            }),
            None => self.window(window).and_then(|window| {
                window
                    .layers
                    .iter()
                    .filter_map(|child| self.layer(*child).map(|child| child.z_order))
                    .max()
            }),
        };
        if let Some(layer) = self.layer_mut(id) {
            layer.z_order = last.map_or(0, |last| last.saturating_add(1));
        }
    }

    /// The rectangle that clips `id`'s subtree.
    ///
    /// A primary layer is the screen-sized draw target. A first layer created
    /// by script starts with the default 32x32 rect (or 0x0 for a direct scene
    /// layer), but the window's primary layer must clip like the window
    /// client; only an explicitly resized primary keeps its own size.
    /// Every other layer clips to its own rect.
    pub fn clipping_rect(&self, id: u32) -> Option<Rect> {
        let layer = self.layer(id)?;
        let rect = layer.rect;
        if layer.is_primary && (rect.w == 0 || rect.h == 0 || (rect.w == 32 && rect.h == 32)) {
            let (width, height) = self.window(layer.window)?.inner_size;
            return Some(Rect::new(rect.x, rect.y, width, height));
        }
        Some(rect)
    }

    /// The sibling list that contains `id`: the parent's `children`, or the
    /// window's root list for a parentless layer.
    fn sibling_ids(&self, id: u32) -> Option<Vec<u32>> {
        let layer = self.layer(id)?;
        match layer.parent {
            Some(parent) => self.layer(parent).map(|parent| parent.children.clone()),
            None => self
                .window(layer.window)
                .map(|window| window.layers.clone()),
        }
    }

    /// The position of `id` in its sibling list.
    pub fn sibling_position(&self, id: u32) -> Option<usize> {
        self.sibling_ids(id)?
            .iter()
            .position(|&sibling| sibling == id)
    }

    /// The sibling position, or `0` for a parentless layer.
    pub fn order_index(&self, id: u32) -> Option<i32> {
        let layer = self.layer(id)?;
        if layer.parent.is_none() {
            return Some(0);
        }
        self.sibling_position(id)
            .map(|position| position.min(i32::MAX as usize) as i32)
    }

    /// The stored absolute index while the parent is in absolute order mode,
    /// otherwise the sibling position. A parentless layer reports `0`.
    pub fn absolute_order_index(&self, id: u32) -> Option<i32> {
        let layer = self.layer(id)?;
        let Some(parent) = layer.parent else {
            return Some(0);
        };
        if self.layer(parent)?.absolute_order_mode {
            Some(layer.z_order)
        } else {
            self.sibling_position(id)
                .map(|position| position.min(i32::MAX as usize) as i32)
        }
    }

    /// Switching the child list into absolute mode seeds every child's
    /// absolute index from its current sibling position. Switching out
    /// renumbers the children back to their positions so the relative order is
    /// explicit. Returns `true` when the mode changed.
    pub fn set_absolute_order_mode(&mut self, id: u32, enabled: bool) -> bool {
        let Some(layer) = self.layer(id) else {
            return false;
        };
        if layer.absolute_order_mode == enabled {
            return false;
        }
        if let Some(layer) = self.layer_mut(id) {
            layer.absolute_order_mode = enabled;
        }
        let children = self
            .layer(id)
            .map(|layer| layer.children.clone())
            .unwrap_or_default();
        for (position, child) in children.iter().enumerate() {
            if let Some(child) = self.layer_mut(*child) {
                child.z_order = position.min(i32::MAX as usize) as i32;
            }
        }
        self.touch_structure();
        true
    }

    /// Require a parent, turn the parent's absolute order mode on, insert the
    /// layer at the position selected by the requested absolute index, then
    /// store that index. The insertion point is the first sibling whose
    /// absolute index is not less than the requested value, compensating for
    /// the removed layer when it moves forward.
    ///
    /// The layer keeps its place in the tree; only its draw position changes.
    ///
    /// # Errors
    ///
    /// The message names the reason the move was refused: the layer no longer
    /// exists, it has no parent (the primary layer and a siblingless layer
    /// cannot be moved), or the sibling list no longer holds it. The scene is
    /// left unchanged when the move is refused.
    pub fn set_absolute_order_index(&mut self, id: u32, absolute: i32) -> Result<(), String> {
        let Some(layer) = self.layer(id) else {
            return Err("Layer: layer no longer exists".to_string());
        };
        let Some(parent) = layer.parent else {
            return Err(
                "Layer.absolute: cannot move the primary layer or a siblingless layer".to_string(),
            );
        };
        if !self
            .layer(parent)
            .is_some_and(|parent| parent.absolute_order_mode)
        {
            self.set_absolute_order_mode(parent, true);
        }
        let from = self
            .sibling_position(id)
            .ok_or_else(|| "Layer.absolute: the sibling no longer exists".to_string())?;
        let mut to = self
            .layer(parent)
            .map(|parent| {
                parent
                    .children
                    .iter()
                    .position(|&child| {
                        self.layer(child).map_or(0, |child| child.z_order) >= absolute
                    })
                    .unwrap_or(parent.children.len())
            })
            .ok_or_else(|| "Layer.absolute: this layer has no parent".to_string())?;
        if from < to {
            to = to.saturating_sub(1);
        }
        let Some(parent_layer) = self.layer_mut(parent) else {
            return Err("Layer.absolute: this layer has no parent".to_string());
        };
        parent_layer.children.retain(|&child| child != id);
        let to = to.min(parent_layer.children.len());
        parent_layer.children.insert(to, id);
        if let Some(layer) = self.layer_mut(id) {
            layer.z_order = absolute;
        }
        self.touch_structure();
        Ok(())
    }

    /// Require a parent, leave absolute order mode, clamp the requested
    /// position to the sibling range, move the layer there, and renumber the
    /// siblings so the relative order index equals the draw position.
    ///
    /// The requested position is clamped to the sibling range; it is never an
    /// error to ask for a position outside it.
    ///
    /// # Errors
    ///
    /// The message names the reason the move was refused: the layer no longer
    /// exists, it has no parent (the primary layer and a siblingless layer
    /// cannot be moved), or the parent no longer holds the layer. The scene is
    /// left unchanged when the move is refused.
    pub fn set_order_index(&mut self, id: u32, order: i32) -> Result<(), String> {
        let Some(layer) = self.layer(id) else {
            return Err("Layer: layer no longer exists".to_string());
        };
        let Some(parent) = layer.parent else {
            return Err(
                "Layer.order: cannot move the primary layer or a siblingless layer".to_string(),
            );
        };
        self.set_absolute_order_mode(parent, false);
        let count = self
            .layer(parent)
            .map(|parent| parent.children.len())
            .unwrap_or(0);
        let max = count.saturating_sub(1).min(i32::MAX as usize) as i32;
        let index = order.clamp(0, max);
        let index = index as usize;
        let Some(parent_layer) = self.layer_mut(parent) else {
            return Err("Layer.order: this layer has no parent".to_string());
        };
        parent_layer.children.retain(|&child| child != id);
        let index = index.min(parent_layer.children.len());
        parent_layer.children.insert(index, id);
        self.renumber_siblings(id);
        self.touch_structure();
        Ok(())
    }

    /// Renumber the sibling list that contains `id` to its current positions,
    /// so `z_order` is the relative draw order for every sibling.
    pub fn renumber_siblings(&mut self, id: u32) {
        let Some(ids) = self.sibling_ids(id) else {
            return;
        };
        for (position, sibling) in ids.iter().enumerate() {
            if let Some(layer) = self.layer_mut(*sibling) {
                layer.z_order = position.min(i32::MAX as usize) as i32;
            }
        }
        self.touch_structure();
    }

    /// Look up a layer by id. Uses the id→index map, falling back to a
    /// linear scan if callers edited [`Scene::layers`] directly.
    pub fn layer(&self, id: u32) -> Option<&LayerState> {
        if let Some(index) = self.layer_index.get(&id).copied()
            && let Some(layer) = self.layers.get(index)
            && layer.id == id
        {
            return Some(layer);
        }
        self.layers.iter().find(|l| l.id == id)
    }

    /// Whether `id` is the primary layer of its window (the root layer the
    /// reference exposes as `window.primaryLayer` and whose MainImage is the
    /// window's screen buffer).
    pub fn is_primary_layer(&self, id: u32) -> bool {
        let Some(layer) = self.layer(id) else {
            return false;
        };
        self.window(layer.window)
            .is_some_and(|window| window.primary_layer == Some(id))
    }

    /// Mark every layer of `window` as having a stale subtree cache.
    fn invalidate_window_cache(&mut self, window: u32) {
        for layer in &mut self.layers {
            if layer.window == window {
                layer.cache_recalc = true;
            }
        }
    }

    /// Reference `tTJSNI_BaseLayer::UpdateChildRegion`
    /// (`LayerIntf.cpp:6236-6258`): mark `id`'s subtree cache and every
    /// caching ancestor's as stale because this layer changed. A malformed
    /// parent cycle terminates after the pool of layers is exhausted.
    fn invalidate_layer_cache(&mut self, id: u32) {
        if let Some(layer) = self.layer_mut_untracked(id) {
            layer.cache_recalc = true;
        }
        self.invalidate_ancestor_caches(id);
    }

    /// Mark every caching ancestor of `id` for recomposite, leaving `id`'s
    /// own subtree cache alone. A transition advances its own frame every
    /// tick, but its *sources* (the endpoint subtrees) are unchanged: the
    /// layer's composited content stays valid even though an ancestor's cache
    /// has to see the new frame. A malformed parent cycle terminates after the
    /// pool of layers is exhausted.
    fn invalidate_ancestor_caches(&mut self, id: u32) {
        self.invalidate_ancestor_caches_except(id, None);
    }

    /// The same walk, skipping `except` itself. A transition source that is
    /// also an ancestor of the destination supplies the frame's source pixels,
    /// so its cached content must stay current even though the destination's
    /// frame changed.
    fn invalidate_ancestor_caches_except(&mut self, id: u32, except: Option<u32>) {
        let limit = self.layers.len().saturating_add(1);
        let mut current = self.layer(id).and_then(|layer| layer.parent);
        let mut steps = 0usize;
        while let Some(c) = current {
            if steps > limit {
                break;
            }
            steps += 1;
            if except == Some(c) {
                current = self.layer(c).and_then(|layer| layer.parent);
                continue;
            }
            current = match self.layer_mut_untracked(c) {
                Some(layer) => {
                    layer.cache_recalc = true;
                    layer.parent
                }
                None => break,
            };
        }
    }

    /// Mutably look up a layer by id, bumping [`Scene::structure_revision`].
    /// The layer's subtree cache and every caching ancestor's are marked stale
    /// (`tTJSNI_BaseLayer::Update`, `LayerIntf.cpp:6305-6335`).
    pub fn layer_mut(&mut self, id: u32) -> Option<&mut LayerState> {
        self.invalidate_layer_cache(id);
        self.touch_structure();
        self.layer_mut_untracked(id)
    }

    /// Set a layer's script-visible `imageModified` flag without
    /// invalidating its cached subtree or the structure revision.
    ///
    /// The layer's image content changed, not its tree shape; the bitmap's own
    /// dirty flag drives the texture upload, so a full reprojection would only
    /// repeat work the upload already covers.
    pub fn mark_layer_image_modified(&mut self, id: u32) {
        if let Some(layer) = self.layer_mut_untracked(id) {
            layer.image_modified = true;
        }
    }

    /// Apply `f` to a layer and invalidate only when `f` reports a change.
    ///
    /// `f` returns true when it actually mutated the layer. Only then is the
    /// layer's subtree cache, and every caching ancestor's, marked stale and
    /// [`Scene::structure_revision`] bumped; an unchanged write touches
    /// neither, so a property setter that receives its current value is free.
    /// Returns `None` when the layer is absent, which lets a caller
    /// distinguish a missing layer from a no-op write.
    ///
    /// The reference guards a property write the same way, comparing the
    /// requested value with the stored one before it invalidates
    /// (`tTJSNI_BaseLayer::SetVisible`, `LayerIntf.cpp:1183`).
    pub fn update_layer(
        &mut self,
        id: u32,
        f: impl FnOnce(&mut LayerState) -> bool,
    ) -> Option<bool> {
        let changed = f(self.layer_mut_untracked(id)?);
        if changed {
            self.invalidate_layer_cache(id);
            self.touch_structure();
        }
        Some(changed)
    }

    /// Mutably look up a layer by id without marking any subtree cache
    /// stale. Used for cache bookkeeping that is not a content change.
    pub(crate) fn layer_mut_untracked(&mut self, id: u32) -> Option<&mut LayerState> {
        let index = self
            .layer_index
            .get(&id)
            .copied()
            .filter(|&index| self.layers.get(index).is_some_and(|l| l.id == id));
        match index {
            Some(index) => self.layers.get_mut(index),
            None => self.layers.iter_mut().find(|l| l.id == id),
        }
    }

    // ------------------------------------------------------------------
    // Layer MainImage / ProvinceImage raw-pixel access
    //
    // The reference exposes the underlying `tTVPBaseBitmap` scan-line
    // pointers (`GetMainImagePixelBuffer*` / `GetProvinceImagePixelBuffer*`,
    // `LayerIntf.cpp:3005-3044`) as `tTVInteger` addresses. The buffers are
    // heap allocations replaced wholesale on a resize, so a returned address
    // stays valid until the next resize of the same image (or until the
    // layer/bitmap is dropped). The VM is single-threaded, matching the
    // reference's raw-pointer contract. All access goes through the safe
    // slice closures below; producing the address is the single boundary.
    // ------------------------------------------------------------------

    /// Bytes per row of the layer's MainImage (RGBA8 → `width * 4`), or 0
    /// when the layer has no image. Reference `GetMainImagePixelBufferPitch`
    /// (`LayerIntf.cpp:3020`).
    pub fn main_image_pitch(&self, layer_id: u32) -> u32 {
        let Some(bitmap_id) = self.layer(layer_id).and_then(|l| l.bitmap) else {
            return 0;
        };
        self.bitmap(bitmap_id)
            .map_or(0, |b| b.width.saturating_mul(4))
    }

    /// Bytes per row of the layer's ProvinceImage (`width` bytes, 8bpp), or 0
    /// when absent. Reference `GetProvinceImagePixelBufferPitch`
    /// (`LayerIntf.cpp:3042`).
    pub fn province_image_pitch(&self, layer_id: u32) -> u32 {
        let Some(layer) = self.layer(layer_id) else {
            return 0;
        };
        if layer.province.is_none() {
            return 0;
        }
        layer.province_width
    }

    /// Address of the layer's MainImage pixel buffer, or 0 when absent or
    /// when the bitmap declares pixels it does not hold.
    /// Reference `GetMainImagePixelBuffer` (`LayerIntf.cpp:3005`).
    pub fn main_image_pixel_buffer(&self, layer_id: u32) -> usize {
        let Some(bitmap_id) = self.layer(layer_id).and_then(|l| l.bitmap) else {
            return 0;
        };
        self.bitmap(bitmap_id).map_or(0, |bitmap| {
            if bitmap_holds_pixels(bitmap) {
                bitmap.rgba.as_ptr() as usize
            } else {
                0
            }
        })
    }

    /// Address of the layer's MainImage pixel buffer for writing, marking the
    /// bitmap dirty (the reference `GetMainImagePixelBufferForWrite` sets
    /// `ImageModified` and calls `GetScanLineForWrite`). 0 when absent or when
    /// the bitmap declares pixels it does not hold.
    pub fn main_image_pixel_buffer_for_write(&mut self, layer_id: u32) -> usize {
        let Some(bitmap_id) = self.layer(layer_id).and_then(|l| l.bitmap) else {
            return 0;
        };
        let Some(bitmap) = self.bitmap_mut(bitmap_id) else {
            return 0;
        };
        if !bitmap_holds_pixels(bitmap) {
            return 0;
        }
        bitmap.mark_dirty();
        bitmap.rgba.as_mut_ptr() as usize
    }

    /// Address of the layer's ProvinceImage pixel buffer, or 0 when absent.
    /// Reference `GetProvinceImagePixelBuffer` (`LayerIntf.cpp:3027`).
    pub fn province_image_pixel_buffer(&self, layer_id: u32) -> usize {
        self.layer(layer_id)
            .and_then(|l| l.province.as_deref())
            .filter(|plane| !plane.is_empty())
            .map_or(0, |p| p.as_ptr() as usize)
    }

    /// Address of the layer's ProvinceImage pixel buffer for writing,
    /// allocating the plane first (the reference `AllocateProvinceImage`).
    /// Marks the image modified. Reference `GetProvinceImagePixelBufferForWrite`
    /// (`LayerIntf.cpp:3034`).
    pub fn province_image_pixel_buffer_for_write(&mut self, layer_id: u32) -> usize {
        self.allocate_province_image(layer_id);
        let Some(layer) = self.layer_mut(layer_id) else {
            return 0;
        };
        if layer.province.is_none() {
            return 0;
        }
        layer.image_modified = true;
        layer
            .province
            .as_deref_mut()
            .filter(|plane| !plane.is_empty())
            .map_or(0, |p| p.as_mut_ptr() as usize)
    }

    /// Safe read access to the layer's MainImage RGBA pixels. Returns `None`
    /// when the layer or its image is absent.
    pub fn with_main_image_pixels<R>(
        &self,
        layer_id: u32,
        f: impl FnOnce(&[u8]) -> R,
    ) -> Option<R> {
        let bitmap_id = self.layer(layer_id)?.bitmap?;
        self.bitmap(bitmap_id).map(|b| f(&b.rgba))
    }

    /// Safe write access to the layer's MainImage RGBA pixels; marks the
    /// bitmap dirty. Returns `None` when the layer or its image is absent.
    pub fn with_main_image_pixels_mut<R>(
        &mut self,
        layer_id: u32,
        f: impl FnOnce(&mut [u8]) -> R,
    ) -> Option<R> {
        let bitmap_id = self.layer(layer_id)?.bitmap?;
        let bitmap = self.bitmap_mut(bitmap_id)?;
        bitmap.mark_dirty();
        Some(f(&mut bitmap.rgba))
    }

    /// Safe read access to the layer's ProvinceImage plane. Returns `None`
    /// when the layer or its province plane is absent.
    pub fn with_province_pixels<R>(&self, layer_id: u32, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
        let layer = self.layer(layer_id)?;
        layer.province.as_deref().map(f)
    }

    /// Safe write access to the layer's ProvinceImage plane; marks the image
    /// modified. Returns `None` when the layer or its province plane is
    /// absent.
    pub fn with_province_pixels_mut<R>(
        &mut self,
        layer_id: u32,
        f: impl FnOnce(&mut [u8]) -> R,
    ) -> Option<R> {
        let layer = self.layer_mut(layer_id)?;
        layer.image_modified = true;
        layer.province.as_deref_mut().map(f)
    }

    /// Reference `AllocateProvinceImage` (`LayerIntf.cpp:2359`): size the
    /// province plane from the MainImage (or the layer rect when there is no
    /// MainImage) and fill the new area with 0.
    pub fn allocate_province_image(&mut self, layer_id: u32) {
        let (w, h) = {
            let Some(layer) = self.layer(layer_id) else {
                return;
            };
            match layer.bitmap.and_then(|id| self.bitmap(id)) {
                Some(bitmap) => (bitmap.width, bitmap.height),
                None => (layer.rect.w, layer.rect.h),
            }
        };
        self.resize_province_image(layer_id, w, h);
    }

    /// Reference `ProvinceImage->SetSizeWithFill(w, h, 0)`: resize the province
    /// plane, preserving the overlapping top-left region and filling the
    /// expansion with 0. A 0-sized province is clamped to 1x1 (the reference
    /// `tTVPBaseBitmap` constructor does the same).
    pub fn resize_province_image(&mut self, layer_id: u32, w: u32, h: u32) {
        let w = w.max(1);
        let h = h.max(1);
        let Some(layer) = self.layer_mut(layer_id) else {
            return;
        };
        if layer.province.is_some() && layer.province_width == w && layer.province_height == h {
            return;
        }
        let old_w = layer.province_width;
        let old_h = layer.province_height;
        let Some(bytes) = (w as usize).checked_mul(h as usize) else {
            return;
        };
        // A plane this large is structurally impossible to allocate; leave
        // the existing plane untouched rather than panicking.
        let mut raw = Vec::new();
        if raw.try_reserve_exact(bytes).is_err() {
            return;
        }
        raw.resize(bytes, 0);
        let mut buf = raw.into_boxed_slice();
        if let Some(old) = layer.province.take() {
            let copy_w = old_w.min(w) as usize;
            let copy_h = old_h.min(h) as usize;
            for y in 0..copy_h {
                let src = &old[y * old_w as usize..y * old_w as usize + copy_w];
                let dst = &mut buf[y * w as usize..y * w as usize + copy_w];
                dst.copy_from_slice(src);
            }
        }
        layer.province = Some(buf);
        layer.province_width = w;
        layer.province_height = h;
        layer.image_modified = true;
    }

    /// Reference `DeallocateProvinceImage` (`LayerIntf.cpp:2374`): drop the
    /// province plane.
    pub fn deallocate_province_image(&mut self, layer_id: u32) {
        if let Some(layer) = self.layer_mut(layer_id)
            && layer.province.take().is_some()
        {
            layer.province_width = 0;
            layer.province_height = 0;
            layer.image_modified = true;
        }
    }

    /// Reference `GetProvincePixel` (`LayerIntf.cpp:2973`): 0 when the plane is
    /// absent or the coordinate is outside it.
    ///
    /// A plane shorter than the size it declares reads as 0 and reports the
    /// inconsistency.
    pub fn province_pixel(&self, layer_id: u32, x: i32, y: i32) -> i32 {
        let Some(layer) = self.layer(layer_id) else {
            return 0;
        };
        let Some(buf) = layer.province.as_deref() else {
            return 0;
        };
        let Ok(px) = u32::try_from(x) else {
            return 0;
        };
        let Ok(py) = u32::try_from(y) else {
            return 0;
        };
        if px >= layer.province_width || py >= layer.province_height {
            return 0;
        }
        match plane_offset(layer.province_width, px, py).and_then(|offset| buf.get(offset)) {
            Some(byte) => i32::from(*byte),
            None => {
                log::error!(
                    "engine: the province pixel read of a {}x{} province plane found {} plane \
                     bytes",
                    layer.province_width,
                    layer.province_height,
                    buf.len()
                );
                0
            }
        }
    }

    /// Reference `SetProvincePixel` (`LayerIntf.cpp:2985`): allocate the plane
    /// when absent, clip against `ClipRect` (and the plane bounds), store the
    /// low byte and mark the image modified.
    ///
    /// A plane shorter than the size it declares is a no-op that reports the
    /// inconsistency.
    pub fn set_province_pixel(&mut self, layer_id: u32, x: i32, y: i32, n: i32) {
        self.allocate_province_image(layer_id);
        let Some(layer) = self.layer_mut(layer_id) else {
            return;
        };
        let (w, h) = (layer.province_width, layer.province_height);
        let in_clip = match layer.clip {
            Some(c) => {
                x >= c.x
                    && y >= c.y
                    && x < c.x.saturating_add(dim_i32(c.w))
                    && y < c.y.saturating_add(dim_i32(c.h))
            }
            None => true,
        };
        if !in_clip {
            return;
        }
        let Ok(px) = u32::try_from(x) else {
            return;
        };
        let Ok(py) = u32::try_from(y) else {
            return;
        };
        if px >= w || py >= h {
            return;
        }
        let Some(offset) = plane_offset(w, px, py) else {
            log::error!(
                "engine: the province pixel write of a {w}x{h} province plane has no address"
            );
            return;
        };
        let Some(plane) = layer.province.as_deref_mut() else {
            return;
        };
        match plane.get_mut(offset) {
            Some(slot) => {
                *slot = n as u8;
                layer.image_modified = true;
            }
            None => log::error!(
                "engine: the province pixel write of a {w}x{h} province plane found {} plane bytes",
                plane.len()
            ),
        }
    }

    /// Detach one layer — the reference `tTJSNI_BaseLayer::Part()`
    /// (`LayerIntf.cpp:624`). The layer is severed from its parent (or the
    /// window root list) and removed from the scene. Direct children are
    /// detached too, but their records stay alive until their own script
    /// objects are finalized (`LayerIntf.cpp:535-549`).
    ///
    /// This is *detachment*, not destruction: use [`Scene::destroy_layer`] to
    /// remove a native instance and its whole subtree.
    pub fn remove_layer(&mut self, id: u32) {
        let Some(layer) = self.layer(id) else {
            return;
        };
        let (
            win,
            parent,
            domain,
            font_id,
            bitmap,
            cache_bitmap,
            cache_content,
            transition_self,
            transition_frame,
            effect,
        ) = (
            layer.window,
            layer.parent,
            layer.domain,
            layer.font_id,
            layer.bitmap,
            layer.cache_bitmap,
            layer.cache_content,
            layer.transition_self,
            layer.transition_frame,
            layer.effect.clone(),
        );
        let children = layer.children.clone();
        // `Part()`: sever this layer from its parent (or the window roots).
        if let Some(p) = parent {
            if let Some(pl) = self.layer_mut(p) {
                pl.children.retain(|&x| x != id);
            }
        } else if let Some(w) = self.window_mut(win) {
            w.layers.retain(|&x| x != id);
        }
        // Sever the direct children too — reference `tTJSNI_BaseLayer::Invalidate`
        // calls `child->Part()` for each of them (`LayerIntf.cpp:535`), and
        // `Part()` is exactly `Parent->SeverChild(this); Parent = nullptr;`
        // (`LayerIntf.cpp:624`).
        //
        // They must **not** be promoted to window-level layers. The reference
        // draws precisely the tree reachable from the window's `Primary` layer
        // (`tTVPLayerManager::RecreateOverallOrderIndex`, `LayerManager.cpp:208`),
        // and `NotifyPart` invalidates that index — so a parted layer stops
        // being drawn *and* hit-tested. A script that wants such a layer back
        // re-parents it explicitly, which restores it.
        for child in &children {
            if let Some(cl) = self.layer_mut(*child) {
                cl.parent = None;
            }
        }
        self.layers.retain(|l| l.id != id);
        // The removed layer releases its MainImage. A bitmap still held by a
        // surviving layer or a script `Bitmap` object is kept
        // (see [`Scene::release_bitmap`]).
        if let Some(bitmap) = bitmap {
            self.release_bitmap(bitmap);
        }
        // The subtree cache is layer-owned too; release it with the layer.
        if cache_bitmap != cache_content
            && let Some(cache) = cache_bitmap
        {
            self.release_bitmap(cache);
        }
        if let Some(content) = cache_content {
            self.release_bitmap(content);
        }
        // The transition self image is layer-owned too.
        if let Some(self_image) = transition_self {
            self.release_bitmap(self_image);
        }
        // The transition frame names the transition's two sources; they are
        // released when nothing else still holds them.
        if let Some(frame) = transition_frame {
            self.release_bitmap(frame.src1.bitmap);
            self.release_bitmap(frame.src2.bitmap);
        }
        // The effect draw names the transition's sources as well.
        if let Some(effect) = &effect {
            for bitmap in effect.bitmaps() {
                self.release_bitmap(bitmap);
            }
        }
        // The removed layer owns its lazily-allocated font; drop it with the
        // layer (a surviving child keeps its own font).
        if let Some(font_id) = font_id {
            self.remove_font(font_id);
        }
        // Indices shifted: rebuild the id→index map.
        self.layer_index.clear();
        for (index, layer) in self.layers.iter().enumerate() {
            self.layer_index.insert(layer.id, index);
        }
        // The reference `DetachPrimary` leaves the window without a primary,
        // and `SeverChild` blurs a removed focus holder. A removed parentless
        // root also unregisters its manager (`UnregisterSelfFromWindow`,
        // `reference/cpp/core/visual/LayerIntf.cpp:533-535`;
        // `reference/cpp/core/visual/LayerTreeOwnerImpl.cpp:99-108`), so the
        // next root becomes the window's input domain.
        if let Some(w) = self.window_mut(win) {
            if w.primary_layer == Some(id) {
                w.primary_layer = None;
            }
            if id == domain {
                w.focus_domains.retain(|registered| *registered != domain);
                w.domain_focus.remove(&domain);
            }
            w.domain_focus.retain(|_, focused| *focused != id);
            w.sync_focused_layer();
        }
        self.touch_structure();
    }

    /// Reference `tTJSNI_BaseLayer::Invalidate` (`LayerIntf.cpp:515`): the
    /// explicit native teardown the script triggers with `invalidate layer`.
    /// The invalidated layer is removed from the scene and its direct
    /// children are severed (`Part`, `LayerIntf.cpp:535-549`), but each
    /// child's record remains until that child's own script object is
    /// finalized. The compat layer finalizes the children before calling this
    /// so their finalizers still see a live layer.
    pub fn invalidate_layer(&mut self, id: u32) {
        if self.layer(id).is_none() {
            return;
        }
        if let Some(l) = self.layer_mut(id) {
            l.shutdown = true;
            l.pending_paint = false;
        }
        self.remove_layer(id);
    }

    /// Drop a font record and rebuild the id→index map.
    fn remove_font(&mut self, font_id: u32) {
        self.fonts.retain(|f| f.id != font_id);
        self.font_index.clear();
        for (index, font) in self.fonts.iter().enumerate() {
            self.font_index.insert(font.id, index);
        }
    }

    /// Reference `tTJSNI_BaseWindow::Invalidate` (`WindowIntf.cpp:175`): the
    /// window invalidates every object registered to it (`ObjectVector`) and
    /// severs its primary layer. In this port every layer whose `window` is
    /// `window` is such a registered object, so invalidate each of them. The
    /// window keeps no strong TJS reference to its layers, but this still
    /// detaches the tree and releases the per-layer resources so an
    /// `invalidate win` cannot leave the layer tree rooted.
    pub fn invalidate_window(&mut self, window: u32) {
        let ids: Vec<u32> = self
            .layers
            .iter()
            .filter(|layer| layer.window == window)
            .map(|layer| layer.id)
            .collect();
        for id in ids {
            self.invalidate_layer(id);
        }
    }

    /// Destroy a layer and its **entire** child subtree — the reference
    /// `tTJSNI_BaseLayer::Invalidate` (`LayerIntf.cpp:535`) followed by the
    /// TJS GC cascade. `Invalidate` severs each direct child (`child->Part()`)
    /// and releases the `Children` array; once the parent no longer holds the
    /// children the whole subtree is destroyed with it. This is the function
    /// `layer_destroy` must use. [`Scene::remove_layer`] remains the explicit
    /// `Part()` detach that *keeps* the children.
    ///
    /// The subtree is collected through both the maintained `parent` links and
    /// the `children` vectors, so a stale link cannot leave an orphan behind.
    /// The removed layers' owned fonts, focus holders and primary-layer slot
    /// are cleaned up as well.
    pub fn destroy_layer(&mut self, id: u32) {
        if self.layer(id).is_none() {
            return;
        }
        // Merge the parent links and the maintained child lists into one
        // adjacency map; either source alone can be stale.
        let mut children_of: Map<u32, Vec<u32>> = map();
        for layer in &self.layers {
            if let Some(parent) = layer.parent {
                children_of.entry(parent).or_default().push(layer.id);
            }
            if !layer.children.is_empty() {
                children_of
                    .entry(layer.id)
                    .or_default()
                    .extend(layer.children.iter().copied());
            }
        }
        let mut doomed: Set<u32> = set();
        let mut stack = vec![id];
        while let Some(current) = stack.pop() {
            if !doomed.insert(current) {
                continue;
            }
            if let Some(children) = children_of.get(&current) {
                stack.extend(children.iter().copied());
            }
        }
        let window = self.layer(id).map(|layer| layer.window);
        let parent = self.layer(id).and_then(|layer| layer.parent);
        // Fonts are keyed by their own font id (layer.font_id), so collect the
        // doomed layers' font ids before the layers disappear.
        let doomed_fonts: Vec<u32> = self
            .layers
            .iter()
            .filter(|layer| doomed.contains(&layer.id))
            .filter_map(|layer| layer.font_id)
            .collect();
        // The doomed layers' MainImages are candidates for release; a bitmap
        // shared with a surviving layer or a script `Bitmap` stays. Collected
        // before the layers disappear.
        let doomed_bitmaps: Vec<u32> = self
            .layers
            .iter()
            .filter(|layer| doomed.contains(&layer.id))
            .flat_map(|layer| {
                layer
                    .bitmap
                    .into_iter()
                    .chain(layer.cache_bitmap)
                    .chain(layer.cache_content)
                    .chain(layer.transition_self)
                    .chain(
                        layer
                            .transition_frame
                            .into_iter()
                            .flat_map(|frame| [frame.src1.bitmap, frame.src2.bitmap]),
                    )
            })
            .collect();
        // Drop the doomed ids from every surviving layer's child list and
        // focus search state.
        for layer in &mut self.layers {
            if doomed.contains(&layer.id) {
                continue;
            }
            layer.children.retain(|child| !doomed.contains(child));
            if layer
                .focus_work
                .is_some_and(|focus| doomed.contains(&focus))
            {
                layer.focus_work = None;
            }
        }
        self.layers.retain(|layer| !doomed.contains(&layer.id));
        // The surviving parent lost its doomed children: its subtree cache
        // and every caching ancestor's are now stale.
        if let Some(parent) = parent {
            self.invalidate_layer_cache(parent);
        }
        for bitmap in doomed_bitmaps {
            self.release_bitmap(bitmap);
        }
        self.fonts.retain(|font| !doomed_fonts.contains(&font.id));
        // Indices shifted: rebuild the id→index maps.
        self.layer_index.clear();
        for (index, layer) in self.layers.iter().enumerate() {
            self.layer_index.insert(layer.id, index);
        }
        self.font_index.clear();
        for (index, font) in self.fonts.iter().enumerate() {
            self.font_index.insert(font.id, index);
        }
        // The reference `DetachPrimary` leaves the window without a primary,
        // and `SeverChild`/`BlurTree` clears a removed focus holder.
        if let Some(window) = window
            && let Some(w) = self.window_mut(window)
        {
            w.layers.retain(|layer| !doomed.contains(layer));
            if w.primary_layer.is_some_and(|p| doomed.contains(&p)) {
                w.primary_layer = None;
            }
            // A doomed parentless root unregisters its manager (a domain id
            // is its root's id); survivors lose only a doomed focus holder.
            w.focus_domains.retain(|domain| !doomed.contains(domain));
            w.domain_focus
                .retain(|domain, focused| !doomed.contains(domain) && !doomed.contains(focused));
            w.sync_focused_layer();
        }
        self.touch_structure();
    }

    /// Reference `tTVPLayerManager::SetFocusTo`: give keyboard focus to
    /// `id` in the window's **input domain** — the first-registered root's
    /// domain, the one `Window.focusedLayer` and all input bind to —
    /// clearing the previous holder. Returns `(previous, new)`. An invalid
    /// `id` returns `(None, None)` and leaves the current holder unchanged;
    /// use [`Scene::clear_focus`] to blur it.
    ///
    /// The layer's own domain is irrelevant, matching the reference setter,
    /// which moves the window's first manager regardless of the layer's
    /// membership (`reference/cpp/core/visual/LayerTreeOwnerImpl.cpp:385-389`).
    /// Use [`Scene::set_domain_focus`] to move focus in a specific domain.
    pub fn set_focus(&mut self, id: u32) -> (Option<u32>, Option<u32>) {
        let Some(layer) = self.layer(id) else {
            return (None, None);
        };
        let win = layer.window;
        let Some(domain) = self.window(win).and_then(|w| w.input_domain()) else {
            return (None, None);
        };
        let prev = self.focused_in(win, domain);
        if prev == Some(id) {
            return (prev, Some(id));
        }
        self.set_domain_focus(win, domain, Some(id));
        (prev, Some(id))
    }

    /// Clear the input-domain focus of `window`, returning the previous
    /// holder. This is the `Window.focusedLayer = null` shape
    /// (`reference/cpp/core/visual/LayerTreeOwnerImpl.cpp:385-389`).
    pub fn clear_focus(&mut self, window: u32) -> Option<u32> {
        let domain = self.window(window).and_then(|w| w.input_domain())?;
        let prev = self.focused_in(window, domain);
        if prev.is_some() {
            self.set_domain_focus(window, domain, None);
        }
        prev
    }

    /// The focused layer of `window`'s `domain`, or `None` when that domain
    /// holds no focus. A domain is one parentless root's manager
    /// (`reference/cpp/core/visual/LayerIntf.cpp:461-473`); every focus
    /// read/write stays inside one domain
    /// (`reference/cpp/core/visual/LayerManager.h:483-490`).
    pub fn focused_in(&self, window: u32, domain: u32) -> Option<u32> {
        self.window(window)
            .and_then(|w| w.domain_focus.get(&domain).copied())
    }

    /// Move `window`'s `domain` focus to `target` (`None` clears it) — the
    /// `tTVPLayerManager::FocusedLayer` assignment (`LayerTreeOwnerImpl.h:483-490`).
    /// Moving a domain other than the input domain leaves the window-level
    /// `focused_layer` untouched.
    pub fn set_domain_focus(&mut self, window: u32, domain: u32, target: Option<u32>) {
        if self.window(window).is_none() {
            return;
        }
        if self.focused_in(window, domain) == target {
            return;
        }
        if let Some(w) = self.window_mut(window) {
            match target {
                Some(id) => {
                    w.domain_focus.insert(domain, id);
                }
                None => {
                    w.domain_focus.remove(&domain);
                }
            }
            w.sync_focused_layer();
        }
    }

    /// The domain a window's input events and `focusedLayer` property bind
    /// to: the first-registered parentless root's domain, or `None` while
    /// the window has no such root (`PrimaryLayerManagerIndex` is 0 and
    /// never reassigned, `reference/cpp/core/visual/LayerTreeOwnerImpl.h:21`,
    /// `reference/cpp/core/visual/LayerTreeOwnerImpl.cpp:20`).
    pub fn window_input_root(&self, window: u32) -> Option<u32> {
        self.window(window).and_then(|w| w.input_domain())
    }

    /// Reference `GetNodeVisible`: `visible` and every ancestor visible.
    /// A [`LayerState::shutdown`] layer is inert and reports `false`.
    ///
    /// A missing id or a corrupted/cyclic parent chain is a typed miss
    /// (`false`), never a hang — the walk is bounded by the layer count, the
    /// same defensive contract [`Scene::window_layer_order`] documents.
    pub fn node_visible(&self, id: u32) -> bool {
        let mut cur = id;
        let mut steps = 0usize;
        loop {
            let Some(layer) = self.layer(cur) else {
                return false;
            };
            if layer.shutdown || !layer.visible {
                return false;
            }
            match layer.parent {
                Some(p) => {
                    steps += 1;
                    if steps > self.layers.len() {
                        return false;
                    }
                    cur = p;
                }
                None => return true,
            }
        }
    }

    /// Reference `GetNodeEnabled`: `enabled` and every ancestor enabled.
    /// A [`LayerState::shutdown`] layer is inert and reports `false`.
    ///
    /// Bounded like [`Scene::node_visible`]: a corrupted parent chain reports
    /// "not enabled" instead of looping forever.
    pub fn node_enabled(&self, id: u32) -> bool {
        let mut cur = id;
        let mut steps = 0usize;
        loop {
            let Some(layer) = self.layer(cur) else {
                return false;
            };
            if layer.shutdown || !layer.enabled {
                return false;
            }
            match layer.parent {
                Some(p) => {
                    steps += 1;
                    if steps > self.layers.len() {
                        return false;
                    }
                    cur = p;
                }
                None => return true,
            }
        }
    }

    /// Reference `GetNodeFocusable` (`LayerIntf.h:665`): `focusable` plus
    /// node visible/enabled at every ancestor. Manager `Mode` layers are not
    /// modelled.
    pub fn node_focusable(&self, id: u32) -> bool {
        self.layer(id)
            .is_some_and(|l| self.node_visible(id) && self.node_enabled(id) && l.focusable)
    }

    /// The layer's direct children in maintained sibling order — the order
    /// `GetChildrenArrayObjectNoAddRef` (`LayerIntf.cpp:669`) builds the
    /// `children` array in.
    pub fn ordered_children(&self, layer_id: u32) -> Vec<u32> {
        let Some(layer) = self.layer(layer_id) else {
            return Vec::new();
        };
        let mut ids = layer.children.clone();
        let maintained = ids.clone();
        self.sort_siblings(&mut ids, &maintained);
        ids
    }

    /// Reference `_GetNextFocusable`/`GetNextFocusable`
    /// (`LayerIntf.cpp:3631`): the next `JoinFocusChain` focusable layer in
    /// the window's paint order, wrapping, never returning `id` itself. Only
    /// layers of `id`'s own focus domain are considered; the reference walks
    /// the receiver's manager node list, which reaches no other root
    /// (`reference/cpp/core/visual/LayerIntf.cpp:1260-1283`).
    pub fn next_focusable(&self, id: u32) -> Option<u32> {
        self.focusable_neighbor(id, true)
    }

    /// Reference `_GetPrevFocusable`/`GetPrevFocusable`
    /// (`LayerIntf.cpp:3595`): the previous focusable layer, wrapping, scoped
    /// to `id`'s focus domain like [`Scene::next_focusable`].
    pub fn prev_focusable(&self, id: u32) -> Option<u32> {
        self.focusable_neighbor(id, false)
    }

    fn focusable_neighbor(&self, id: u32, forward: bool) -> Option<u32> {
        let layer = self.layer(id)?;
        // The manager chain holds exactly one root's tree, so the search is
        // restricted to `id`'s domain.
        let order: Vec<u32> = self
            .window_layer_order(layer.window)
            .into_iter()
            .filter(|&cand| self.layer(cand).is_some_and(|l| l.domain == layer.domain))
            .collect();
        let n = order.len();
        if n == 0 {
            return None;
        }
        let start = order.iter().position(|&x| x == id)?;
        for step in 1..n {
            let idx = if forward {
                (start + step) % n
            } else {
                (start + n - step) % n
            };
            let cand = order[idx];
            let Some(candidate) = self.layer(cand) else {
                continue;
            };
            if self.node_focusable(cand) && candidate.join_focus_chain {
                return Some(cand);
            }
        }
        None
    }

    /// Reference `GetMostFrontChildAt` (`LayerIntf.cpp:3363`) behind the
    /// `getLayerAt` native: the frontmost layer at window point `(x, y)`,
    /// where `(x, y)` is given in `layer_id`'s local coordinates. Returns
    /// `None` when the point misses everything or the frontmost hit is
    /// disabled and `get_disabled` is false.
    pub fn layer_at(
        &self,
        layer_id: u32,
        x: i32,
        y: i32,
        exclude_self: bool,
        get_disabled: bool,
    ) -> Option<u32> {
        let layer = self.layer(layer_id)?;
        let window = layer.window;
        // Convert to window coordinates (the native `getLayerAt` adds this
        // layer's and its non-root ancestors' offsets before hit-testing).
        let (mut px, mut py) = (x, y);
        let mut cur = Some(layer_id);
        // Bounded like [`Self::node_visible`]: a cyclic parent chain is a
        // typed miss, not a hang in the input path.
        let mut steps = 0usize;
        while let Some(c) = cur {
            let l = self.layer(c)?;
            match l.parent {
                Some(p) => {
                    px = px.saturating_add(l.rect.x);
                    py = py.saturating_add(l.rect.y);
                    steps += 1;
                    if steps > self.layers.len() {
                        return None;
                    }
                    cur = Some(p);
                }
                None => break,
            }
        }
        // Front-to-back over the flattened tree; an invisible parent prunes
        // its subtree through `node_visible`.
        for id in self.window_layer_order(window).into_iter().rev() {
            if exclude_self && id == layer_id {
                continue;
            }
            let Some(l) = self.layer(id) else {
                continue;
            };
            if !self.node_visible(id) {
                continue;
            }
            if !self.ancestor_rect_contains(id, px, py) {
                continue;
            }
            let Some((ax, ay)) = self.absolute_offset(id) else {
                continue;
            };
            let Some(clip) = self.clipping_rect(id) else {
                continue;
            };
            let lx = px - ax;
            let ly = py - ay;
            if lx < 0 || ly < 0 || lx >= clip.w as i32 || ly >= clip.h as i32 {
                continue;
            }
            if self.hit_test_layer(l, lx, ly) {
                // A disabled topmost hit stops the search with no layer (the
                // reference sets `*lay = nullptr` and returns true).
                if !get_disabled && !self.node_enabled(id) {
                    return None;
                }
                return Some(id);
            }
        }
        None
    }

    /// Whether `(x, y)`, in window coordinates, lies inside every ancestor
    /// layer's `Rect` of `id`. The check walks parent to root and is bounded
    /// like [`Self::node_visible`], so a descendant cannot be hit outside an
    /// ancestor's rectangle.
    fn ancestor_rect_contains(&self, id: u32, x: i32, y: i32) -> bool {
        let mut current = self.layer(id).and_then(|layer| layer.parent);
        let mut steps = 0usize;
        while let Some(ancestor) = current {
            let Some(layer) = self.layer(ancestor) else {
                return false;
            };
            let Some((ax, ay)) = self.absolute_offset(ancestor) else {
                return false;
            };
            let Some(clip) = self.clipping_rect(ancestor) else {
                return false;
            };
            let lx = x.saturating_sub(ax);
            let ly = y.saturating_sub(ay);
            if lx < 0 || ly < 0 || lx >= clip.w as i32 || ly >= clip.h as i32 {
                return false;
            }
            steps += 1;
            if steps > self.layers.len() {
                return false;
            }
            current = layer.parent;
        }
        true
    }

    /// Sum of this layer's and its ancestors' `rect` offsets (absolute
    /// window position). Bounded like [`Self::node_visible`]; a stale id or a
    /// corrupted parent chain is a typed miss (`None`).
    pub fn absolute_offset(&self, id: u32) -> Option<(i32, i32)> {
        let mut x = 0i32;
        let mut y = 0i32;
        let mut cur = Some(id);
        let mut steps = 0usize;
        while let Some(c) = cur {
            let l = self.layer(c)?;
            x = x.saturating_add(l.rect.x);
            y = y.saturating_add(l.rect.y);
            steps += 1;
            if steps > self.layers.len() {
                return None;
            }
            cur = l.parent;
        }
        Some((x, y))
    }

    /// Reference `_HitTestNoVisibleCheck` (`LayerIntf.cpp:3229`). `htMask`
    /// compares the main-image alpha against `hit_threshold` (&#8804;0 accepts
    /// any pixel; 256 rejects everything); `htProvince` treats a non-zero
    /// province byte as a hit. The `onHitTest` script hook is not dispatched
    /// here.
    fn hit_test_layer(&self, layer: &LayerState, x: i32, y: i32) -> bool {
        match layer.hit_type {
            // htProvince = 1
            1 => {
                let Some(buf) = layer.province.as_deref() else {
                    return false;
                };
                let px = x.saturating_sub(layer.image_left);
                let py = y.saturating_sub(layer.image_top);
                let (Ok(px), Ok(py)) = (u32::try_from(px), u32::try_from(py)) else {
                    return false;
                };
                if px >= layer.province_width || py >= layer.province_height {
                    return false;
                }
                // The plane can be shorter than the size it declares, so the
                // byte is read only when the buffer carries it.
                let byte =
                    plane_offset(layer.province_width, px, py).and_then(|offset| buf.get(offset));
                let Some(byte) = byte else {
                    log::error!(
                        "engine: the hit test of a {}x{} province plane found {} plane bytes",
                        layer.province_width,
                        layer.province_height,
                        buf.len()
                    );
                    return false;
                };
                *byte != 0
            }
            // htMask = 0
            0 => match layer.bitmap.and_then(|id| self.bitmap(id)) {
                Some(bmp) => {
                    let px = x.saturating_sub(layer.image_left);
                    let py = y.saturating_sub(layer.image_top);
                    if px < 0 || py < 0 || px >= bmp.width as i32 || py >= bmp.height as i32 {
                        return false;
                    }
                    if layer.hit_threshold <= 0 {
                        return true;
                    }
                    // A bitmap can declare a pixel and hold no bytes for it
                    // (a blank image whose allocation failed), so the alpha is
                    // read only when the buffer actually carries it.
                    let Some(offset) = bmp.pixel_offset(px as u32, py as u32) else {
                        return false;
                    };
                    let Some(alpha) = bmp.rgba.get(offset + 3) else {
                        log::error!(
                            "engine: the hit test of a {}x{} image found no pixel buffer",
                            bmp.width,
                            bmp.height
                        );
                        return false;
                    };
                    *alpha as i32 >= layer.hit_threshold
                }
                None => layer.hit_threshold <= 0,
            },
            // Unknown hit types hit (the reference falls through to true).
            _ => true,
        }
    }

    /// Move the layer to the front among its siblings (`bringToFront`): leave
    /// absolute order mode and place it at the last sibling slot. A parentless
    /// layer is a window root and moves to the end of the window's root list.
    pub fn layer_move_to_front(&mut self, id: u32) {
        let (window, parent) = match self.layer(id) {
            Some(l) => (l.window, l.parent),
            None => return,
        };
        if let Some(p) = parent {
            self.set_absolute_order_mode(p, false);
        }
        match parent {
            Some(p) => {
                if let Some(l) = self.layer_mut(p) {
                    l.children.retain(|&x| x != id);
                    l.children.push(id);
                }
            }
            None => {
                if let Some(w) = self.window_mut(window) {
                    w.layers.retain(|&x| x != id);
                    w.layers.push(id);
                }
            }
        }
        self.renumber_siblings(id);
        self.touch_structure();
    }

    /// Add a decoded bitmap and return its new id. The bitmap starts dirty;
    /// the caller supplies exactly `width * height * 4` RGBA bytes.
    pub fn add_bitmap(&mut self, width: u32, height: u32, rgba: Vec<u8>) -> u32 {
        let id = self.next_bitmap;
        self.next_bitmap += 1;
        self.bitmap_index.insert(id, self.bitmaps.len());
        self.bitmaps.push(BitmapState {
            id,
            width,
            height,
            rgba,
            name: None,
            dirty: true,
            dirty_region: None,
            script_owned: false,
            content_generation: 0,
            pending: None,
        });
        self.touch_structure();
        id
    }

    /// Adds a bitmap whose pixels are still being decoded and returns its new
    /// id.
    ///
    /// The bitmap starts with the probed `width` x `height` and no pixels:
    /// the decode result lands in it when [`Scene::resolve_bitmap`] runs.
    /// The bitmap is dirty, so the frame's upload sends the resolved pixels
    /// to the texture in the same frame.
    ///
    /// The pending image must come from a running decode: it is the future
    /// the result is published into, and a wait on it ends when that decode
    /// finishes or is cancelled.
    ///
    /// # Errors
    ///
    /// Returns [`ImageSizeError::TooLarge`] when the pixel count of the
    /// probed size is not representable. The caller then decodes
    /// synchronously and uses [`Scene::add_bitmap`].
    pub fn add_pending_bitmap(
        &mut self,
        width: u32,
        height: u32,
        pending: PendingImage,
    ) -> Result<u32, ImageSizeError> {
        if image_pixel_bytes(width, height).is_none() {
            return Err(ImageSizeError::TooLarge { width, height });
        }
        let id = self.next_bitmap;
        self.next_bitmap += 1;
        self.bitmap_index.insert(id, self.bitmaps.len());
        self.bitmaps.push(BitmapState {
            id,
            width,
            height,
            rgba: Vec::new(),
            name: None,
            dirty: true,
            dirty_region: None,
            script_owned: false,
            content_generation: 0,
            pending: Some(pending),
        });
        self.touch_structure();
        Ok(id)
    }

    /// Returns the cached decoded image for `key`, or `None` when the cache
    /// is disabled or holds no entry for the key.
    ///
    /// The entry is marked most recently used, exactly as
    /// [`Scene::cached_decoded_image`] does on a hit, and the caller receives
    /// an owned copy. A caller that wants to decide *how* the image is
    /// produced — a decode it can defer, for instance — looks the entry up
    /// here and only decodes on a miss.
    pub fn lookup_decoded_image(&mut self, key: &DecodedImageKey) -> Option<DecodedImage> {
        if !self.image_cache.enabled() {
            return None;
        }
        self.image_cache.take_copy(key)
    }

    /// Puts a pending decode on an existing bitmap.
    ///
    /// The bitmap takes the probed `width` x `height` and no pixels: the
    /// decode result lands in it when [`Scene::resolve_bitmap`] runs. A
    /// pending decode the bitmap already carries is cancelled, because the
    /// new one supersedes it — unless it is the same decode, or another
    /// bitmap waits for it.
    ///
    /// Returns whether the bitmap was present.
    pub fn set_pending_image(
        &mut self,
        id: u32,
        width: u32,
        height: u32,
        pending: PendingImage,
    ) -> bool {
        // The decode the bitmap carries is superseded by the new one. The
        // same decode installed again is not a new one, and a decode another
        // bitmap waits for is not this bitmap's to cancel.
        let previous = self.bitmap(id).and_then(|bitmap| bitmap.pending.clone());
        let cancel_previous = previous.as_ref().is_some_and(|previous| {
            !pending.same_decode(previous) && !self.another_bitmap_waits_for(previous, id)
        });
        let Some(bitmap) = self.bitmap_mut_untracked(id) else {
            return false;
        };
        if let Some(previous) = bitmap.pending.take()
            && cancel_previous
        {
            previous.cancel();
        }
        bitmap.width = width;
        bitmap.height = height;
        bitmap.rgba = Vec::new();
        bitmap.pending = Some(pending);
        bitmap.mark_dirty();
        self.touch_structure();
        true
    }

    /// Marks a bitmap as owned by a script `Bitmap` object.
    ///
    /// The flag is set without resolving a pending image, so a script object
    /// can take ownership of a bitmap whose decode is still running. Returns
    /// whether the bitmap was present.
    pub fn set_bitmap_script_owned(&mut self, id: u32) -> bool {
        let Some(bitmap) = self.bitmap_mut_untracked(id) else {
            return false;
        };
        bitmap.script_owned = true;
        true
    }

    /// The pending image of a bitmap, or `None` when it holds its pixels.
    pub fn pending_image(&self, id: u32) -> Option<&PendingImage> {
        self.bitmap(id).and_then(|bitmap| bitmap.pending.as_ref())
    }

    /// The decode already running for `key`, when the scene holds one.
    ///
    /// The lookup covers every bitmap that still carries a pending decode, so
    /// a load of an image whose decode has not finished shares that decode
    /// instead of reading and decoding the same bytes a second time. The
    /// size is the probed size of the image and the pending image is a clone
    /// of the shared future: every holder waits for the same decode and sees
    /// the same pixels.
    pub fn pending_decode_for(&self, key: &DecodedImageKey) -> Option<(u32, u32, PendingImage)> {
        self.bitmaps.iter().find_map(|bitmap| {
            let pending = bitmap.pending.as_ref()?;
            if !pending.fills_cache_entry(key) {
                return None;
            }
            Some((bitmap.width, bitmap.height, pending.clone()))
        })
    }

    /// Whether a bitmap other than `except` waits for the same decode.
    ///
    /// A decode can be shared: several bitmaps carry a clone of the same
    /// future and the pixels land in all of them. Cancelling it while
    /// another bitmap waits would leave that bitmap without pixels.
    fn another_bitmap_waits_for(&self, pending: &PendingImage, except: u32) -> bool {
        self.bitmaps.iter().any(|bitmap| {
            bitmap.id != except
                && bitmap
                    .pending
                    .as_ref()
                    .is_some_and(|other| other.same_decode(pending))
        })
    }

    /// Waits for one bitmap's pending image and installs its pixels.
    ///
    /// The wait is on that image alone. A decode that failed installs a blank
    /// transparent image of the probed size and reports the failure in the
    /// log; a cancelled decode installs the same blank. A bitmap without a
    /// pending image is left alone.
    ///
    /// A decode that produced pixels of the probed size also enters the
    /// decoded-image cache, under the key the load named — the same entry the
    /// synchronous load path stores, so a later load of the same image is a
    /// cache hit. A failed, cancelled or malformed decode caches nothing.
    ///
    /// Returns whether the bitmap exists.
    pub fn resolve_bitmap(&mut self, id: u32) -> bool {
        let Some(pending) = self.bitmap(id).and_then(|bitmap| bitmap.pending.clone()) else {
            return self.bitmap(id).is_some();
        };
        // The caller waits, so the job is demanded and a worker prefers it
        // over the normal jobs in the queue.
        pending.demand();
        let outcome = pending.await_ready();
        let Some(bitmap) = self.bitmap_mut_untracked(id) else {
            return false;
        };
        if bitmap.pending.take().is_none() {
            // Another caller resolved the same image first.
            return true;
        }
        let width = bitmap.width;
        let height = bitmap.height;
        // The entry the decoded pixels take in the cache, once the decode has
        // produced them and they match the probed size.
        let mut cache_entry: Option<(DecodedImageKey, DecodedImage)> = None;
        match outcome {
            ImageOutcome::Ready(image) => {
                let expected = (width as usize)
                    .checked_mul(height as usize)
                    .and_then(|pixels| pixels.checked_mul(4));
                match expected {
                    Some(expected) if expected == image.rgba.len() => {
                        let decoded = match Arc::try_unwrap(image) {
                            Ok(decoded) => decoded,
                            Err(shared) => (*shared).clone(),
                        };
                        // The cache takes the image only when it is
                        // consistent; the synchronous path refuses a
                        // malformed image too.
                        if let Some(key) = pending.cache_key()
                            && decoded.is_consistent()
                        {
                            cache_entry = Some((key, decoded.clone()));
                        }
                        bitmap.rgba = decoded.rgba;
                    }
                    _ => {
                        log::error!(
                            "engine: the decoded image of a {width}x{height} bitmap does not \
                             match the probed size; the bitmap stays blank"
                        );
                        install_blank(bitmap);
                    }
                }
            }
            ImageOutcome::Failed(message) => {
                log::error!("engine: an image decode failed: {message}");
                install_blank(bitmap);
            }
            ImageOutcome::Cancelled => {
                log::warn!("engine: an image decode was cancelled before it produced pixels");
                install_blank(bitmap);
            }
        }
        bitmap.mark_dirty();
        // The pixels landed, so the bitmap's content changed; the structure
        // revision moves with it because the projection's texture table may
        // have skipped the bitmap while it had no pixels.
        self.touch_content();
        self.touch_structure();
        if let Some((key, image)) = cache_entry {
            self.cache_decoded_image(key, image);
        }
        true
    }

    /// Waits for every pending image and installs the pixels.
    ///
    /// The decodes run concurrently, so the wait is the time the last one
    /// needs rather than the sum of them. Returns the number of images that
    /// were resolved.
    pub fn resolve_pending_images(&mut self) -> usize {
        let ids: Vec<u32> = self
            .bitmaps
            .iter()
            .filter(|bitmap| bitmap.pending.is_some())
            .map(|bitmap| bitmap.id)
            .collect();
        let mut resolved = 0;
        for id in ids {
            if self.resolve_bitmap(id) {
                resolved += 1;
            }
        }
        resolved
    }

    /// Resize a bitmap in place, preserving the overlapping top-left region
    /// and filling any expansion with `fill` (RGBA) — the reference
    /// `iTVPBaseBitmap::SetSizeWithFill` (`LayerBitmapIntf.cpp:122`).
    /// Dimensions are clamped to at least 1, matching the reference
    /// `tTVPBaseTexture` constructor. Only the rows the source buffer
    /// actually holds are preserved: a bitmap that declares a size without
    /// holding its pixels keeps the fill, and the missing rows are reported
    /// in the log rather than read out of bounds. Marks the bitmap dirty and
    /// bumps [`Scene::structure_revision`]. Returns true when the bitmap was
    /// present and resized.
    pub fn resize_bitmap(&mut self, id: u32, width: u32, height: u32, fill: [u8; 4]) -> bool {
        let width = width.max(1);
        let height = height.max(1);
        let Some((old_w, old_h)) = self.bitmap(id).map(|bitmap| (bitmap.width, bitmap.height))
        else {
            return false;
        };
        if old_w == width && old_h == height {
            return true;
        }
        // The image may still be decoding: the decoded pixels are the ones
        // the size change preserves.
        if !self.resolve_bitmap(id) {
            return false;
        }
        let (Ok(old_w), Ok(old_h), Ok(new_w), Ok(new_h)) = (
            usize::try_from(old_w),
            usize::try_from(old_h),
            usize::try_from(width),
            usize::try_from(height),
        ) else {
            return false;
        };
        let Some(pixels) = image_pixel_bytes(width, height) else {
            return false;
        };
        let (Some(old_pitch), Some(new_pitch)) = (old_w.checked_mul(4), new_w.checked_mul(4))
        else {
            return false;
        };
        let copy_w = old_w.min(new_w);
        let copy_h = old_h.min(new_h);
        let Some(copy_bytes) = copy_w.checked_mul(4) else {
            return false;
        };
        // The old pixels are reused in place: growing only has to extend the
        // buffer, and the preserved rows move within it. The uncovered part
        // alone takes the fill, so a resize never rewrites the preserved
        // pixels.
        let Some(bitmap) = self.bitmap_mut_untracked(id) else {
            return false;
        };
        let mut rgba = std::mem::take(&mut bitmap.rgba);
        let available = rgba.len();
        // The source rows are contiguous from the first, so the first row the
        // buffer does not hold ends the preserved region.
        let mut preserved = 0usize;
        for y in 0..copy_h {
            let Some(src_end) = y
                .checked_mul(old_pitch)
                .and_then(|start| start.checked_add(copy_bytes))
            else {
                break;
            };
            if src_end > available {
                break;
            }
            preserved += 1;
        }
        if rgba.len() < pixels {
            if rgba.try_reserve_exact(pixels - rgba.len()).is_err() {
                if let Some(bitmap) = self.bitmap_mut_untracked(id) {
                    bitmap.rgba = rgba;
                }
                return false;
            }
            rgba.resize(pixels, 0);
        }
        // Move the preserved rows within the buffer when the row pitch
        // changes. A wider pitch moves a row forward past rows that are still
        // to be read, so those move from the last row to the first; a narrower
        // pitch moves the other way. An unchanged pitch keeps every row where
        // it is.
        if new_pitch > old_pitch {
            for y in (0..preserved).rev() {
                let Some(src_start) = y.checked_mul(old_pitch) else {
                    break;
                };
                let Some(dst_start) = y.checked_mul(new_pitch) else {
                    break;
                };
                let (Some(src_end), Some(dst_end)) = (
                    src_start.checked_add(copy_bytes),
                    dst_start.checked_add(copy_bytes),
                ) else {
                    break;
                };
                if src_end <= available && dst_end <= pixels {
                    rgba.copy_within(src_start..src_end, dst_start);
                }
            }
        } else if new_pitch < old_pitch {
            for y in 0..preserved {
                let Some(src_start) = y.checked_mul(old_pitch) else {
                    break;
                };
                let Some(dst_start) = y.checked_mul(new_pitch) else {
                    break;
                };
                let (Some(src_end), Some(dst_end)) = (
                    src_start.checked_add(copy_bytes),
                    dst_start.checked_add(copy_bytes),
                ) else {
                    break;
                };
                if src_end <= available && dst_end <= pixels {
                    rgba.copy_within(src_start..src_end, dst_start);
                }
            }
        }
        let fill_span = |span: &mut [u8]| {
            if fill == [0, 0, 0, 0] {
                span.fill(0);
            } else {
                for chunk in span.as_chunks_mut::<4>().0 {
                    chunk.copy_from_slice(&fill);
                }
            }
        };
        if copy_bytes < new_pitch {
            for y in 0..preserved {
                let Some(start) = y
                    .checked_mul(new_pitch)
                    .and_then(|start| start.checked_add(copy_bytes))
                else {
                    break;
                };
                let Some(end) = y.checked_add(1).and_then(|row| row.checked_mul(new_pitch)) else {
                    break;
                };
                if let Some(span) = rgba.get_mut(start..end) {
                    fill_span(span);
                }
            }
        }
        for y in preserved..new_h {
            let Some(start) = y.checked_mul(new_pitch) else {
                break;
            };
            let Some(end) = start.checked_add(new_pitch) else {
                break;
            };
            // A row entirely inside the grown tail was zeroed by the resize
            // above, so a transparent fill has nothing to do.
            if fill == [0, 0, 0, 0] && start >= available {
                continue;
            }
            if let Some(span) = rgba.get_mut(start..end) {
                fill_span(span);
            }
        }
        // The source rows were read from the whole old buffer, so the
        // truncation to the new size happens after the copies and fills.
        rgba.truncate(pixels);
        if preserved < copy_h {
            let missing_rows = copy_h - preserved;
            log::error!(
                "engine: bitmap #{id} declares {old_w}x{old_h} but holds {available} pixel bytes; \
                 a resize to {new_w}x{new_h} keeps the fill for {missing_rows} of {copy_h} \
                 preserved rows"
            );
        }
        let Some(bitmap) = self.bitmap_mut(id) else {
            return false;
        };
        bitmap.width = width;
        bitmap.height = height;
        bitmap.rgba = rgba;
        bitmap.mark_dirty();
        self.touch_structure();
        true
    }

    /// Give a layer a MainImage of at least `min_w` x `min_h` pixels,
    /// allocating one when absent and growing it when smaller. Existing
    /// pixels are preserved and new pixels are filled with the layer's
    /// neutral color. When the image is created or resized, the tracked image
    /// size is refreshed and the clip rectangle reset to the whole image, and
    /// a province plane is resized to match. Returns false when the layer is
    /// missing or the size cannot be represented.
    ///
    /// Reference `AllocateImage` and `ChangeImageSize` (`LayerIntf.cpp:2326`,
    /// `:2307`).
    pub fn ensure_layer_image(&mut self, layer_id: u32, min_w: u32, min_h: u32) -> bool {
        let min_w = min_w.max(1);
        let min_h = min_h.max(1);
        let (bitmap, fill) = match self.layer(layer_id) {
            Some(layer) => (layer.bitmap, argb_to_rgba(layer.neutral_color)),
            None => return false,
        };
        let changed = match bitmap {
            Some(bitmap_id) => {
                let Some((w, h)) = self.bitmap(bitmap_id).map(|b| (b.width, b.height)) else {
                    return false;
                };
                if w < min_w || h < min_h {
                    if !self.resize_bitmap(bitmap_id, w.max(min_w), h.max(min_h), fill) {
                        return false;
                    }
                    true
                } else {
                    false
                }
            }
            None => {
                if !self.allocate_layer_bitmap(layer_id, min_w, min_h, fill) {
                    return false;
                }
                true
            }
        };
        if changed {
            self.sync_layer_image(layer_id);
        }
        true
    }

    /// Resize a layer's MainImage to exactly `width` x `height`, allocating
    /// it when absent. Existing pixels are preserved and the difference
    /// filled with the neutral color. The tracked image size is refreshed and
    /// the clip reset to the whole image, and a province plane is resized to
    /// match. Returns false when the layer is missing or the size cannot be
    /// represented.
    ///
    /// Reference `ChangeImageSize` (`LayerIntf.cpp:2307`).
    pub fn set_layer_image_size(&mut self, layer_id: u32, width: u32, height: u32) -> bool {
        let width = width.max(1);
        let height = height.max(1);
        let (bitmap, fill) = match self.layer(layer_id) {
            Some(layer) => (layer.bitmap, argb_to_rgba(layer.neutral_color)),
            None => return false,
        };
        match bitmap {
            Some(bitmap_id) => {
                if !self.resize_bitmap(bitmap_id, width, height, fill) {
                    return false;
                }
            }
            None => {
                if !self.allocate_layer_bitmap(layer_id, width, height, fill) {
                    return false;
                }
            }
        }
        self.sync_layer_image(layer_id);
        true
    }

    /// Set a layer's MainImage to `width` x `height` and adjust the layer
    /// rectangle and image offsets around it: a width or height smaller than
    /// the rectangle shrinks the rectangle and clears the matching offset, a
    /// rectangle the image no longer covers shifts the offset, then the image
    /// takes the new size. Because the rectangle only shrinks when the image
    /// is smaller, the image is guaranteed to cover it afterwards. Returns
    /// false when the layer is missing or the image cannot be resized.
    pub fn internal_set_image_size(&mut self, layer_id: u32, width: u32, height: u32) -> bool {
        let Some(layer) = self.layer_mut(layer_id) else {
            return false;
        };
        if i64::from(width) < i64::from(layer.rect.w) {
            layer.image_left = 0;
            layer.rect.w = width;
        }
        if i64::from(width) + i64::from(layer.image_left) < i64::from(layer.rect.w) {
            let left = i64::from(layer.rect.w) - i64::from(width);
            layer.image_left = left.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        }
        if i64::from(height) < i64::from(layer.rect.h) {
            layer.image_top = 0;
            layer.rect.h = height;
        }
        if i64::from(height) + i64::from(layer.image_top) < i64::from(layer.rect.h) {
            let top = i64::from(layer.rect.h) - i64::from(height);
            layer.image_top = top.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        }
        self.set_layer_image_size(layer_id, width, height)
    }

    /// Grow a layer's MainImage so it covers the layer rectangle and clamp
    /// `ImageLeft`/`ImageTop`, the reference `ImageLayerSizeChanged`
    /// (`LayerIntf.cpp:2656`). A layer with no MainImage is left untouched.
    pub fn layer_size_changed(&mut self, layer_id: u32) {
        let (rect_w, rect_h) = match self.layer(layer_id) {
            Some(layer) if layer.bitmap.is_some() => (layer.rect.w, layer.rect.h),
            _ => return,
        };
        self.ensure_layer_image(layer_id, rect_w, rect_h);
        let Some((bmp_w, bmp_h)) = self
            .layer(layer_id)
            .and_then(|layer| layer.bitmap)
            .and_then(|id| self.bitmap(id))
            .map(|bitmap| (bitmap.width, bitmap.height))
        else {
            return;
        };
        if let Some(layer) = self.layer_mut(layer_id) {
            if i64::from(bmp_w) + i64::from(layer.image_left) < i64::from(layer.rect.w) {
                let left = i64::from(layer.rect.w) - i64::from(bmp_w);
                layer.image_left = left.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
            }
            if i64::from(bmp_h) + i64::from(layer.image_top) < i64::from(layer.rect.h) {
                let top = i64::from(layer.rect.h) - i64::from(bmp_h);
                layer.image_top = top.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
            }
        }
    }

    /// Attach a freshly allocated `w` x `h` MainImage filled with `fill` to
    /// the layer, resetting the image offset. Returns false when the size is
    /// structurally impossible to allocate or the layer is missing.
    fn allocate_layer_bitmap(&mut self, layer_id: u32, w: u32, h: u32, fill: [u8; 4]) -> bool {
        let Some(bytes) = (w as usize)
            .checked_mul(h as usize)
            .and_then(|count| count.checked_mul(4))
        else {
            return false;
        };
        let mut rgba = Vec::new();
        if rgba.try_reserve_exact(bytes).is_err() {
            return false;
        }
        rgba.resize(bytes, 0);
        for chunk in rgba.as_chunks_mut::<4>().0 {
            chunk.copy_from_slice(&fill);
        }
        let bitmap_id = self.add_bitmap(w, h, rgba);
        let Some(layer) = self.layer_mut(layer_id) else {
            return false;
        };
        layer.bitmap = Some(bitmap_id);
        layer.image_left = 0;
        layer.image_top = 0;
        true
    }

    /// Refresh a layer's tracked image size and clip after its MainImage
    /// changed, and resize its province plane to match. Reference `ResetClip`
    /// and the province resize in `AllocateImage`/`ChangeImageSize`.
    fn sync_layer_image(&mut self, layer_id: u32) {
        let Some((w, h)) = self
            .layer(layer_id)
            .and_then(|layer| layer.bitmap)
            .and_then(|id| self.bitmap(id))
            .map(|bitmap| (bitmap.width, bitmap.height))
        else {
            return;
        };
        if let Some(layer) = self.layer_mut(layer_id) {
            layer.image_width = w;
            layer.image_height = h;
            layer.clip = None;
            layer.image_modified = true;
        }
        if self
            .layer(layer_id)
            .is_some_and(|layer| layer.province.is_some())
        {
            self.resize_province_image(layer_id, w, h);
        }
    }

    /// Remove a bitmap unconditionally (the low-level primitive behind
    /// [`Scene::release_bitmap`]). Keeps the id→index map consistent and
    /// bumps [`Scene::structure_revision`]. A pending decode is cancelled, so
    /// a waiter on it does not wait for pixels nobody will read. Returns true
    /// when the bitmap was present.
    pub fn remove_bitmap(&mut self, id: u32) -> bool {
        let before = self.bitmaps.len();
        let mut pending: Option<PendingImage> = None;
        self.bitmaps.retain_mut(|bitmap| {
            if bitmap.id == id {
                pending = bitmap.pending.take();
                false
            } else {
                true
            }
        });
        if self.bitmaps.len() == before {
            return false;
        }
        if let Some(pending) = pending {
            // Another bitmap may wait for the same decode: the pixels land
            // in it too, so the decode stays alive.
            if !self.another_bitmap_waits_for(&pending, id) {
                pending.cancel();
            }
        }
        self.bitmap_index.clear();
        for (index, bitmap) in self.bitmaps.iter().enumerate() {
            self.bitmap_index.insert(bitmap.id, index);
        }
        self.touch_structure();
        true
    }

    /// Whether any live layer still references `id` as its MainImage, its
    /// subtree cache, its transition self image or a source of its transition
    /// frame or effect draw.
    pub fn bitmap_referenced_by_layer(&self, id: u32) -> bool {
        self.layers.iter().any(|layer| {
            layer.bitmap == Some(id)
                || layer.cache_bitmap == Some(id)
                || layer.cache_content == Some(id)
                || layer.transition_self == Some(id)
                || layer
                    .transition_frame
                    .is_some_and(|frame| frame.src1.bitmap == id || frame.src2.bitmap == id)
                || layer
                    .effect
                    .as_ref()
                    .is_some_and(|effect| effect.bitmaps().any(|bitmap| bitmap == id))
        })
    }

    /// Reference `tTJSNI_BaseLayer::IncCacheEnabledCount`
    /// (`LayerIntf.cpp:3996`): register one consumer that needs this layer's
    /// subtree rendered into its cache. The composited content and the
    /// transition frame are allocated by
    /// [`crate::layer_ops::render_layer_subtree`] on the first render and by
    /// [`crate::layer_ops::ensure_transition_display`]. Returns the new count
    /// (`0` for an unknown layer).
    pub fn inc_cache_enabled_count(&mut self, id: u32) -> u32 {
        let Some(layer) = self.layer_mut(id) else {
            return 0;
        };
        layer.cache_enabled_count = layer.cache_enabled_count.saturating_add(1);
        layer.cache_recalc = true;
        layer.cache_enabled_count
    }

    /// Reference `tTJSNI_BaseLayer::DecCacheEnabledCount`
    /// (`LayerIntf.cpp:4008`): release one cache consumer. The composited
    /// content and the transition frame are freed with the last one. Returns
    /// the new count (`0` for an unknown layer).
    pub fn dec_cache_enabled_count(&mut self, id: u32) -> u32 {
        self.touch_structure();
        let count = match self.layer_mut_untracked(id) {
            Some(layer) => {
                layer.cache_enabled_count = layer.cache_enabled_count.saturating_sub(1);
                layer.cache_enabled_count
            }
            None => return 0,
        };
        if count == 0 {
            let (display, content) = match self.layer_mut_untracked(id) {
                Some(layer) => (layer.cache_bitmap.take(), layer.cache_content.take()),
                None => (None, None),
            };
            if display != content
                && let Some(display) = display
            {
                self.release_bitmap(display);
            }
            if let Some(content) = content {
                self.release_bitmap(content);
            }
        }
        count
    }

    /// Release a bitmap when the last holder is gone.
    ///
    /// Reference semantics: a layer's MainImage is a **reference-counted**
    /// `tTVPBaseBitmap`; destroying the layer drops only its own reference.
    /// In this port a bitmap id can be held by more than one owner, so
    /// freeing is conservative — the bitmap is removed only when
    /// * no live layer references it (MainImage, subtree cache or transition
    ///   self image), and
    /// * it is not owned by a script `Bitmap` object
    ///   ([`BitmapState::script_owned`]).
    ///
    /// Returns true when the bitmap was removed.
    pub fn release_bitmap(&mut self, id: u32) -> bool {
        let Some(bitmap) = self.bitmap(id) else {
            return false;
        };
        if bitmap.script_owned {
            return false;
        }
        if self.bitmap_referenced_by_layer(id) {
            return false;
        }
        if self.remove_bitmap(id) {
            log::trace!("released scene bitmap #{id}");
            return true;
        }
        false
    }

    /// Release a script object's claim on `id` and free the image when no
    /// layer still references it. Returns true when the bitmap was removed.
    pub fn release_script_bitmap(&mut self, id: u32) -> bool {
        if let Some(bitmap) = self.bitmaps.iter_mut().find(|bitmap| bitmap.id == id) {
            bitmap.script_owned = false;
        }
        self.release_bitmap(id)
    }

    /// Release every bitmap whose last holder has gone away. Returns the
    /// number freed. Per-layer release in [`Scene::remove_layer`] /
    /// [`Scene::destroy_layer`] is the normal path; this sweep is for a
    /// caller that tore a scene down through direct vec edits.
    pub fn collect_unused_bitmaps(&mut self) -> usize {
        let ids: Vec<u32> = self.bitmaps.iter().map(|bitmap| bitmap.id).collect();
        let mut freed = 0;
        for id in ids {
            if self.release_bitmap(id) {
                freed += 1;
            }
        }
        freed
    }

    /// The decoded-image cache byte limit (`0` when the cache is disabled or
    /// unlimited). The script-visible property uses the reference's numeric
    /// encoding; [`Self::decoded_image_cache_limit`] exposes the typed value.
    pub fn graphic_cache_limit(&self) -> u64 {
        self.image_cache.limit.option().unwrap_or(0)
    }

    /// The typed decoded-image cache bound.
    pub fn decoded_image_cache_limit(&self) -> CacheLimit {
        self.image_cache.limit
    }

    /// Set the decoded-image cache bound. `None` disables caching, `Some(0)`
    /// keeps every entry, and `Some(n)` caps the cached bytes at `n`.
    pub fn set_decoded_image_cache_limit(&mut self, limit: impl Into<CacheLimit>) {
        self.image_cache.limit = limit.into();
        if self.image_cache.enabled() {
            self.image_cache.evict_to_limit();
        } else {
            self.clear_decoded_image_cache();
        }
    }

    /// Set the decoded-image cache byte limit, mirroring
    /// `TVPSetGraphicCacheLimit` (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:2423-2441`):
    /// `0` disables the cache; `-1` selects the system limit; every other
    /// value — including a negative one below `-1`, which the reference's
    /// `tjs_uint64` parameter converts to a huge unsigned quantity — is
    /// clamped to the system limit. Entries are then evicted down to the new
    /// limit.
    ///
    /// The reference derives its system limit from physical memory at startup
    /// and caps it at 512 MiB
    /// (`reference/cpp/core/base/impl/SysInitImpl.cpp:235-277`); the port has
    /// no memory probe, so the cap is the system limit.
    pub fn set_graphic_cache_limit(&mut self, limit: i64) {
        const SYSTEM_LIMIT: u64 = 512 * 1024 * 1024;
        self.set_decoded_image_cache_limit(match limit {
            0 => CacheLimit::Disabled,
            -1 => CacheLimit::bytes(SYSTEM_LIMIT),
            _ => CacheLimit::bytes((limit as u64).min(SYSTEM_LIMIT)),
        });
    }

    /// Drop every decoded-image cache entry (the reference
    /// `TVPClearGraphicCache`,
    /// `reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1456-1459`).
    pub fn clear_decoded_image_cache(&mut self) {
        self.image_cache.entries.clear();
        self.image_cache.order.clear();
        self.image_cache.total_bytes = 0;
    }

    /// Return the cached decoded image for `key`, or run `decode`, cache the
    /// result and return it.
    ///
    /// A caller receives an owned copy, so mutating the returned pixels
    /// cannot corrupt a later cache hit. The reference shares a refcounted
    /// bitmap and copies on write
    /// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:641-664`), which
    /// is observably the same. When the cache is disabled the decode always
    /// runs and nothing is stored.
    ///
    /// # Errors
    ///
    /// Returns `decode`'s error unchanged when the key is missing and the
    /// decode fails.
    pub fn cached_decoded_image<F, E>(
        &mut self,
        key: DecodedImageKey,
        decode: F,
    ) -> Result<DecodedImage, E>
    where
        F: FnOnce() -> Result<DecodedImage, E>,
    {
        if !self.image_cache.enabled() {
            return decode();
        }
        if let Some(image) = self.image_cache.take_copy(&key) {
            return Ok(image);
        }
        let image = decode()?;
        self.cache_decoded_image(key, image.clone());
        Ok(image)
    }

    /// Store `image` in the decoded-image cache under `key`.
    ///
    /// Nothing is stored while the cache is disabled. The entry replaces a
    /// previous one under the same key, and the least-recently-used entries
    /// are evicted down to the limit first — the accounting the synchronous
    /// load path uses.
    fn cache_decoded_image(&mut self, key: DecodedImageKey, image: DecodedImage) {
        if !self.image_cache.enabled() {
            return;
        }
        self.image_cache.insert(key, image);
    }

    /// The pool index of a bitmap id. Uses the id→index map, falling back to
    /// a linear scan if callers edited [`Scene::bitmaps`] directly.
    fn bitmap_position(&self, id: u32) -> Option<usize> {
        if let Some(index) = self.bitmap_index.get(&id).copied()
            && self.bitmaps.get(index).is_some_and(|b| b.id == id)
        {
            return Some(index);
        }
        self.bitmaps.iter().position(|b| b.id == id)
    }

    /// Look up a bitmap by id.
    pub fn bitmap(&self, id: u32) -> Option<&BitmapState> {
        self.bitmap_position(id)
            .and_then(|index| self.bitmaps.get(index))
    }

    /// Mutably look up a bitmap by id, bumping [`Scene::content_revision`]
    /// and the bitmap's [`BitmapState::content_generation`] (every mutable
    /// access is a possible content change). A dimension change is a
    /// structural change: write it through [`Scene::resize_bitmap`] or
    /// [`Scene::replace_bitmap_image`], which bump
    /// [`Scene::structure_revision`].
    pub fn bitmap_mut(&mut self, id: u32) -> Option<&mut BitmapState> {
        self.touch_content();
        let mut bitmap = self.bitmap_mut_untracked(id);
        if let Some(state) = bitmap.as_mut() {
            state.content_generation = state.content_generation.wrapping_add(1);
        }
        bitmap
    }

    /// Replace a bitmap's image with `width` x `height` RGBA pixels, marking
    /// it dirty. A changed size is structural (the projection reads the
    /// bitmap dimensions through the layer's image size), so it bumps
    /// [`Scene::structure_revision`]; every replace bumps
    /// [`Scene::content_revision`]. Script ownership is left alone. Returns
    /// `false` when the bitmap is gone.
    ///
    /// A pending decode is cancelled: the replaced image supersedes it, and
    /// a waiter on it sees the cancellation instead of the stale pixels. A
    /// decode another bitmap waits for is left alone.
    pub fn replace_bitmap_image(
        &mut self,
        id: u32,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    ) -> bool {
        let current = self
            .bitmap(id)
            .map(|bitmap| ((bitmap.width, bitmap.height), bitmap.pending.clone()));
        let Some((previous, pending)) = current else {
            return false;
        };
        // The decode the bitmap carries is superseded by the replaced image,
        // unless another bitmap waits for the same decode.
        let cancel_pending = pending
            .as_ref()
            .is_some_and(|pending| !self.another_bitmap_waits_for(pending, id));
        let Some(bitmap) = self.bitmap_mut(id) else {
            return false;
        };
        if let Some(pending) = bitmap.pending.take()
            && cancel_pending
        {
            pending.cancel();
        }
        bitmap.width = width;
        bitmap.height = height;
        bitmap.rgba = rgba;
        bitmap.mark_dirty();
        if previous != (width, height) {
            self.touch_structure();
        }
        true
    }

    /// Mutably look up a bitmap by id without bumping either revision or the
    /// content generation. Used by a pass that writes one bitmap many times
    /// and reports the change once.
    pub(crate) fn bitmap_mut_untracked(&mut self, id: u32) -> Option<&mut BitmapState> {
        let index = self.bitmap_position(id)?;
        self.bitmaps.get_mut(index)
    }

    /// Borrow the composite target `dst` mutably and one source `src`
    /// immutably at the same time. Unlike [`Scene::bitmap_mut`] this bumps
    /// neither revision nor a content generation: a composite takes one
    /// mutable borrow per pass and samples many sources. It does mark the
    /// scene changed, because the composite writes the target's pixels.
    /// Returns `None` when an id is missing or `dst` and `src` are the
    /// same bitmap, because a composite never samples the bitmap it writes.
    pub fn bitmap_mut_with_source(
        &mut self,
        dst: u32,
        src: u32,
    ) -> Option<(&mut BitmapState, &BitmapState)> {
        self.mark_dirty();
        let di = self.bitmap_position(dst)?;
        let si = self.bitmap_position(src)?;
        if di == si {
            return None;
        }
        let (left, right) = self.bitmaps.split_at_mut(di);
        let (dst_slice, right) = right.split_at_mut(1);
        let source = if si < di {
            &left[si]
        } else {
            &right[si - di - 1]
        };
        Some((&mut dst_slice[0], source))
    }

    /// Borrow `dst` mutably and `src1`/`src2` immutably at the same time. The
    /// two sources may alias each other. Returns `None` when an id is missing
    /// or `dst` aliases a source; callers then take a copying path. Unlike
    /// [`Scene::bitmap_mut`] this does not bump [`Scene::content_revision`]: a
    /// whole-subtree transition rewrites the same frame bitmap every tick
    /// without changing the scene graph. It does mark the scene changed,
    /// because the transition rewrites the frame's pixels.
    pub fn bitmap_mut_with_refs(
        &mut self,
        dst: u32,
        src1: u32,
        src2: u32,
    ) -> Option<(&mut BitmapState, &BitmapState, &BitmapState)> {
        self.mark_dirty();
        let di = self.bitmaps.iter().position(|b| b.id == dst)?;
        let a = self.bitmaps.iter().position(|b| b.id == src1)?;
        let b = self.bitmaps.iter().position(|b| b.id == src2)?;
        if di == a || di == b {
            return None;
        }
        let (left, right) = self.bitmaps.split_at_mut(di);
        let (dst_slice, right) = right.split_at_mut(1);
        let a_ref: &BitmapState = if a < di { &left[a] } else { &right[a - di - 1] };
        let b_ref: &BitmapState = if b < di { &left[b] } else { &right[b - di - 1] };
        Some((&mut dst_slice[0], a_ref, b_ref))
    }

    /// Set the frame bitmap a whole-subtree transition draws, without
    /// touching the composited content. Bumps [`Scene::structure_revision`]
    /// only when the id actually changes. The replaced bitmap is released
    /// once nothing else references it.
    pub fn set_cache_bitmap(&mut self, id: u32, bitmap: Option<u32>) {
        let mut replaced = None;
        let changed = match self.layer_mut_untracked(id) {
            Some(layer) if layer.cache_bitmap != bitmap => {
                replaced = layer.cache_bitmap;
                layer.cache_bitmap = bitmap;
                true
            }
            _ => false,
        };
        if changed {
            self.touch_structure();
            if let Some(old) = replaced {
                self.release_bitmap(old);
            }
        }
    }

    /// Set the image drawn in place of this layer's own image, without
    /// touching its subtree. Bumps [`Scene::structure_revision`] and marks
    /// the layer's cache and every caching ancestor's stale when the id
    /// actually changes; a cache that already composited the previous source
    /// must not keep showing it. The replaced bitmap is released once nothing
    /// else references it.
    pub fn set_transition_self_image(&mut self, id: u32, bitmap: Option<u32>) {
        let mut replaced = None;
        let changed = match self.layer_mut_untracked(id) {
            Some(layer) if layer.transition_self != bitmap => {
                replaced = layer.transition_self;
                layer.transition_self = bitmap;
                true
            }
            _ => false,
        };
        if changed {
            self.invalidate_layer_cache(id);
            self.touch_structure();
            if let Some(old) = replaced {
                self.release_bitmap(old);
            }
        }
    }

    /// Set (or clear) the frame a running transition draws on `id`.
    ///
    /// A changed frame marks every caching ancestor's cache stale and bumps
    /// [`Scene::structure_revision`], so the projection redraws the two source
    /// images the frame advances to. The layer's own composited content is the
    /// frame's source and does not change with the frame, so it stays reusable
    /// instead of being recomposited every tick. A source the outgoing frame
    /// named and the new one does not is released once nothing else holds it;
    /// the transition's own sources keep their owners' references.
    pub fn set_transition_frame(&mut self, id: u32, frame: Option<TransitionFrame>) {
        self.set_transition_frame_except(id, None, frame);
    }

    /// Set a running transition's frame while keeping the transition source
    /// layer's own cache current. When the source is one of the destination's
    /// ancestors its composited content is also the frame source, so the frame
    /// change must not recomposite it; only the other caching ancestors need
    /// the new frame.
    pub fn set_transition_frame_for(
        &mut self,
        id: u32,
        source: u32,
        frame: Option<TransitionFrame>,
    ) {
        self.set_transition_frame_except(id, Some(source), frame);
    }

    fn set_transition_frame_except(
        &mut self,
        id: u32,
        source: Option<u32>,
        frame: Option<TransitionFrame>,
    ) {
        let mut replaced = None;
        let changed = match self.layer_mut_untracked(id) {
            Some(layer) if layer.transition_frame != frame => {
                replaced = layer.transition_frame;
                layer.transition_frame = frame;
                true
            }
            _ => false,
        };
        if changed {
            if frame.is_none() {
                // The layer returns to drawing its own subtree: a cache kept
                // from before the transition (or the one its transition ran
                // through) is stale now, so resume from a fresh composite.
                self.invalidate_layer_cache(id);
            } else {
                self.invalidate_ancestor_caches_except(id, source);
            }
            self.touch_structure();
            if let Some(replaced) = replaced {
                let kept = frame.map(|frame| [frame.src1.bitmap, frame.src2.bitmap]);
                for bitmap in [replaced.src1.bitmap, replaced.src2.bitmap] {
                    if kept.is_some_and(|kept| kept.contains(&bitmap)) {
                        continue;
                    }
                    self.release_bitmap(bitmap);
                }
            }
        }
    }

    /// Set (or clear) the custom-shader draw a running transition composites
    /// on `id`.
    ///
    /// A changed draw marks every caching ancestor's cache stale and bumps
    /// [`Scene::structure_revision`], so the projection redraws it. The
    /// layer's own composited content is one of the draw's sources and does
    /// not change with the draw, so it stays reusable instead of being
    /// recomposited every tick. A source the outgoing draw named and the new
    /// one does not is released once nothing else holds it; the transition's
    /// own sources keep their owners' references.
    pub fn set_effect(&mut self, id: u32, effect: Option<EffectDraw>) {
        self.set_effect_except(id, None, effect);
    }

    /// Set a running transition's effect draw while keeping the transition
    /// source layer's own cache current, for the same reason as
    /// [`Scene::set_transition_frame_for`].
    pub fn set_effect_for(&mut self, id: u32, source: u32, effect: Option<EffectDraw>) {
        self.set_effect_except(id, Some(source), effect);
    }

    fn set_effect_except(&mut self, id: u32, source: Option<u32>, effect: Option<EffectDraw>) {
        let cleared = effect.is_none();
        let kept: Vec<u32> = effect
            .as_ref()
            .map(|effect| effect.bitmaps().collect())
            .unwrap_or_default();
        let unchanged = match self.layer(id) {
            Some(layer) => layer.effect.as_ref() == effect.as_ref(),
            None => true,
        };
        let mut replaced = None;
        let changed = if unchanged {
            false
        } else if let Some(layer) = self.layer_mut_untracked(id) {
            replaced = layer.effect.take();
            layer.effect = effect;
            true
        } else {
            false
        };
        if changed {
            if cleared {
                // The layer returns to drawing its own subtree: a cache kept
                // from before the transition (or the one its transition ran
                // through) is stale now, so resume from a fresh composite.
                self.invalidate_layer_cache(id);
            } else {
                self.invalidate_ancestor_caches_except(id, source);
            }
            self.touch_structure();
            if let Some(replaced) = replaced {
                for bitmap in replaced.bitmaps() {
                    if kept.contains(&bitmap) {
                        continue;
                    }
                    self.release_bitmap(bitmap);
                }
            }
        }
    }

    /// The custom-shader draw a transition composites at its current phase,
    /// or `None` when this build cannot draw it.
    ///
    /// `None` means the transition blends on the CPU: the kind has no program,
    /// or the scene has no pipeline for the program's blend.
    pub fn transition_effect_draw(
        &self,
        trans: &Trans,
        src1: u32,
        src2: u32,
        size: (u32, u32),
        covers_layer: bool,
    ) -> Option<EffectDraw> {
        let draw = trans.effect_draw(src1, src2, size, covers_layer)?;
        // Every pass must name a program the scene can draw; a draw that
        // names one it cannot is not drawn at all, and the transition falls
        // back to the CPU blend.
        if draw
            .passes
            .iter()
            .all(|pass| self.effect_pipelines.contains(pass.program))
        {
            Some(draw)
        } else {
            None
        }
    }

    /// Clear a bitmap's [`BitmapState::dirty`] flag and its
    /// [`BitmapState::dirty_region`] without bumping either revision. Called
    /// by the projection after a texture upload; the flag is backend
    /// bookkeeping, not a scene change.
    pub fn set_bitmap_clean(&mut self, id: u32) -> bool {
        let Some(bitmap) = self.bitmap_record_mut(id) else {
            return false;
        };
        bitmap.dirty = false;
        bitmap.dirty_region = None;
        true
    }

    /// Marks a bitmap's dirty region as the whole bitmap.
    ///
    /// The frame's upload sends every pixel of a bitmap whose texture was
    /// just created, because the rest of a new texture is undefined until
    /// its first copy. Marking the bitmap wholly keeps a retry of that
    /// upload whole as well.
    pub fn mark_bitmap_whole(&mut self, id: u32) -> bool {
        let Some(bitmap) = self.bitmap_record_mut(id) else {
            return false;
        };
        bitmap.dirty_region = None;
        true
    }

    /// The bitmap record with the given id, for mutation.
    ///
    /// The id-indexed lookup is the fast path; the scan covers a bitmap whose
    /// index entry is missing.
    fn bitmap_record_mut(&mut self, id: u32) -> Option<&mut BitmapState> {
        let index = self.bitmap_index.get(&id).copied();
        if let Some(index) = index
            && self
                .bitmaps
                .get(index)
                .is_some_and(|bitmap| bitmap.id == id)
        {
            return self.bitmaps.get_mut(index);
        }
        self.bitmaps.iter_mut().find(|b| b.id == id)
    }

    /// Add a font record (`face`, pixel `height`, RGBA `color`) and return
    /// its new id. Style flags default off and `angle` to 0.
    pub fn add_font(&mut self, face: String, height: i32, color: [u8; 4]) -> u32 {
        let id = self.next_font;
        self.next_font += 1;
        self.font_index.insert(id, self.fonts.len());
        self.fonts.push(FontState {
            id,
            face,
            height,
            color,
            bold: false,
            italic: false,
            strikeout: false,
            underline: false,
            angle: 0.0,
        });
        self.touch_structure();
        id
    }

    /// Reset a scene font in place to the default style with the given face,
    /// height and color. The reference `tTJSNI_Font::Construct` with no layer
    /// argument replaces the native font with the default font, so the style
    /// fields reset rather than preserving a pre-constructor write. Returns
    /// `false` when the font is gone.
    pub fn reset_font(&mut self, id: u32, face: String, height: i32, color: [u8; 4]) -> bool {
        let Some(font) = self.fonts.iter_mut().find(|font| font.id == id) else {
            return false;
        };
        font.face = face;
        font.height = height;
        font.color = color;
        font.bold = false;
        font.italic = false;
        font.strikeout = false;
        font.underline = false;
        font.angle = 0.0;
        self.touch_structure();
        true
    }

    /// Look up a font by id. Uses the id→index map, falling back to a
    /// linear scan if callers edited [`Scene::fonts`] directly.
    pub fn font(&self, id: u32) -> Option<&FontState> {
        if let Some(index) = self.font_index.get(&id).copied()
            && let Some(font) = self.fonts.get(index)
            && font.id == id
        {
            return Some(font);
        }
        self.fonts.iter().find(|f| f.id == id)
    }

    /// All layers of a window flattened back -> front.
    ///
    /// A parent's complete subtree is emitted before the next sibling. This
    /// is important for TVP's layer tree: a parent is a backdrop/container,
    /// while its children are composited in front of it. Sibling `z_order`
    /// remains the primary key and the maintained sibling vectors provide the
    /// stable tie-breaker (including `bringToFront`). Invalid or cyclic links
    /// are handled defensively and cannot make a layer disappear or recurse
    /// forever.
    pub fn window_layer_order(&self, window: u32) -> Vec<u32> {
        let Some(window_state) = self.window(window) else {
            return Vec::new();
        };

        // One pass over this window's layers builds the `parent → all
        // children` index (shutdown layers excluded, so they can never be
        // discovered during the walk below). The old implementation re-scanned
        // the entire layer table for every layer, which made this O(n²); the
        // index makes it a single O(n) pass plus the sibling sorts below.
        let mut all_children: Map<Option<u32>, Vec<u32>> = map();
        for layer in self
            .layers
            .iter()
            .filter(|l| l.window == window && !l.shutdown)
        {
            all_children.entry(layer.parent).or_default().push(layer.id);
        }

        // Root list: only the layers the window actually owns. A layer whose
        // parent was severed is deliberately **not** adopted here: the draw
        // order is the tree reachable from the window's `Primary` layer, so a
        // `Part()`ed layer stops being drawn and hit-tested. Adopting every
        // `parent == None` layer would put detached, invalidated subtrees back
        // into the draw order as top-level window layers.
        let mut roots: Vec<u32> = window_state
            .layers
            .iter()
            .copied()
            .filter(|id| self.layer(*id).is_some_and(|l| !l.shutdown))
            .collect();
        let root_order = roots.clone();
        self.sort_siblings(&mut roots, &root_order);

        let mut order = Vec::new();
        let mut visited = set();
        for id in roots {
            self.append_layer_subtree(window, id, &all_children, &mut order, &mut visited);
        }
        // Only layers reachable from the window's roots are emitted. A layer
        // the root walk does not visit is detached — a `Part()`ed layer or one
        // whose parent link is stale — so it stays out of both the draw order
        // and hit-testing. A child genuinely missing from its parent's
        // `children` list is still found: `all_children` is indexed by the
        // layer's own `parent` field and `append_layer_subtree` merges it into
        // the maintained list.
        order
    }

    /// Stable sibling sort keyed by `(z_order, maintained position, id)`.
    ///
    /// `maintained` is the scene's insertion/maintained sibling order; ids
    /// not present in it (children discovered only through the parent index)
    /// sort after it by id, exactly like the previous `Vec::position`-based
    /// comparator. A position map replaces that O(n) `position` scan.
    fn sort_siblings(&self, ids: &mut [u32], maintained: &[u32]) {
        if ids.len() <= 1 {
            return;
        }
        if ids == maintained {
            // Common case: the group is already in maintained insertion
            // order, so the position tie-breaker is the current index. A
            // *stable* sort by z-order reproduces the exact
            // `(z, position, id)` order without allocating a position map.
            ids.sort_by_key(|&id| self.layer(id).map_or(0, |l| l.z_order));
            return;
        }
        let mut position: Map<u32, usize> = map();
        for (index, &id) in maintained.iter().enumerate() {
            position.insert(id, index);
        }
        // Resolve each id's sort key once (an O(1) layer lookup) instead of
        // re-resolving it on every comparison. The key includes `id`, so all
        // keys are unique and an unstable sort is deterministic.
        let mut keyed: Vec<(i32, usize, u32)> = ids
            .iter()
            .map(|&id| {
                let z_order = self.layer(id).map_or(0, |l| l.z_order);
                let pos = position.get(&id).copied().unwrap_or(usize::MAX);
                (z_order, pos, id)
            })
            .collect();
        keyed.sort_unstable();
        for (slot, &(_, _, id)) in ids.iter_mut().zip(keyed.iter()) {
            *slot = id;
        }
    }

    fn append_layer_subtree(
        &self,
        window: u32,
        id: u32,
        all_children: &Map<Option<u32>, Vec<u32>>,
        order: &mut Vec<u32>,
        visited: &mut Set<u32>,
    ) {
        let Some(layer) = self.layer(id) else { return };
        if layer.window != window || layer.shutdown || !visited.insert(id) {
            return;
        }
        order.push(id);

        let mut children = layer.children.clone();
        if let Some(children_of) = all_children.get(&Some(id))
            && children != *children_of
        {
            // A child exists in the parent index but is missing from the
            // maintained `children` list: merge it in. The common case (both
            // lists agree) skips the set allocation entirely.
            let mut seen: Set<u32> = set();
            for &child in children.iter() {
                seen.insert(child);
            }
            for &child in children_of {
                if seen.insert(child) {
                    children.push(child);
                }
            }
        }
        self.sort_siblings(&mut children, &layer.children);
        for child in children {
            self.append_layer_subtree(window, child, all_children, order, visited);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    /// The layer owns its cache bitmap and must free the replaced one: the
    /// reference deletes `CacheBitmap` in `DeallocateCache` and never drops
    /// the pointer without deleting the old object
    /// (`reference/cpp/core/visual/LayerIntf.cpp:3975-3979`).
    #[test]
    fn set_cache_bitmap_releases_the_replaced_bitmap() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (2, 2));
        let layer = scene.add_layer(win, None);
        let old = scene.add_bitmap(2, 2, vec![1u8; 16]);
        let new = scene.add_bitmap(2, 2, vec![2u8; 16]);

        scene.set_cache_bitmap(layer, Some(old));
        assert!(scene.bitmap(old).is_some());
        scene.set_cache_bitmap(layer, Some(new));

        assert!(scene.bitmap(new).is_some(), "the new cache survives");
        assert!(
            scene.bitmap(old).is_none(),
            "the replaced cache is released"
        );
    }

    /// A synthetic RGBA image whose buffer is exactly `bytes` long (a
    /// multiple of four), so the cache's byte accounting can be asserted
    /// without decoding a real file.
    fn cache_image(bytes: usize) -> DecodedImage {
        DecodedImage::new((bytes / 4) as u32, 1, vec![0x5a; bytes])
    }

    /// The load key of a full-colour, unkeyed, unscaled load of `name`.
    fn cache_key(name: &str) -> DecodedImageKey {
        DecodedImageKey::new(name, 0, DecodedImageMode::Full, 0, 0)
    }

    /// Load `name` through the scene's decoded-image cache, counting how
    /// often the decode closure actually runs; a `bytes`-sized image would
    /// be produced on every miss.
    fn load_cached(scene: &mut Scene, decodes: &Cell<usize>, name: &str, bytes: usize) {
        scene
            .cached_decoded_image::<_, ()>(cache_key(name), || {
                decodes.set(decodes.get() + 1);
                Ok(cache_image(bytes))
            })
            .expect("the synthetic decode cannot fail");
    }

    /// Whether `name` currently sits in the cache.
    fn cached(scene: &Scene, name: &str) -> bool {
        scene.image_cache.entries.contains_key(&cache_key(name))
    }

    /// Inserting past the byte limit chops the least-recently-used entry
    /// first and the byte total tracks the survivors. The check runs on the
    /// old total before the new image is pushed
    /// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1951-1960`), so
    /// right after an insert the total may sit above the limit by the new
    /// entry's size; the chop loop (`:1441-1454`) then runs at the next
    /// insert.
    #[test]
    fn cache_insert_past_the_limit_evicts_the_lru_entry_first() {
        let decodes = Cell::new(0);
        let mut scene = Scene::default();
        scene.set_graphic_cache_limit(768); // room for three 256-byte images

        load_cached(&mut scene, &decodes, "a", 256);
        load_cached(&mut scene, &decodes, "b", 256);
        load_cached(&mut scene, &decodes, "c", 256);
        // reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1955-1958 and
        // :1447-1448: the total is the sum of the cached images' sizes.
        assert_eq!(decodes.get(), 3);
        assert_eq!(scene.image_cache.total_bytes, 768);

        // The fourth image pushes the total over the limit, but nothing is
        // chopped at its own insert.
        load_cached(&mut scene, &decodes, "d", 256);
        // reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1952-1960: the
        // check sees the old total, then the new image is added.
        assert_eq!(scene.image_cache.total_bytes, 1024);
        assert!(cached(&scene, "a"));

        // The fifth insert first chops the least-recently-used entry ("a"),
        // then pushes the new one.
        load_cached(&mut scene, &decodes, "e", 256);
        // reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1441-1454: while
        // the total is above the limit the last (least recently used) entry
        // is chopped.
        assert!(!cached(&scene, "a"), "the LRU entry is evicted first");
        assert!(cached(&scene, "b"));
        assert!(cached(&scene, "c"));
        assert!(cached(&scene, "d"));
        assert!(cached(&scene, "e"), "the new entry is stored");
        assert_eq!(scene.image_cache.total_bytes, 1024);

        // The survivors still hit; the evicted entry decodes again.
        load_cached(&mut scene, &decodes, "b", 256);
        load_cached(&mut scene, &decodes, "c", 256);
        load_cached(&mut scene, &decodes, "d", 256);
        load_cached(&mut scene, &decodes, "e", 256);
        assert_eq!(decodes.get(), 5, "the survivors hit the cache");
        load_cached(&mut scene, &decodes, "a", 256);
        assert_eq!(decodes.get(), 6, "the evicted entry decodes again");
    }

    /// A cache hit promotes the entry out of the least-recently-used slot,
    /// so it is not the one the next eviction chops
    /// (`reference/cpp/core/tjs2/tjsHashSearch.h:358-364`: find-and-touch
    /// updates the additional order, whose most-recent end is where adds and
    /// touches land and whose least-recent end is what eviction chops,
    /// `:507-527`).
    #[test]
    fn cache_hit_promotes_the_entry_against_eviction() {
        let decodes = Cell::new(0);
        let mut scene = Scene::default();
        scene.set_graphic_cache_limit(768);

        load_cached(&mut scene, &decodes, "a", 256);
        load_cached(&mut scene, &decodes, "b", 256);
        load_cached(&mut scene, &decodes, "c", 256);

        // The hit on "a" promotes it; it does not decode again.
        // reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1895-1897: a
        // found entry is returned without loading the image again.
        load_cached(&mut scene, &decodes, "a", 256);
        assert_eq!(decodes.get(), 3, "the hit does not decode");

        // The next two inserts chop "b" — now the LRU entry — while the
        // touched "a" survives.
        // reference/cpp/core/tjs2/tjsHashSearch.h:542-554: the touch moves
        // the entry to the most-recent end.
        load_cached(&mut scene, &decodes, "d", 256);
        load_cached(&mut scene, &decodes, "e", 256);
        assert!(!cached(&scene, "b"), "the untouched entry is evicted");
        assert!(cached(&scene, "a"), "the touched entry survives");
        assert!(cached(&scene, "c"));
        assert!(cached(&scene, "d"));
        assert!(cached(&scene, "e"));

        load_cached(&mut scene, &decodes, "a", 256);
        assert_eq!(decodes.get(), 5, "the promoted entry still hits");
        load_cached(&mut scene, &decodes, "b", 256);
        assert_eq!(decodes.get(), 6, "the un-promoted entry decodes again");
    }

    /// Eviction runs at the start of an insert, so an image bigger than the
    /// whole limit is admitted alone — the check sees the old total and the
    /// size guard is commented out
    /// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1951-1960`,
    /// `:1956-1957`) — but it is chopped at the very next insert
    /// (`:1441-1454`) and never survives alongside another cached image.
    #[test]
    fn oversized_image_is_chopped_at_the_next_insert() {
        let decodes = Cell::new(0);
        let mut scene = Scene::default();
        scene.set_graphic_cache_limit(512);

        // Bigger than the whole limit: admitted on its own, over the limit.
        load_cached(&mut scene, &decodes, "huge", 1024);
        assert_eq!(scene.image_cache.total_bytes, 1024);
        load_cached(&mut scene, &decodes, "huge", 1024);
        assert_eq!(decodes.get(), 1, "it is temporarily a cache hit");

        // The next insert chops it first: the loop sees 1024 > 512 before
        // the new image is pushed.
        load_cached(&mut scene, &decodes, "small", 256);
        assert!(!cached(&scene, "huge"), "the oversized entry is chopped");
        assert!(cached(&scene, "small"));
        assert_eq!(scene.image_cache.total_bytes, 256);

        load_cached(&mut scene, &decodes, "huge", 1024);
        assert_eq!(decodes.get(), 3, "the oversized entry decodes again");
    }

    /// The typed limit distinguishes no cache, unlimited caching and a byte
    /// cap. `Option` follows the same encoding: `None` disables, `Some(0)` is
    /// unlimited, `Some(n)` is a byte cap.
    #[test]
    fn cache_limit_option_distinguishes_disabled_unlimited_and_bytes() {
        assert_eq!(CacheLimit::from(None), CacheLimit::Disabled);
        assert_eq!(CacheLimit::from(Some(0)), CacheLimit::Unlimited);
        assert_eq!(CacheLimit::from(Some(256)), CacheLimit::bytes(256));
        assert!(!CacheLimit::Disabled.is_enabled());
        assert!(CacheLimit::Unlimited.is_enabled());
        assert_eq!(CacheLimit::Unlimited.option(), Some(0));
        assert_eq!(CacheLimit::bytes(256).option(), Some(256));
        assert_eq!(CacheLimit::bytes(0), CacheLimit::Unlimited);
    }

    /// An unlimited cache keeps every entry until it is replaced by a finite
    /// cap or disabled. Disabling clears the entries immediately.
    #[test]
    fn unlimited_cache_keeps_entries_until_disabled() {
        let decodes = Cell::new(0);
        let mut scene = Scene::default();
        scene.set_decoded_image_cache_limit(CacheLimit::Unlimited);
        assert_eq!(scene.decoded_image_cache_limit(), CacheLimit::Unlimited);

        load_cached(&mut scene, &decodes, "a", 256);
        load_cached(&mut scene, &decodes, "b", 256);
        load_cached(&mut scene, &decodes, "c", 256);
        assert_eq!(scene.image_cache.total_bytes, 768);
        load_cached(&mut scene, &decodes, "a", 256);
        assert_eq!(decodes.get(), 3, "the unlimited cache hits");

        scene.set_decoded_image_cache_limit(Some(256));
        assert_eq!(scene.decoded_image_cache_limit(), CacheLimit::bytes(256));
        assert_eq!(scene.image_cache.total_bytes, 256);
        assert!(cached(&scene, "a"), "the most recent entry survives");

        scene.set_decoded_image_cache_limit(None);
        assert_eq!(scene.decoded_image_cache_limit(), CacheLimit::Disabled);
        assert!(scene.image_cache.entries.is_empty());
        assert_eq!(scene.image_cache.total_bytes, 0);
        load_cached(&mut scene, &decodes, "a", 256);
        load_cached(&mut scene, &decodes, "a", 256);
        assert_eq!(decodes.get(), 5, "the disabled cache never hits");
    }

    /// A limit of zero disables the cache: existing entries are evicted by
    /// the setter's own check
    /// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:2441`) and every
    /// later load decodes and stores nothing (`:2425-2427`, `:1886`).
    #[test]
    fn cache_limit_zero_disables_future_caching() {
        let decodes = Cell::new(0);
        let mut scene = Scene::default();
        scene.set_graphic_cache_limit(768);
        load_cached(&mut scene, &decodes, "a", 256);
        load_cached(&mut scene, &decodes, "b", 256);
        assert_eq!(scene.image_cache.total_bytes, 512);

        scene.set_graphic_cache_limit(0);
        assert_eq!(scene.graphic_cache_limit(), 0);
        assert_eq!(scene.image_cache.total_bytes, 0);
        assert!(scene.image_cache.entries.is_empty());

        // Every call decodes and stores nothing.
        load_cached(&mut scene, &decodes, "a", 256);
        load_cached(&mut scene, &decodes, "a", 256);
        load_cached(&mut scene, &decodes, "b", 256);
        assert_eq!(decodes.get(), 5, "the disabled cache never hits");
        assert_eq!(scene.image_cache.total_bytes, 0);
        assert!(scene.image_cache.entries.is_empty());

        // Re-enabling restores caching, so the decodes above were the
        // disabled path.
        scene.set_graphic_cache_limit(768);
        load_cached(&mut scene, &decodes, "a", 256);
        load_cached(&mut scene, &decodes, "a", 256);
        assert_eq!(decodes.get(), 6, "the re-enabled cache hits again");
    }

    /// Nonzero limits are clamped to the 512 MiB system cap. A negative
    /// value below -1 reaches the reference's `tjs_uint64` parameter as a
    /// huge unsigned quantity and clamps the same way
    /// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:2423-2441`), and
    /// a value within the cap is kept as-is.
    #[test]
    fn cache_limit_clamps_to_the_system_cap() {
        const SYSTEM_LIMIT: u64 = 512 * 1024 * 1024;
        let mut scene = Scene::default();

        // reference/cpp/core/visual/GraphicsLoaderIntf.cpp:2428-2430: -1
        // selects the system limit; :2438-2439 caps it at 512 MiB.
        scene.set_graphic_cache_limit(-1);
        assert_eq!(scene.graphic_cache_limit(), SYSTEM_LIMIT);

        // reference/cpp/core/visual/GraphicsLoaderIntf.cpp:2432-2433: a
        // value above the system limit clamps to it.
        scene.set_graphic_cache_limit(1 << 40);
        assert_eq!(scene.graphic_cache_limit(), SYSTEM_LIMIT);
        scene.set_graphic_cache_limit(i64::MAX);
        assert_eq!(scene.graphic_cache_limit(), SYSTEM_LIMIT);

        // A negative value below -1 converts to a huge tjs_uint64
        // (reference/cpp/core/visual/GraphicsLoaderIntf.cpp:2423) and
        // clamps the same way.
        scene.set_graphic_cache_limit(-2);
        assert_eq!(scene.graphic_cache_limit(), SYSTEM_LIMIT);

        scene.set_graphic_cache_limit(1 << 20);
        assert_eq!(scene.graphic_cache_limit(), 1 << 20);
    }

    /// Lowering the limit evicts down to it, keeping the most recently used
    /// entries (the chop loop at the end of the setter,
    /// `reference/cpp/core/visual/GraphicsLoaderIntf.cpp:2441` + `:1441-1454`).
    #[test]
    fn lowering_the_cache_limit_evicts_down_to_it() {
        let decodes = Cell::new(0);
        let mut scene = Scene::default();
        scene.set_graphic_cache_limit(768);
        load_cached(&mut scene, &decodes, "a", 256);
        load_cached(&mut scene, &decodes, "b", 256);
        load_cached(&mut scene, &decodes, "c", 256);

        scene.set_graphic_cache_limit(256);
        assert!(!cached(&scene, "a"));
        assert!(!cached(&scene, "b"));
        assert!(cached(&scene, "c"));
        assert_eq!(scene.image_cache.total_bytes, 256);
    }

    /// Clearing the cache drops every entry and zeroes the byte total while
    /// the limit is kept
    /// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1456-1459`).
    #[test]
    fn clear_cache_empties_entries_and_zeroes_total_bytes() {
        let decodes = Cell::new(0);
        let mut scene = Scene::default();
        scene.set_graphic_cache_limit(768);
        load_cached(&mut scene, &decodes, "a", 256);
        load_cached(&mut scene, &decodes, "b", 256);
        load_cached(&mut scene, &decodes, "c", 256);
        assert_eq!(scene.image_cache.total_bytes, 768);

        scene.clear_decoded_image_cache();
        assert_eq!(scene.image_cache.total_bytes, 0);
        assert!(scene.image_cache.entries.is_empty());
        assert!(scene.image_cache.order.is_empty());
        assert_eq!(scene.graphic_cache_limit(), 768, "the limit is kept");

        // The next load of a cleared image decodes again.
        load_cached(&mut scene, &decodes, "a", 256);
        assert_eq!(decodes.get(), 4);
    }

    /// Out of the box the cache is disabled: the reference starts with
    /// `TVPGraphicCacheEnabled = false` and a zero limit
    /// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1433-1434`) and
    /// skips the lookup entirely while disabled (`:1886-1896`), so every
    /// call decodes and nothing is stored.
    #[test]
    fn cache_is_disabled_by_default() {
        let decodes = Cell::new(0);
        let mut scene = Scene::default();
        assert_eq!(scene.graphic_cache_limit(), 0);

        load_cached(&mut scene, &decodes, "a", 256);
        load_cached(&mut scene, &decodes, "a", 256);
        assert_eq!(decodes.get(), 2, "the default cache never hits");
        assert_eq!(scene.image_cache.total_bytes, 0);
        assert!(scene.image_cache.entries.is_empty());
        assert!(scene.image_cache.order.is_empty());
    }

    /// Every parentless layer founds its own focus domain (the reference
    /// creates one `tTVPLayerManager` per parentless layer,
    /// `reference/cpp/core/visual/LayerIntf.cpp:461-473`), and a child
    /// inherits its parent's (`:449-455`). The domain id is the founding
    /// root's layer id.
    #[test]
    fn parentless_layers_found_domains_and_children_inherit_them() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1, 1));
        let a = scene.add_layer(win, None);
        let a_child = scene.add_layer(win, Some(a));
        let b = scene.add_layer(win, None);

        assert_eq!(scene.layer(a).unwrap().domain, a);
        assert_eq!(scene.layer(a_child).unwrap().domain, a);
        assert_eq!(scene.layer(b).unwrap().domain, b);
        assert_ne!(a, b, "the siblings are separate domains");
        assert_eq!(scene.layer(a_child).unwrap().parent, Some(a));
    }

    /// Focus is per domain: moving one domain's focus leaves the other
    /// domain untouched, and the window keeps reporting the input domain
    /// (domain 0) — `reference/cpp/core/visual/LayerManager.h:483-490`,
    /// `reference/cpp/core/visual/LayerTreeOwnerImpl.h:21`.
    #[test]
    fn focus_is_isolated_between_domains() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1, 1));
        let a = scene.add_layer(win, None);
        let b = scene.add_layer(win, None);
        let a_domain = scene.layer(a).unwrap().domain;
        let b_domain = scene.layer(b).unwrap().domain;

        assert_eq!(scene.window_input_root(win), Some(a_domain));
        scene.set_domain_focus(win, a_domain, Some(a));
        scene.set_domain_focus(win, b_domain, Some(b));

        assert_eq!(scene.focused_in(win, a_domain), Some(a));
        assert_eq!(scene.focused_in(win, b_domain), Some(b));
        assert_eq!(
            scene.window(win).unwrap().focused_layer,
            Some(a),
            "the window-level focus stays on the input domain"
        );
        scene.set_domain_focus(win, b_domain, None);
        assert_eq!(scene.focused_in(win, a_domain), Some(a));
        assert_eq!(scene.focused_in(win, b_domain), None);
    }

    /// The focus search wraps only inside the receiver's domain; a sibling
    /// parentless root is invisible to it
    /// (`reference/cpp/core/visual/LayerIntf.cpp:1260-1283`).
    #[test]
    fn focus_search_stays_inside_the_domain() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1, 1));
        let a = scene.add_layer(win, None);
        let a2 = scene.add_layer(win, Some(a));
        let b = scene.add_layer(win, None);
        for id in [a, a2, b] {
            let layer = scene.layer_mut(id).unwrap();
            layer.visible = true;
            layer.focusable = true;
        }

        assert_eq!(scene.next_focusable(a), Some(a2));
        assert_eq!(scene.prev_focusable(a), Some(a2));
        assert_eq!(scene.next_focusable(a2), Some(a));
        assert_eq!(
            scene.next_focusable(b),
            None,
            "b's domain has no other layer"
        );
    }

    /// `set_focus`/`clear_focus` are the window-level (input-domain) shape:
    /// a layer of another domain is still moved into domain 0, matching the
    /// reference setter
    /// (`reference/cpp/core/visual/LayerTreeOwnerImpl.cpp:385-389`).
    #[test]
    fn set_focus_moves_the_input_domain_regardless_of_the_layer_domain() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1, 1));
        let a = scene.add_layer(win, None);
        let b = scene.add_layer(win, None);
        let a_domain = scene.layer(a).unwrap().domain;
        let b_domain = scene.layer(b).unwrap().domain;

        assert_eq!(scene.set_focus(b), (None, Some(b)));
        assert_eq!(scene.focused_in(win, a_domain), Some(b));
        assert_eq!(scene.focused_in(win, b_domain), None);
        assert_eq!(scene.window(win).unwrap().focused_layer, Some(b));
        assert_eq!(scene.clear_focus(win), Some(b));
        assert_eq!(scene.focused_in(win, a_domain), None);
    }

    /// Removing a parentless root unregisters its manager
    /// (`reference/cpp/core/visual/LayerIntf.cpp:533-535`;
    /// `reference/cpp/core/visual/LayerTreeOwnerImpl.cpp:99-108`), clears
    /// its focus and promotes the next root to the input domain.
    #[test]
    fn removing_a_root_unregisters_its_domain_and_promotes_the_next() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1, 1));
        let a = scene.add_layer(win, None);
        let a_child = scene.add_layer(win, Some(a));
        let b = scene.add_layer(win, None);
        let a_domain = scene.layer(a).unwrap().domain;
        let b_domain = scene.layer(b).unwrap().domain;
        scene.set_domain_focus(win, a_domain, Some(a_child));

        scene.remove_layer(a);

        assert_eq!(
            scene.window_input_root(win),
            Some(b_domain),
            "b's manager is promoted once a's is unregistered"
        );
        assert_eq!(scene.focused_in(win, a_domain), None);
        assert_eq!(scene.window(win).unwrap().focused_layer, None);
        // The detached child survives and keeps the assigned domain, which
        // the reference fixes for the layer's whole life.
        assert_eq!(scene.layer(a_child).unwrap().domain, a_domain);
        assert!(scene.layer(a).is_none());
    }

    /// Destroying a whole subtree unregisters any parentless root inside it
    /// and drops focus holders anywhere in the scene.
    #[test]
    fn destroying_a_root_subtree_unregisters_its_domain() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1, 1));
        let a = scene.add_layer(win, None);
        let a_child = scene.add_layer(win, Some(a));
        let b = scene.add_layer(win, None);
        let a_domain = scene.layer(a).unwrap().domain;
        let b_domain = scene.layer(b).unwrap().domain;
        scene.set_domain_focus(win, b_domain, Some(b));

        scene.destroy_layer(a);

        assert!(scene.layer(a).is_none());
        assert!(scene.layer(a_child).is_none());
        assert_eq!(scene.focused_in(win, a_domain), None);
        assert_eq!(scene.window_input_root(win), Some(b_domain));
        assert_eq!(scene.focused_in(win, b_domain), Some(b));
    }
}
