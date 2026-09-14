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
    /// destroyed: the reference `Invalidate` calls `child->Part()` for each
    /// direct child (`LayerIntf.cpp:551`), so their parent link is cleared and
    /// they stay alive as window-level roots. The old implementation removed
    /// the whole subtree, which silently deleted live children (focus, button
    /// and transition layers all share a parent).
    pub fn remove_layer(&mut self, id: u32) {
        let Some(layer) = self.layer(id) else {
            return;
        };
        let (win, parent) = (layer.window, layer.parent);
        let children = layer.children.clone();
        // `Part()`: sever this layer from its parent (or the window roots).
        if let Some(p) = parent {
            if let Some(pl) = self.layer_mut(p) {
                pl.children.retain(|&x| x != id);
            }
        } else if let Some(w) = self.window_mut(win) {
            w.layers.retain(|&x| x != id);
        }
        // Sever the direct children from this layer; they become roots of
        // the window and must remain in the scene.
        for child in &children {
            if let Some(cl) = self.layer_mut(*child) {
                cl.parent = None;
            }
            if let Some(w) = self.window_mut(win)
                && !w.layers.contains(child)
            {
                w.layers.push(*child);
            }
        }
        self.layers.retain(|l| l.id != id);
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
    fn absolute_offset(&self, id: u32) -> Option<(i32, i32)> {
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
        });
        self.touch();
        id
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
        for layer in self.layers.iter().filter(|l| l.window == window) {
            window_layers.push(layer.id);
            all_children.entry(layer.parent).or_default().push(layer.id);
        }

        // Root list: the maintained window order first, then any root layer
        // created by a caller that did not update the window list. This also
        // makes the contract robust to hand-built test scenes and stale FFI
        // links.
        let mut roots = window_state.layers.clone();
        if let Some(root_children) = all_children.get(&None) {
            for &id in root_children {
                if !roots.contains(&id) {
                    roots.push(id);
                }
            }
        }
        let root_order = roots.clone();
        self.sort_siblings(&mut roots, &root_order);

        let mut order = Vec::new();
        let mut visited = HashSet::new();
        for id in roots {
            self.append_layer_subtree(window, id, &all_children, &mut order, &mut visited);
        }
        // A malformed parent link should not hide a layer from rendering.
        // Valid children omitted from a parent's `children` list are found by
        // append_layer_subtree; this final pass is for cycles/invalid links.
        let mut leftovers: Vec<u32> = window_layers
            .into_iter()
            .filter(|id| !visited.contains(id))
            .collect();
        self.sort_siblings(&mut leftovers, &[]);
        for id in leftovers {
            self.append_layer_subtree(window, id, &all_children, &mut order, &mut visited);
        }
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
        if layer.window != window || !visited.insert(id) {
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
#[derive(Default)]
pub struct BitmapCache {
    pub by_name: HashMap<String, u32>,
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

    /// A layer with an invalid parent and no root-list entry must still be
    /// emitted by the final defensive pass (never silently hidden).
    #[test]
    fn window_layer_order_renders_orphaned_layers_defensively() {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1, 1));
        let orphan = scene.add_layer(win, None);
        scene.window_mut(win).unwrap().layers.clear();
        scene.layer_mut(orphan).unwrap().parent = Some(9999);
        assert_eq!(scene.window_layer_order(win), vec![orphan]);
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

    /// `remove_layer` is `Part()`-only: it detaches the layer but keeps its
    /// direct children alive as window roots (the old implementation deleted
    /// the whole subtree).
    #[test]
    fn remove_layer_detaches_but_keeps_children() {
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
        // The child is now a root of the window and still rendered.
        let order = scene.window_layer_order(win);
        assert!(order.contains(&child));
        assert!(order.contains(&grandchild));
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
