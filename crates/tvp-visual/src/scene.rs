//! Logical scene model for the TVP visual natives.
//!
//! This is the **shared contract** between the natives (`crates/tvp-visual`)
//! and the Bevy renderer (`crates/render`): natives mutate the scene under a
//! write lock, render systems read it under a read lock. Everything is
//! id-based; invalid ids are no-ops.
//!
//! No Bevy types here — this crate stays engine-agnostic.

use std::collections::{HashMap, HashSet};

/// One logical window.
#[derive(Debug, Clone)]
pub struct WindowState {
    pub id: u32,
    pub title: String,
    /// Game logical size in pixels.
    pub inner_size: (u32, u32),
    pub visible: bool,
    /// 0.0 ..= 1.0
    pub opacity: f32,
    /// The layer scripts use as `window.primaryLayer`.
    pub primary_layer: Option<u32>,
    /// The layer that currently holds keyboard focus in this window
    /// (reference `tTVPLayerManager::FocusedLayer`), if any.
    pub focused_layer: Option<u32>,
    /// Render order, back -> front.
    pub layers: Vec<u32>,
}

/// Core TVP drawable/blend types used by `Layer.type` (the native
/// `tTVPLayerType` values). The full 29-mode mapping lives in
/// `render::blend`; these four are the ones the logical scene model and its
/// tests reference directly. `blend_type` stores the raw integer contract so
/// every mode round-trips through the shared model without changes here.
pub const LT_OPAQUE: i64 = 1;
pub const LT_ALPHA: i64 = 2;
pub const LT_ADDITIVE: i64 = 3;
pub const LT_SUBTRACTIVE: i64 = 4;

/// One logical layer (sprite surface) attached to a window.
#[derive(Debug, Clone)]
pub struct LayerState {
    pub id: u32,
    pub window: u32,
    /// Parent layer id, or None to attach directly to the window.
    pub parent: Option<u32>,
    pub children: Vec<u32>,
    /// Bitmap id shown on this layer, if any.
    pub bitmap: Option<u32>,
    /// Position + size in window coordinates.
    pub rect: Rect,
    pub visible: bool,
    pub opacity: f32,
    /// TVP drawable/blend type (`ltAlpha` by default). Kept in the scene so
    /// render synchronization does not lose `Layer.type` updates.
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
    pub image_top: i32,
    pub image_width: u32,
    pub image_height: u32,
    /// Reference `ProvinceImage` (`LayerIntf.cpp:2359` `AllocateProvinceImage`):
    /// an 8-bit plane (`province_width * province_height` bytes, one byte per
    /// pixel) backing `getProvincePixel`/`setProvincePixel` and the
    /// `provinceImageBuffer*` properties. `None` until `setProvincePixel` or a
    /// `provinceImageBufferForWrite` read allocates it. The plane is layer
    /// owned (the reference allocates a fresh `tTVPBaseBitmap(..., 8)`), so it
    /// is not shared through `bitmap`.
    pub province: Option<Box<[u8]>>,
    pub province_width: u32,
    pub province_height: u32,
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
    /// `dfAddAlpha=4`). Stored; the renderer always draws the main image.
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
    pub attention_top: i32,
    /// Reference `UseAttention`: whether the layer participates in the
    /// attention/hint system.
    pub use_attention: bool,
    /// Reference `Cached`: request the layer be rendered to a cache.
    pub cached: bool,
    /// Reference `Hint` (script `layer.hint`) and the hint-system flags.
    pub hint: String,
    pub show_parent_hint: bool,
    pub ignore_hint_sensing: bool,
    /// Reference `ImeMode` (stored) and `NeutralColor` (ARGB, stored).
    pub ime_mode: i32,
    pub neutral_color: i64,
    /// Reference `AbsoluteOrderMode`/`CallOnPaint` flags.
    pub absolute_order_mode: bool,
    pub call_on_paint: bool,
    /// Reference `HoldAlpha`-adjacent `ImageModified` flag: set by pixel
    /// operations, clearable by script.
    pub image_modified: bool,
    /// Font id (into [`Scene::fonts`]) backing `layer.font`, or `None` until
    /// the script reads `layer.font` (which lazily allocates one). `drawText`
    /// resolves the requested face from here.
    pub font_id: Option<u32>,
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
pub struct BitmapState {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Storage name it was loaded from, if any.
    pub name: Option<String>,
    /// Set when contents changed; the renderer re-uploads on true.
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

/// One font face used by `Layer.drawText` (rasterized by `tvp-text`).
#[derive(Debug, Clone)]
pub struct FontState {
    pub id: u32,
    pub face: String,
    pub height: i32,
    pub color: [u8; 4],
    pub bold: bool,
    pub italic: bool,
    pub strikeout: bool,
    pub underline: bool,
    /// Rotation in the reference's tenths-of-a-degree units (game code does
    /// `font.angle \ 10`).
    pub angle: f64,
}

/// Rectangle in window coordinates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// The whole logical scene.
#[derive(Debug, Default)]
pub struct Scene {
    pub windows: Vec<WindowState>,
    pub layers: Vec<LayerState>,
    pub bitmaps: Vec<BitmapState>,
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

    pub fn window(&self, id: u32) -> Option<&WindowState> {
        if let Some(index) = self.window_index.get(&id).copied()
            && let Some(window) = self.windows.get(index)
            && window.id == id
        {
            return Some(window);
        }
        self.windows.iter().find(|w| w.id == id)
    }

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
        self.bitmap(bitmap_id).map_or(0, |b| b.width * 4)
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
        let mut buf = vec![0u8; w as usize * h as usize].into_boxed_slice();
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
            Some(c) => x >= c.x && y >= c.y && x < c.x + c.w as i32 && y < c.y + c.h as i32,
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
    /// `(previous, new)`. A `None`/invalid `id` blurs the current holder.
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
                    px += l.rect.x;
                    py += l.rect.y;
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
            x += l.rect.x;
            y += l.rect.y;
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
                let px = x - layer.image_left;
                let py = y - layer.image_top;
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
                    let px = x - layer.image_left;
                    let py = y - layer.image_top;
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
    /// The decode cache never holds a scene bitmap id: [`BitmapCache`] owns
    /// its decoded pixels directly, so a loaded layer's image is always
    /// releasable and a re-`load` still hits the cache.
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

    pub fn bitmap(&self, id: u32) -> Option<&BitmapState> {
        if let Some(index) = self.bitmap_index.get(&id).copied()
            && let Some(bitmap) = self.bitmaps.get(index)
            && bitmap.id == id
        {
            return Some(bitmap);
        }
        self.bitmaps.iter().find(|b| b.id == id)
    }

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
    /// [`Scene::revision`]. Called by the renderer after a texture upload;
    /// the flag is renderer bookkeeping, not a logical scene mutation.
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

/// Bitmap cache for `Bitmap(name)` reuse, keyed by normalized storage name.
///
/// The cache owns its decoded pixels **independently of any scene bitmap**
/// (the reference's `tTVPGraphicImageData` holds its own refcounted image).
/// That means `TVPClearGraphicCache` frees the templates by simply clearing
/// the map, and a layer's MainImage can be released when its layer is
/// destroyed without invalidating the cache.
#[derive(Default)]
pub struct BitmapCache {
    pub by_name: HashMap<String, CachedImage>,
}

/// One decode-cache template: straight-alpha RGBA8 pixels plus size.
#[derive(Debug, Clone)]
pub struct CachedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real title scene's stack z-values (system_status.tjs:
    /// LAYER_LOGO=110000, LAYER_COVER=150000, LAYER_HINT=210000; content
    /// layers at 0). The game's own comment "数字が大きいほど手前" (larger =
    /// closer/front) and the reference's AbsoluteOrderIndex semantics (higher
    /// index draws later = in front) mean window_layer_order must return
    /// ascending z, back→front, with insertion order only breaking ties.
    #[test]
    fn window_layer_order_real_title_z_values() {
        let mut scene = Scene::default();
        let win = scene.add_window("title", (1280, 720));
        // Insertion order deliberately disagrees with z-order (HINT created
        // first in MainWindow ctor, then LOGO, then z=0 content).
        let hint = scene.add_layer(win, None);
        let logo = scene.add_layer(win, None);
        let bg = scene.add_layer(win, None);
        let cover = scene.add_layer(win, None);
        let art = scene.add_layer(win, None);
        scene.layer_mut(hint).unwrap().z_order = 210000;
        scene.layer_mut(logo).unwrap().z_order = 110000;
        scene.layer_mut(cover).unwrap().z_order = 150000;
        // bg + art keep z = 0.

        let order = scene.window_layer_order(win);
        let pos = |id: u32| order.iter().position(|&x| x == id).unwrap();
        assert_eq!(order.len(), 5);
        assert!(pos(bg) < pos(art), "equal z keeps insertion order");
        assert!(
            pos(art) < pos(logo),
            "z=0 renders behind LAYER_LOGO (110000)"
        );
        assert!(pos(logo) < pos(cover), "110000 renders behind 150000");
        assert!(pos(cover) < pos(hint), "150000 renders behind 210000");
        assert_eq!(pos(hint), order.len() - 1, "LAYER_HINT is frontmost");
    }

    /// A parent is emitted before its children, and the complete child
    /// subtree remains together before the next root sibling.
    #[test]
    fn window_layer_order_flattens_hierarchy() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1280, 720));
        let parent = scene.add_layer(win, None);
        let child = scene.add_layer(win, Some(parent));
        let sibling = scene.add_layer(win, None);
        let order = scene.window_layer_order(win);
        assert_eq!(order, vec![parent, child, sibling]);
    }

    #[test]
    fn moving_layer_to_front_updates_equal_z_sibling_order() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1, 1));
        let back = scene.add_layer(win, None);
        let front = scene.add_layer(win, None);
        scene.layer_move_to_front(back);
        assert_eq!(scene.window_layer_order(win), vec![front, back]);
    }

    /// Deterministic back→front order across z-order, insertion tie-breaks
    /// and a nested subtree. This is the reference order the O(n log n)
    /// rewrite must keep matching.
    #[test]
    fn window_layer_order_complex_hierarchy_matches_reference() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1, 1));
        let a = scene.add_layer(win, None);
        let b = scene.add_layer(win, None);
        let c = scene.add_layer(win, None);
        scene.layer_mut(a).unwrap().z_order = 5;
        scene.layer_mut(b).unwrap().z_order = 1;
        // c keeps z = 0.
        let b1 = scene.add_layer(win, Some(b));
        let b2 = scene.add_layer(win, Some(b));
        scene.layer_mut(b2).unwrap().z_order = -1;
        let b1c = scene.add_layer(win, Some(b1));

        // Roots by (z, insertion, id): c(0), b(1), a(5).
        // b's children by (z, insertion, id): b2(-1), b1(0); b1c follows b1.
        assert_eq!(scene.window_layer_order(win), vec![c, b, b2, b1, b1c, a]);
    }

    /// A child that exists in the layer table but is missing from its
    /// parent's `children` vector must still render (found by the parent
    /// index) and stay in the same relative position.
    #[test]
    fn window_layer_order_finds_children_missing_from_parent_list() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1, 1));
        let parent = scene.add_layer(win, None);
        let child = scene.add_layer(win, Some(parent));
        scene.layer_mut(parent).unwrap().children.clear();
        assert_eq!(scene.window_layer_order(win), vec![parent, child]);
    }

    /// A layer the window does not own is **not** drawn, whatever its parent
    /// link looks like. The reference renders exactly the tree reachable from
    /// the window's `Primary` layer (`LayerManager.cpp:208`), so a layer outside
    /// that tree is invisible and not hit-tested.
    ///
    /// This used to be a "defensive" pass that appended every unvisited layer of
    /// the window, on the theory that a stale parent link should not hide a
    /// layer. In practice it silently defeated `Part()` and `invalidate`: a
    /// detached (invalidated) subtree was re-added to the draw order every
    /// frame, which is what kept KAG's config window on screen after leaving the
    /// settings page.
    #[test]
    fn window_layer_order_skips_layers_the_window_does_not_own() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1, 1));
        let owned = scene.add_layer(win, None);
        let orphan = scene.add_layer(win, None);
        // The window no longer owns `orphan`, as after a `Part()`.
        scene
            .window_mut(win)
            .unwrap()
            .layers
            .retain(|&id| id != orphan);
        scene.layer_mut(orphan).unwrap().parent = Some(9999);
        assert_eq!(scene.window_layer_order(win), vec![owned]);
        // But a child of an owned layer is still reached.
        let child = scene.add_layer(win, Some(owned));
        assert!(scene.window_layer_order(win).contains(&child));
    }

    /// The `*_mut` lookups are index-accelerated but must remain correct once
    /// a removal shifts the underlying vectors.
    #[test]
    fn id_lookups_stay_correct_after_layer_removal() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1, 1));
        let a = scene.add_layer(win, None);
        let b = scene.add_layer(win, None);
        let c = scene.add_layer(win, None);
        scene.remove_layer(b);
        assert_eq!(scene.layer(a).unwrap().id, a);
        assert_eq!(scene.layer(c).unwrap().id, c);
        assert!(scene.layer(b).is_none());

        let bitmap = scene.add_bitmap(1, 1, vec![0; 4]);
        let font = scene.add_font("x".into(), 12, [0; 4]);
        assert_eq!(scene.bitmap(bitmap).unwrap().id, bitmap);
        assert_eq!(scene.font(font).unwrap().id, font);
        assert_eq!(scene.window(win).unwrap().id, win);
    }

    /// `remove_layer` is `Part()`: it severs the layer from its parent (or the
    /// window roots) and severs its direct children from it, but destroys
    /// nothing. A severed layer stops being **rendered and hit-tested**: the
    /// reference draws exactly the tree reachable from the window's `Primary`
    /// layer (`tTVPLayerManager::RecreateOverallOrderIndex`,
    /// `LayerManager.cpp:208`) and `NotifyPart` invalidates that index. So a
    /// `Part()`ed child is invisible until something re-parents it — it is not
    /// promoted to a window-level layer.
    #[test]
    fn remove_layer_detaches_children_and_stops_rendering_them() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1, 1));
        let parent = scene.add_layer(win, None);
        let child = scene.add_layer(win, Some(parent));
        let grandchild = scene.add_layer(win, Some(child));
        scene.remove_layer(parent);
        assert!(scene.layer(parent).is_none());
        assert!(scene.layer(child).is_some(), "child survives Part()");
        assert!(scene.layer(grandchild).is_some(), "grandchild survives");
        assert_eq!(scene.layer(child).unwrap().parent, None);
        assert_eq!(scene.layer(grandchild).unwrap().parent, Some(child));
        // Detached from the window's roots, so neither is drawn any more.
        let order = scene.window_layer_order(win);
        assert!(
            !order.contains(&child),
            "a Part()ed child is not rendered as a window root"
        );
        assert!(!order.contains(&grandchild));
        // Re-parenting it to a window-level layer restores it: the subtree was
        // detached, not destroyed.
        let other = scene.add_layer(win, None);
        scene.layer_mut(child).unwrap().parent = Some(other);
        scene.layer_mut(other).unwrap().children.push(child);
        let order = scene.window_layer_order(win);
        assert!(order.contains(&child), "re-parented layer renders again");
        assert!(order.contains(&grandchild), "with its subtree intact");
    }

    /// `destroy_layer` models the reference `Invalidate` + GC cascade: the
    /// whole subtree is removed, so no descendant survives as a window root.
    /// This is the fix for a torn-down scene's images staying on screen.
    #[test]
    fn destroy_layer_removes_whole_subtree() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1, 1));
        let parent = scene.add_layer(win, None);
        let child = scene.add_layer(win, Some(parent));
        let grandchild = scene.add_layer(win, Some(child));
        let unrelated = scene.add_layer(win, None);
        scene.destroy_layer(parent);
        assert!(scene.layer(parent).is_none());
        assert!(scene.layer(child).is_none(), "child destroyed with parent");
        assert!(
            scene.layer(grandchild).is_none(),
            "grandchild destroyed with parent"
        );
        assert!(scene.layer(unrelated).is_some(), "unrelated layer survives");
        // None of the destroyed layers may reach the renderer.
        let order = scene.window_layer_order(win);
        assert_eq!(order, vec![unrelated]);
    }

    /// Destroying a subtree that was itself `Part()`ed out to the window root
    /// list removes it from that list and from `window_layer_order`, leaving
    /// the rest of the window untouched.
    #[test]
    fn destroy_layer_removes_detached_root_from_layer_order() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1, 1));
        let grandpa = scene.add_layer(win, None);
        let parent = scene.add_layer(win, Some(grandpa));
        let child = scene.add_layer(win, Some(parent));
        // Part `parent` out to the window root list; its child comes along.
        scene.layer_mut(grandpa).unwrap().children.clear();
        scene.layer_mut(parent).unwrap().parent = None;
        scene.window_mut(win).unwrap().layers.push(parent);
        assert!(scene.window_layer_order(win).contains(&parent));
        scene.destroy_layer(parent);
        let order = scene.window_layer_order(win);
        assert_eq!(order, vec![grandpa]);
        assert!(
            !order.contains(&child),
            "child destroyed with the detached parent"
        );
        assert_eq!(scene.window(win).unwrap().layers, vec![grandpa]);
    }

    /// `destroy_layer` drops the doomed layers' lazily-allocated fonts and
    /// clears a primary/focus holder that pointed into the subtree.
    #[test]
    fn destroy_layer_cleans_fonts_primary_and_focus() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1, 1));
        let primary = scene.add_layer(win, None);
        let child = scene.add_layer(win, Some(primary));
        let font = scene.add_font("f".into(), 12, [0; 4]);
        scene.layer_mut(child).unwrap().font_id = Some(font);
        scene.set_focus(child);
        assert_eq!(scene.window(win).unwrap().primary_layer, Some(primary));
        assert_eq!(scene.window(win).unwrap().focused_layer, Some(child));
        scene.destroy_layer(primary);
        assert_eq!(scene.window(win).unwrap().primary_layer, None);
        assert_eq!(scene.window(win).unwrap().focused_layer, None);
        assert!(scene.font(font).is_none(), "owned font destroyed");
    }

    /// `destroy_layer` releases the bitmaps its subtree alone held, while a
    /// bitmap still attached to a surviving layer stays (reference
    /// refcounted MainImage).
    #[test]
    fn destroy_layer_releases_layer_owned_bitmaps_but_keeps_shared() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1, 1));
        let parent = scene.add_layer(win, None);
        let child_solo = scene.add_layer(win, Some(parent));
        let child_shared = scene.add_layer(win, Some(parent));
        let survivor = scene.add_layer(win, None);
        let solo = scene.add_bitmap(2, 2, vec![0; 16]);
        let shared = scene.add_bitmap(2, 2, vec![1; 16]);
        scene.layer_mut(child_solo).unwrap().bitmap = Some(solo);
        scene.layer_mut(child_shared).unwrap().bitmap = Some(shared);
        scene.layer_mut(survivor).unwrap().bitmap = Some(shared);

        scene.destroy_layer(parent);

        assert!(scene.bitmap(solo).is_none(), "solo bitmap freed");
        assert!(scene.bitmap(shared).is_some(), "shared bitmap survives");
        assert!(scene.bitmap_referenced_by_layer(shared));
    }

    /// A whole-scene teardown releases every layer-owned bitmap — the count
    /// the renderer then prunes from `Assets<Image>`. Only a bitmap still
    /// referenced by a surviving layer is kept.
    #[test]
    fn scene_teardown_releases_all_layer_owned_bitmaps() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1280, 720));
        let root = scene.add_layer(win, None);
        let mut owned = Vec::new();
        for _ in 0..100 {
            let layer = scene.add_layer(win, Some(root));
            let bmp = scene.add_bitmap(2, 2, vec![0; 16]);
            scene.layer_mut(layer).unwrap().bitmap = Some(bmp);
            owned.push(bmp);
        }
        let shared = scene.add_bitmap(2, 2, vec![0; 16]);
        scene.layer_mut(root).unwrap().bitmap = Some(shared);
        let survivor = scene.add_layer(win, None);
        scene.layer_mut(survivor).unwrap().bitmap = Some(shared);

        scene.destroy_layer(root);

        for bmp in &owned {
            assert!(scene.bitmap(*bmp).is_none(), "bitmap {bmp} released");
        }
        assert!(scene.bitmap(shared).is_some(), "shared bitmap survives");
        assert_eq!(
            scene.collect_unused_bitmaps(),
            0,
            "every layer-owned bitmap was already released"
        );
    }

    /// `remove_layer` (`Part()`) also releases the removed layer's own
    /// bitmap; a child kept alive by the detach keeps its own image.
    #[test]
    fn remove_layer_releases_its_bitmap_and_keeps_children_images() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1, 1));
        let parent = scene.add_layer(win, None);
        let child = scene.add_layer(win, Some(parent));
        let other = scene.add_layer(win, None);
        let parent_bmp = scene.add_bitmap(2, 2, vec![0; 16]);
        let child_bmp = scene.add_bitmap(2, 2, vec![1; 16]);
        let other_bmp = scene.add_bitmap(2, 2, vec![2; 16]);
        scene.layer_mut(parent).unwrap().bitmap = Some(parent_bmp);
        scene.layer_mut(child).unwrap().bitmap = Some(child_bmp);
        scene.layer_mut(other).unwrap().bitmap = Some(other_bmp);

        scene.remove_layer(parent);

        assert!(scene.bitmap(parent_bmp).is_none(), "parent bitmap freed");
        assert!(scene.bitmap(child_bmp).is_some(), "child survives Part");
        assert!(scene.bitmap(other_bmp).is_some(), "other layer untouched");
    }

    /// A script `Bitmap` object keeps its pixels alive even after every
    /// layer sharing the id is destroyed (the reference refcounted
    /// MainImage).
    #[test]
    fn script_owned_bitmap_survives_layer_destroy() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1, 1));
        let layer = scene.add_layer(win, None);

        let script = scene.add_bitmap(2, 2, vec![0; 16]);
        scene.bitmap_mut(script).unwrap().script_owned = true;
        scene.layer_mut(layer).unwrap().bitmap = Some(script);
        scene.destroy_layer(layer);
        assert!(scene.bitmap(script).is_some(), "script Bitmap survives");
    }

    /// The window primary layer's screen buffer survives while its window
    /// lives and is freed when the primary layer is destroyed.
    #[test]
    fn primary_screen_buffer_survives_until_the_layer_is_destroyed() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1, 1));
        let primary = scene.add_layer(win, None);
        assert_eq!(scene.window(win).unwrap().primary_layer, Some(primary));
        let content = scene.add_layer(win, Some(primary));
        let screen = scene.add_bitmap(4, 4, vec![0; 64]);
        scene.bitmap_mut(screen).unwrap().screen_buffer = true;
        scene.layer_mut(primary).unwrap().bitmap = Some(screen);
        let content_bmp = scene.add_bitmap(2, 2, vec![1; 16]);
        scene.layer_mut(content).unwrap().bitmap = Some(content_bmp);

        // A sweep while the window lives must not free the screen buffer.
        assert_eq!(scene.collect_unused_bitmaps(), 0);
        assert!(scene.bitmap(screen).is_some());

        scene.destroy_layer(primary);
        assert!(
            scene.bitmap(screen).is_none(),
            "screen buffer freed with the primary layer"
        );
        assert!(scene.bitmap(content_bmp).is_none(), "content freed too");
    }

    /// `collect_unused_bitmaps` sweeps orphans but respects the script
    /// protection.
    #[test]
    fn collect_unused_bitmaps_sweeps_orphans_only() {
        let mut scene = Scene::default();
        let orphan = scene.add_bitmap(1, 1, vec![0; 4]);
        let script = scene.add_bitmap(1, 1, vec![0; 4]);
        scene.bitmap_mut(script).unwrap().script_owned = true;

        assert_eq!(scene.collect_unused_bitmaps(), 1);
        assert!(scene.bitmap(orphan).is_none());
        assert!(scene.bitmap(script).is_some());
    }

    /// Rebuilding a scene after a teardown must not surface a stale layer:
    /// the new generation renders exactly its own layers.
    #[test]
    fn scene_rebuild_after_teardown_leaves_no_stale_layers() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1280, 720));
        // First scene generation: a root with nested content.
        let old_root = scene.add_layer(win, None);
        let old_child = scene.add_layer(win, Some(old_root));
        let old_grandchild = scene.add_layer(win, Some(old_child));
        scene.destroy_layer(old_root);
        assert!(scene.layers.is_empty());
        // New generation.
        let new_root = scene.add_layer(win, None);
        let new_child = scene.add_layer(win, Some(new_root));
        let order = scene.window_layer_order(win);
        assert_eq!(order, vec![new_root, new_child]);
        for stale in [old_root, old_child, old_grandchild] {
            assert!(scene.layer(stale).is_none(), "no stale layer {stale}");
            assert!(!order.contains(&stale), "stale {stale} not rendered");
        }
    }

    /// `node_visible`/`node_enabled` walk the ancestor chain.
    #[test]
    fn node_state_walks_ancestors() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1, 1));
        let parent = scene.add_layer(win, None);
        let child = scene.add_layer(win, Some(parent));
        scene.layer_mut(parent).unwrap().visible = true;
        scene.layer_mut(child).unwrap().visible = true;
        assert!(scene.node_visible(child));
        scene.layer_mut(parent).unwrap().visible = false;
        assert!(!scene.node_visible(child), "hidden ancestor hides child");
        assert!(scene.node_enabled(child));
        scene.layer_mut(parent).unwrap().enabled = false;
        assert!(!scene.node_enabled(child));
    }

    /// `set_focus` moves the window's focus and `remove_layer` blurs a
    /// removed focus holder.
    #[test]
    fn focus_moves_and_clears_with_removal() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1, 1));
        let a = scene.add_layer(win, None);
        let b = scene.add_layer(win, None);
        assert_eq!(scene.set_focus(a), (None, Some(a)));
        assert_eq!(scene.window(win).unwrap().focused_layer, Some(a));
        assert_eq!(scene.set_focus(b), (Some(a), Some(b)));
        scene.remove_layer(b);
        assert_eq!(scene.window(win).unwrap().focused_layer, None);
    }

    /// Natives can still mutate the public `fonts`/`windows` vectors with a
    /// direct `retain`; the verified fallback must keep lookups correct even
    /// though the index map then points at a shifted slot.
    #[test]
    fn lookups_fall_back_correctly_when_natives_retain_directly() {
        let mut scene = Scene::default();
        let win = scene.add_window("a", (1, 1));
        let _win_b = scene.add_window("b", (1, 1));
        let f1 = scene.add_font("one".into(), 10, [0; 4]);
        let f2 = scene.add_font("two".into(), 10, [0; 4]);
        scene.fonts.retain(|f| f.id != f1);
        assert!(scene.font(f1).is_none());
        assert_eq!(scene.font(f2).unwrap().id, f2);
        assert_eq!(scene.window(win).unwrap().title, "a");
    }

    #[test]
    fn revision_tracks_mutations_but_not_dirty_clear() {
        let mut scene = Scene::default();
        let start = scene.revision();
        let win = scene.add_window("t", (1, 1));
        assert!(scene.revision() > start, "add_window bumps revision");
        let layer = scene.add_layer(win, None);
        let after_add = scene.revision();
        scene.layer_mut(layer).unwrap().opacity = 0.5;
        assert!(scene.revision() > after_add, "layer_mut bumps revision");
        let bitmap = scene.add_bitmap(1, 1, vec![0; 4]);
        let after_bitmap = scene.revision();
        scene.set_bitmap_clean(bitmap);
        assert_eq!(
            scene.revision(),
            after_bitmap,
            "clearing the renderer dirty flag is not a logical mutation"
        );
        let after_clean = scene.revision();
        scene.remove_layer(layer);
        assert!(
            scene.revision() > after_clean,
            "remove_layer bumps revision"
        );
    }
}
