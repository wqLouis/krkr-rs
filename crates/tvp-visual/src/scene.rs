//! Logical scene model for the TVP visual natives.
//!
//! This is the **shared contract** between the natives (`crates/tvp-visual`)
//! and the Bevy renderer (`crates/render`): natives mutate the scene under a
//! write lock, render systems read it under a read lock. Everything is
//! id-based; invalid ids are no-ops. See WAVE3.md.
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
    /// Render order, back -> front.
    pub layers: Vec<u32>,
}

/// TVP drawable types used by `Layer.type`.
///
/// The renderer currently uses Bevy's built-in sprite pipeline. The values are
/// kept as the native integer contract so unsupported future TVP modes can
/// still round-trip without changing this shared model.
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
    pub hit_threshold: i32,
    /// Hit-test mode (reference `tTVPHitType`): `htMask=0` (per-pixel
    /// threshold) or `htProvince=1` (non-transparent province). Stored so
    /// `SelectItemBase` can set it; input hit-testing reads it later.
    pub hit_type: i32,
    /// Mouse-cursor id (reference `tTVPCursorType`, e.g. `crDefault=0`,
    /// `crHandPoint=-21`). Stored; the desktop cursor is not yet driven.
    pub cursor: i32,
    /// Draw face (`dfBoth=0`, `dfMain=1`, `dfMask=2`, `dfProvince=3`,
    /// `dfAddAlpha=4`). Stored; the renderer always draws the main image.
    pub face: i32,
    /// `holdAlpha` — keep the alpha when drawing. Stored only for now.
    pub hold_alpha: bool,
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

/// One font face (text rendering arrives in milestone 3B).
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

/// Events natives queue for the app's update loop.
#[derive(Debug, Clone, Copy)]
pub enum VmEvent {
    /// The timer with this id fired (its TJS callback must run).
    TimerFire { timer_id: u32 },
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
            hit_threshold: 16,
            hit_type: 0,
            cursor: 0,
            face: 0,
            hold_alpha: false,
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

    pub fn remove_layer(&mut self, id: u32) {
        let Some(root) = self.layer(id) else {
            return;
        };
        let (win, parent) = (root.window, root.parent);
        // Collect the whole subtree: a destroyed parent takes its children
        // with it (the reference layer tree owns them). Descendants removed
        // here get a no-op `remove_layer` when their own destroy runs.
        let mut stack = vec![id];
        let mut remove = Vec::new();
        while let Some(cur) = stack.pop() {
            if let Some(l) = self.layer(cur) {
                remove.push(cur);
                stack.extend(l.children.iter().copied());
            }
        }
        if let Some(w) = self.window_mut(win) {
            w.layers.retain(|x| !remove.contains(x));
            if let Some(p) = w.primary_layer
                && remove.contains(&p)
            {
                w.primary_layer = None;
            }
        }
        if let Some(p) = parent
            && let Some(pl) = self.layer_mut(p)
        {
            pl.children.retain(|x| !remove.contains(x));
        }
        self.layers.retain(|l| !remove.contains(&l.id));
        // Indices shifted: rebuild the id→index map.
        self.layer_index.clear();
        for (index, layer) in self.layers.iter().enumerate() {
            self.layer_index.insert(layer.id, index);
        }
        self.touch();
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

    pub fn font_mut(&mut self, id: u32) -> Option<&mut FontState> {
        self.touch();
        let index = self
            .font_index
            .get(&id)
            .copied()
            .filter(|&index| self.fonts.get(index).is_some_and(|f| f.id == id));
        match index {
            Some(index) => self.fonts.get_mut(index),
            None => self.fonts.iter_mut().find(|f| f.id == id),
        }
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
