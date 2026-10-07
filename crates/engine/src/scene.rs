//! TVP layer-system state model (L2).
//!
//! This is the state half of the middle layer: the logical `Scene` value and
//! its windows, layers, bitmaps and fonts, plus every CPU operation on them.
//! The caller owns the [`Scene`]; there are no globals, no locks and no
//! hidden caches. Everything is id-based, and operations on an unknown id are
//! no-ops.
//!
//! The module is deliberately backend-free and Bevy-free. It knows nothing
//! about rendering, script objects or native registration: the L1
//! `RenderPipeline` consumes a projected scene tree elsewhere, and the L3
//! `compat` native adapter calls into these operations. See
//! `docs/migration/02-target-architecture.md` §3 and §5.

use std::collections::{HashMap, HashSet};

/// One logical window.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct WindowState {
    /// Unique window id, stable for the lifetime of the scene.
    pub id: u32,
    /// Human-readable window title (used for the host window title).
    pub title: String,
    /// Game logical size in pixels.
    pub inner_size: (u32, u32),
    /// Whether the host shows the window.
    pub visible: bool,
    /// Window opacity in `0.0..=1.0`.
    pub opacity: f32,
    /// The layer scripts use as `window.primaryLayer`.
    pub primary_layer: Option<u32>,
    /// The layer that currently holds keyboard focus in this window
    /// (reference `tTVPLayerManager::FocusedLayer`), if any.
    pub focused_layer: Option<u32>,
    /// Render order, back -> front.
    pub layers: Vec<u32>,
}

/// TVP `ltOpaque` drawable/blend type (`Layer.type`).
pub const LT_OPAQUE: i64 = 1;
/// TVP `ltAlpha` drawable/blend type (`Layer.type`; the scene default).
pub const LT_ALPHA: i64 = 2;
/// TVP `ltAdditive` drawable/blend type (`Layer.type`).
pub const LT_ADDITIVE: i64 = 3;
/// TVP `ltSubtractive` drawable/blend type (`Layer.type`).
pub const LT_SUBTRACTIVE: i64 = 4;

/// Clamp a `u32` pixel dimension to a non-negative `i32` (a plain `as i32`
/// would wrap for sizes above `i32::MAX`).
fn dim_i32(v: u32) -> i32 {
    v.min(i32::MAX as u32) as i32
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
    /// TVP drawable/blend type (`ltAlpha` by default). Kept in the scene so
    /// the `RenderScene` projection does not lose `Layer.type` updates.
    pub blend_type: i64,
    /// Z order among sibling layers of the same window.
    pub z_order: i32,
    /// Solid fill (RGBA, straight alpha) used when there is no bitmap.
    pub fill_color: Option<[u8; 4]>,
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
    /// Reference `AbsoluteOrderMode`/`CallOnPaint` flags.
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
    /// Reference `tTJSNI_BaseLayer::Shutdown`: set by `Invalidate`
    /// (`LayerIntf.cpp:516`). A shut-down layer is inert — hit-testing,
    /// input dispatch, `onPaint` polling and rendering must not touch it.
    /// The record stays in the table only until the script reference drops
    /// and [`Scene::destroy_layer`] removes it.
    pub shutdown: bool,
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
    /// True when this bitmap is a window primary layer's **screen buffer**
    /// (synthesized by `composite_primary_layer` / `ensure_primary_bitmap`).
    /// The reference composites the layer tree into the primary layer's
    /// MainImage and scripts read it back (`piledCopy(0, 0,
    /// window.primaryLayer, …)`, `saveLayerImage`); it is never content to
    /// blit. Drawing it would replay the snapshot captured when the script
    /// last read it and leave stale imagery on screen.
    pub screen_buffer: bool,
    /// True when a script `Bitmap` object owns this id (created or attached
    /// through the `Bitmap` natives). A layer's MainImage may share the same
    /// id (`Layer.assignImages(bitmap)` attaches the source id directly), so
    /// destroying the layer must not free it while the script object can
    /// still read it.
    pub script_owned: bool,
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
            screen_buffer: false,
            script_owned: false,
        }
    }

    /// Mark pixels as changed. Native drawing operations use this instead of
    /// relying on a renderer-specific upload mechanism.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Return the byte offset of a pixel, or `None` outside the bitmap.
    pub fn pixel_offset(&self, x: u32, y: u32) -> Option<usize> {
        (x < self.width && y < self.height)
            .then_some((y as usize * self.width as usize + x as usize) * 4)
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

/// The whole logical scene.
#[derive(Debug, Default)]
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
    window_index: HashMap<u32, usize>,
    /// `layer.id` → index into `layers`. Rebuilt whenever a layer is
    /// removed; verified defensively like [`Scene::window_index`].
    layer_index: HashMap<u32, usize>,
    /// `bitmap.id` → index into `bitmaps`.
    bitmap_index: HashMap<u32, usize>,
    /// `font.id` → index into `fonts`.
    font_index: HashMap<u32, usize>,
    /// Monotonic mutation counter (see [`Scene::revision`]).
    revision: u64,
}

impl Scene {
    /// Monotonic counter bumped by every scene mutation (adds, removes and
    /// every `*_mut` accessor). The renderer compares it across frames to
    /// skip an unchanged sync entirely. [`Scene::set_bitmap_clean`]
    /// deliberately does not bump: clearing a renderer-internal dirty flag is
    /// not a logical mutation.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Bump [`Scene::revision`]. Over-approximating (bumping on a `*_mut`
    /// accessor that ends up not writing) is safe: it only costs a resync.
    fn touch(&mut self) {
        self.revision = self.revision.wrapping_add(1);
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
            layers: Vec::new(),
        });
        self.touch();
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

    /// Mutably look up a window by id, bumping [`Scene::revision`].
    pub fn window_mut(&mut self, id: u32) -> Option<&mut WindowState> {
        self.touch();
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
        self.touch();
        true
    }

    /// Add a layer. `parent == None` attaches to the window directly.
    pub fn add_layer(&mut self, window: u32, parent: Option<u32>) -> u32 {
        // A parent belongs to the same window; invalid/cross-window ids fall
        // back to a window-attached layer rather than creating an unreachable
        // subtree.
        let parent = parent.filter(|parent_id| {
            self.layer(*parent_id)
                .is_some_and(|layer| layer.window == window)
        });
        let id = self.next_layer;
        self.next_layer += 1;
        let primary = self.window(window).and_then(|w| w.primary_layer).is_none();
        self.layers.push(LayerState {
            id,
            window,
            parent,
            children: Vec::new(),
            bitmap: None,
            rect: Rect::default(),
            visible: false,
            opacity: 1.0,
            blend_type: LT_ALPHA,
            z_order: 0,
            fill_color: None,
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
            hint: String::new(),
            show_parent_hint: false,
            ignore_hint_sensing: false,
            ime_mode: 0,
            neutral_color: 0,
            absolute_order_mode: false,
            call_on_paint: false,
            image_modified: false,
            font_id: None,
            is_primary: false,
            pending_paint: false,
            shutdown: false,
        });
        self.layer_index.insert(id, self.layers.len() - 1);
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
        match parent {
            Some(p) => {
                if let Some(l) = self.layer_mut(p) {
                    l.children.push(id);
                }
            }
            None => {
                if let Some(w) = self.window_mut(window) {
                    w.layers.push(id);
                }
            }
        }
        self.touch();
        id
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

    /// Mutably look up a layer by id, bumping [`Scene::revision`].
    pub fn layer_mut(&mut self, id: u32) -> Option<&mut LayerState> {
        self.touch();
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

    /// Address of the layer's MainImage pixel buffer, or 0 when absent.
    /// Reference `GetMainImagePixelBuffer` (`LayerIntf.cpp:3005`).
    pub fn main_image_pixel_buffer(&self, layer_id: u32) -> usize {
        let Some(bitmap_id) = self.layer(layer_id).and_then(|l| l.bitmap) else {
            return 0;
        };
        self.bitmap(bitmap_id)
            .map_or(0, |b| b.rgba.as_ptr() as usize)
    }

    /// Address of the layer's MainImage pixel buffer for writing, marking the
    /// bitmap dirty (the reference `GetMainImagePixelBufferForWrite` sets
    /// `ImageModified` and calls `GetScanLineForWrite`). 0 when absent.
    pub fn main_image_pixel_buffer_for_write(&mut self, layer_id: u32) -> usize {
        let Some(bitmap_id) = self.layer(layer_id).and_then(|l| l.bitmap) else {
            return 0;
        };
        let Some(bitmap) = self.bitmap_mut(bitmap_id) else {
            return 0;
        };
        bitmap.mark_dirty();
        bitmap.rgba.as_mut_ptr() as usize
    }

    /// Address of the layer's ProvinceImage pixel buffer, or 0 when absent.
    /// Reference `GetProvinceImagePixelBuffer` (`LayerIntf.cpp:3027`).
    pub fn province_image_pixel_buffer(&self, layer_id: u32) -> usize {
        self.layer(layer_id)
            .and_then(|l| l.province.as_deref())
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
    pub fn get_province_pixel(&self, layer_id: u32, x: i32, y: i32) -> i32 {
        let Some(layer) = self.layer(layer_id) else {
            return 0;
        };
        let Some(buf) = layer.province.as_deref() else {
            return 0;
        };
        if x < 0 || y < 0 || x >= layer.province_width as i32 || y >= layer.province_height as i32 {
            return 0;
        }
        i32::from(buf[y as usize * layer.province_width as usize + x as usize])
    }

    /// Reference `SetProvincePixel` (`LayerIntf.cpp:2985`): allocate the plane
    /// when absent, clip against `ClipRect` (and the plane bounds), store the
    /// low byte and mark the image modified.
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
        if !in_clip || x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
            return;
        }
        if let Some(buf) = layer.province.as_deref_mut() {
            buf[y as usize * w as usize + x as usize] = n as u8;
            layer.image_modified = true;
        }
    }

    /// Detach one layer — the reference `tTJSNI_BaseLayer::Part()`
    /// (`LayerIntf.cpp:624`). The layer is severed from its parent (or the
    /// window root list) and removed from the scene. Children are **not**
    /// destroyed: the layer's direct children are re-parented to the window
    /// root list and stay alive (the reference `Invalidate` calls
    /// `child->Part()` for each direct child, `LayerIntf.cpp:551`). The old
    /// implementation removed the whole subtree, which silently deleted live
    /// children (focus, button and transition layers all share a parent).
    ///
    /// This is *detachment*, not destruction. Destroying a layer's native
    /// instance must use [`Scene::destroy_layer`], which removes the whole
    /// subtree the way the reference's GC cascade does. Using this function
    /// for destruction is what left a torn-down scene's layers alive as
    /// window roots (the "old scene images stay on screen" bug).
    pub fn remove_layer(&mut self, id: u32) {
        let Some(layer) = self.layer(id) else {
            return;
        };
        let (win, parent, font_id, bitmap) =
            (layer.window, layer.parent, layer.font_id, layer.bitmap);
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
        // being drawn *and* hit-tested. Registering these children instead made
        // them fresh top-level window layers, which is what left KAG's config
        // window with its sliders and buttons stuck on screen after
        // `invalidate _base[i]`: they were drawn at full opacity, outside the
        // faded parent, and their scripts then saw `parent == null` and threw
        // (`SelectItem.tjs` `onButtonLeave`/`onButtonEnter`). A script that
        // wants such a layer back re-parents it explicitly, which restores it.
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
        // and `SeverChild` blurs a removed focus holder.
        if let Some(w) = self.window_mut(win) {
            if w.primary_layer == Some(id) {
                w.primary_layer = None;
            }
            if w.focused_layer == Some(id) {
                w.focused_layer = None;
            }
        }
        self.touch();
    }

    /// Reference `tTJSNI_BaseLayer::Invalidate` (`LayerIntf.cpp:515`): the
    /// explicit native teardown the script triggers with `invalidate layer`.
    /// This is the **cycle breaker**: it detaches the layer from its window
    /// and parent, parts each direct child (the children become window roots
    /// and stay alive — the script's reference counts decide their fate),
    /// and releases the resources the layer owns (MainImage, province plane,
    /// font, primary slot).
    ///
    /// The layer is fully unregistered from the scene here — the reference's
    /// `Manager->UnregisterSelfFromWindow()` / `Manager->Release()` leave it
    /// detached and inert, and the script has already invalidated it. The
    /// later [`Scene::destroy_layer`] (when the TJS reference count finally
    /// drops) is then a no-op. This is [`Scene::remove_layer`]'s `Part()`
    /// semantics — it must **not** recurse into the child subtree. Keeping
    /// the inert record instead let invalidated scenes accumulate forever
    /// (their TJS objects are retained by script state).
    pub fn invalidate_layer(&mut self, id: u32) {
        if self.layer(id).is_none() {
            return;
        }
        // Mark it inert first so a re-entrant query cannot touch it while the
        // tree is being parted.
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
        let mut children_of: HashMap<u32, Vec<u32>> = HashMap::new();
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
        let mut doomed: HashSet<u32> = HashSet::new();
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
            .filter_map(|layer| layer.bitmap)
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
            if w.focused_layer.is_some_and(|f| doomed.contains(&f)) {
                w.focused_layer = None;
            }
        }
        self.touch();
    }

    /// Reference `tTVPLayerManager::SetFocusTo`: give keyboard focus to
    /// `id` within its window, clearing the previous holder. Returns
    /// `(previous, new)`. An invalid `id` returns `(None, None)` and leaves
    /// the current holder unchanged; use [`Scene::clear_focus`] to blur it.
    pub fn set_focus(&mut self, id: u32) -> (Option<u32>, Option<u32>) {
        let Some(layer) = self.layer(id) else {
            return (None, None);
        };
        let win = layer.window;
        let prev = self.window(win).and_then(|w| w.focused_layer);
        if prev == Some(id) {
            return (prev, Some(id));
        }
        if let Some(w) = self.window_mut(win) {
            w.focused_layer = Some(id);
        }
        self.touch();
        (prev, Some(id))
    }

    /// Clear the focused layer of `window`, returning the previous holder.
    pub fn clear_focus(&mut self, window: u32) -> Option<u32> {
        let prev = self.window(window).and_then(|w| w.focused_layer);
        if let Some(w) = self.window_mut(window) {
            w.focused_layer = None;
        }
        if prev.is_some() {
            self.touch();
        }
        prev
    }

    /// Reference `GetNodeVisible`: `visible` and every ancestor visible.
    pub fn node_visible(&self, id: u32) -> bool {
        let mut cur = id;
        loop {
            let Some(layer) = self.layer(cur) else {
                return false;
            };
            if !layer.visible {
                return false;
            }
            match layer.parent {
                Some(p) => cur = p,
                None => return true,
            }
        }
    }

    /// Reference `GetNodeEnabled`: `enabled` and every ancestor enabled.
    pub fn node_enabled(&self, id: u32) -> bool {
        let mut cur = id;
        loop {
            let Some(layer) = self.layer(cur) else {
                return false;
            };
            if !layer.enabled {
                return false;
            }
            match layer.parent {
                Some(p) => cur = p,
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
    /// the window's paint order, wrapping, never returning `id` itself.
    pub fn next_focusable(&self, id: u32) -> Option<u32> {
        self.focusable_neighbor(id, true)
    }

    /// Reference `_GetPrevFocusable`/`GetPrevFocusable`
    /// (`LayerIntf.cpp:3595`): the previous focusable layer, wrapping.
    pub fn prev_focusable(&self, id: u32) -> Option<u32> {
        self.focusable_neighbor(id, false)
    }

    fn focusable_neighbor(&self, id: u32, forward: bool) -> Option<u32> {
        let layer = self.layer(id)?;
        let order = self.window_layer_order(layer.window);
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
        while let Some(c) = cur {
            let l = self.layer(c)?;
            match l.parent {
                Some(p) => {
                    px = px.saturating_add(l.rect.x);
                    py = py.saturating_add(l.rect.y);
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
            let Some((ax, ay)) = self.absolute_offset(id) else {
                continue;
            };
            let lx = px - ax;
            let ly = py - ay;
            if lx < 0 || ly < 0 || lx >= l.rect.w as i32 || ly >= l.rect.h as i32 {
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

    /// Sum of this layer's and its ancestors' `rect` offsets (absolute
    /// window position).
    pub fn absolute_offset(&self, id: u32) -> Option<(i32, i32)> {
        let mut x = 0i32;
        let mut y = 0i32;
        let mut cur = Some(id);
        while let Some(c) = cur {
            let l = self.layer(c)?;
            x = x.saturating_add(l.rect.x);
            y = y.saturating_add(l.rect.y);
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
                if px < 0
                    || py < 0
                    || px >= layer.province_width as i32
                    || py >= layer.province_height as i32
                {
                    return false;
                }
                buf[py as usize * layer.province_width as usize + px as usize] != 0
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
                    let alpha = bmp.rgba[(py as usize * bmp.width as usize + px as usize) * 4 + 3];
                    alpha as i32 >= layer.hit_threshold
                }
                None => layer.hit_threshold <= 0,
            },
            // Unknown hit types hit (the reference falls through to true).
            _ => true,
        }
    }

    /// Move the layer to the front among its siblings (`bringToFront`),
    /// affecting only the insertion tie-break; `z_order` still dominates.
    pub fn layer_move_to_front(&mut self, id: u32) {
        // Detach + re-attach as last (frontmost) among its siblings.
        let (window, parent) = match self.layer(id) {
            Some(l) => (l.window, l.parent),
            None => return,
        };
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
        self.touch();
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
            screen_buffer: false,
            script_owned: false,
        });
        self.touch();
        id
    }

    /// Resize a bitmap in place, preserving the overlapping top-left region
    /// and filling any expansion with `fill` (RGBA) — the reference
    /// `iTVPBaseBitmap::SetSizeWithFill` (`LayerBitmapIntf.cpp:122`).
    /// Dimensions are clamped to at least 1, matching the reference
    /// `tTVPBaseTexture` constructor. Marks the bitmap dirty. Returns true
    /// when the bitmap was present and resized.
    pub fn resize_bitmap(&mut self, id: u32, width: u32, height: u32, fill: [u8; 4]) -> bool {
        let width = width.max(1);
        let height = height.max(1);
        let Some(old) = self.bitmap(id) else {
            return false;
        };
        if old.width == width && old.height == height {
            return true;
        }
        let old_w = old.width as usize;
        let old_h = old.height as usize;
        let copy_w = old_w.min(width as usize);
        let copy_h = old_h.min(height as usize);
        let Some(bytes) = (width as usize).checked_mul(height as usize) else {
            return false;
        };
        let Some(pixels) = bytes.checked_mul(4) else {
            return false;
        };
        let mut rgba: Vec<u8> = Vec::new();
        if rgba.try_reserve_exact(pixels).is_err() {
            return false;
        }
        rgba.resize(pixels, 0);
        for chunk in rgba.chunks_exact_mut(4) {
            chunk.copy_from_slice(&fill);
        }
        // Copy the overlapping region row by row (the row pitch changes).
        let Some(source) = self.bitmap(id) else {
            return false;
        };
        for y in 0..copy_h {
            let src = &source.rgba[y * old_w * 4..(y * old_w + copy_w) * 4];
            let dst = &mut rgba[y * width as usize * 4..(y * width as usize + copy_w) * 4];
            dst.copy_from_slice(src);
        }
        let Some(bitmap) = self.bitmap_mut(id) else {
            return false;
        };
        bitmap.width = width;
        bitmap.height = height;
        bitmap.rgba = rgba;
        bitmap.mark_dirty();
        true
    }

    /// Remove a bitmap unconditionally (the low-level primitive behind
    /// [`Scene::release_bitmap`]). Keeps the id→index map consistent and
    /// bumps [`Scene::revision`]. Returns true when the bitmap was present.
    pub fn remove_bitmap(&mut self, id: u32) -> bool {
        let before = self.bitmaps.len();
        self.bitmaps.retain(|bitmap| bitmap.id != id);
        if self.bitmaps.len() == before {
            return false;
        }
        self.bitmap_index.clear();
        for (index, bitmap) in self.bitmaps.iter().enumerate() {
            self.bitmap_index.insert(bitmap.id, index);
        }
        self.touch();
        true
    }

    /// Whether any live layer still references `id` as its MainImage.
    pub fn bitmap_referenced_by_layer(&self, id: u32) -> bool {
        self.layers.iter().any(|layer| layer.bitmap == Some(id))
    }

    /// Release a bitmap when the last holder is gone.
    ///
    /// Reference semantics: a layer's MainImage is a **reference-counted**
    /// `tTVPBaseBitmap`; destroying the layer drops only its own reference.
    /// In this port a bitmap id can be held by more than one owner, so
    /// freeing is conservative — the bitmap is removed only when
    /// * no live layer references it (`layer.bitmap == Some(id)`), and
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
            log::debug!("released scene bitmap #{id}");
            return true;
        }
        false
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

    /// Look up a bitmap by id. Uses the id→index map, falling back to a
    /// linear scan if callers edited [`Scene::bitmaps`] directly.
    pub fn bitmap(&self, id: u32) -> Option<&BitmapState> {
        if let Some(index) = self.bitmap_index.get(&id).copied()
            && let Some(bitmap) = self.bitmaps.get(index)
            && bitmap.id == id
        {
            return Some(bitmap);
        }
        self.bitmaps.iter().find(|b| b.id == id)
    }

    /// Mutably look up a bitmap by id, bumping [`Scene::revision`].
    pub fn bitmap_mut(&mut self, id: u32) -> Option<&mut BitmapState> {
        self.touch();
        let index = self
            .bitmap_index
            .get(&id)
            .copied()
            .filter(|&index| self.bitmaps.get(index).is_some_and(|b| b.id == id));
        match index {
            Some(index) => self.bitmaps.get_mut(index),
            None => self.bitmaps.iter_mut().find(|b| b.id == id),
        }
    }

    /// Clear a bitmap's [`BitmapState::dirty`] flag without bumping
    /// [`Scene::revision`]. Called by the projection after a texture upload;
    /// the flag is backend bookkeeping, not a logical scene mutation.
    pub fn set_bitmap_clean(&mut self, id: u32) -> bool {
        let index = self.bitmap_index.get(&id).copied();
        if let Some(index) = index
            && let Some(bitmap) = self.bitmaps.get_mut(index)
            && bitmap.id == id
        {
            bitmap.dirty = false;
            return true;
        }
        if let Some(bitmap) = self.bitmaps.iter_mut().find(|b| b.id == id) {
            bitmap.dirty = false;
            return true;
        }
        false
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
        self.touch();
        id
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

        // One pass over this window's layers builds both the id list (in
        // insertion order) and a `parent → all children` index. The old
        // implementation re-scanned the entire layer table for every layer,
        // which made this O(n²); the index makes it a single O(n) pass plus
        // the sibling sorts below.
        let mut all_children: HashMap<Option<u32>, Vec<u32>> = HashMap::new();
        let mut window_layers: Vec<u32> = Vec::new();
        for layer in self
            .layers
            .iter()
            .filter(|l| l.window == window && !l.shutdown)
        {
            window_layers.push(layer.id);
            all_children.entry(layer.parent).or_default().push(layer.id);
        }

        // Root list: only the layers the window actually owns. A layer whose
        // parent was severed is deliberately **not** adopted here. The
        // reference draws exactly the tree reachable from the window's
        // `Primary` layer (`tTVPLayerManager::RecreateOverallOrderIndex`,
        // `LayerManager.cpp:208`), and `NotifyPart` invalidates that index, so a
        // `Part()`ed layer stops being drawn and hit-tested. Adopting every
        // `parent == None` layer made detached, invalidated subtrees render
        // again as top-level window layers — KAG's config window kept its
        // sliders and buttons on screen after leaving the page.
        let mut roots: Vec<u32> = window_state
            .layers
            .iter()
            .copied()
            .filter(|id| self.layer(*id).is_some_and(|l| !l.shutdown))
            .collect();
        let root_order = roots.clone();
        self.sort_siblings(&mut roots, &root_order);

        let mut order = Vec::new();
        let mut visited = HashSet::new();
        for id in roots {
            self.append_layer_subtree(window, id, &all_children, &mut order, &mut visited);
        }
        // No "leftovers" pass. It used to append every window layer that the
        // root walk had not visited, on the theory that a malformed parent link
        // should not hide a layer. But a layer is unvisited precisely when it is
        // **not reachable from the window's roots** — which is the definition of
        // a detached layer. Re-adding those made `Part()` a no-op for rendering:
        // `invalidate` severed a layer's children and this pass put them back on
        // screen (and back into hit-testing), which is why KAG's config window
        // kept its sliders and buttons after `invalidate _base[i]`.
        // A child that is genuinely missing from its parent's `children` list is
        // still found: `all_children` is indexed by the layer's own `parent`
        // field and `append_layer_subtree` merges it into the maintained list.
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
        let position: HashMap<u32, usize> = maintained
            .iter()
            .enumerate()
            .map(|(index, &id)| (id, index))
            .collect();
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
        all_children: &HashMap<Option<u32>, Vec<u32>>,
        order: &mut Vec<u32>,
        visited: &mut HashSet<u32>,
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
            let mut seen: HashSet<u32> = children.iter().copied().collect();
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
