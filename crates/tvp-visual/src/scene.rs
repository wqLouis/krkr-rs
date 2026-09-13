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
    pub is_primary: bool,
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
}

impl Scene {
    pub fn add_window(&mut self, title: impl Into<String>, inner_size: (u32, u32)) -> u32 {
        let id = self.next_window;
        self.next_window += 1;
        self.windows.push(WindowState {
            id,
            title: title.into(),
            inner_size,
            visible: true,
            opacity: 1.0,
            primary_layer: None,
            layers: Vec::new(),
        });
        id
    }

    pub fn window(&self, id: u32) -> Option<&WindowState> {
        self.windows.iter().find(|w| w.id == id)
    }

    pub fn window_mut(&mut self, id: u32) -> Option<&mut WindowState> {
        self.windows.iter_mut().find(|w| w.id == id)
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
            visible: true,
            opacity: 1.0,
            blend_type: LT_ALPHA,
            z_order: 0,
            fill_color: None,
            image_left: 0,
            image_top: 0,
            image_width: 0,
            image_height: 0,
            hit_threshold: 0,
            hit_type: 0,
            cursor: 0,
            face: 0,
            hold_alpha: false,
            is_primary: false,
        });
        // First layer of a window becomes its primary layer.
        if primary {
            if let Some(w) = self.window_mut(window) {
                w.primary_layer = Some(id);
            }
            if let Some(l) = self.layer_mut(id) {
                l.is_primary = true;
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
        id
    }

    pub fn layer(&self, id: u32) -> Option<&LayerState> {
        self.layers.iter().find(|l| l.id == id)
    }

    pub fn layer_mut(&mut self, id: u32) -> Option<&mut LayerState> {
        self.layers.iter_mut().find(|l| l.id == id)
    }

    pub fn remove_layer(&mut self, id: u32) {
        // Copy the ids out of the borrow so `self` can be mutated below.
        let (win, parent) = match self.layer(id) {
            Some(l) => (l.window, l.parent),
            None => return,
        };
        if let Some(w) = self.window_mut(win) {
            w.layers.retain(|&x| x != id);
            if w.primary_layer == Some(id) {
                w.primary_layer = None;
            }
        }
        if let Some(p) = parent
            && let Some(pl) = self.layer_mut(p)
        {
            pl.children.retain(|&x| x != id);
        }
        self.layers.retain(|l| l.id != id);
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
    }

    pub fn add_bitmap(&mut self, width: u32, height: u32, rgba: Vec<u8>) -> u32 {
        let id = self.next_bitmap;
        self.next_bitmap += 1;
        self.bitmaps.push(BitmapState {
            id,
            width,
            height,
            rgba,
            name: None,
            dirty: true,
        });
        id
    }

    pub fn bitmap(&self, id: u32) -> Option<&BitmapState> {
        self.bitmaps.iter().find(|b| b.id == id)
    }

    pub fn bitmap_mut(&mut self, id: u32) -> Option<&mut BitmapState> {
        self.bitmaps.iter_mut().find(|b| b.id == id)
    }

    pub fn add_font(&mut self, face: String, height: i32, color: [u8; 4]) -> u32 {
        let id = self.next_font;
        self.next_font += 1;
        self.fonts.push(FontState {
            id,
            face,
            height,
            color,
            bold: false,
            italic: false,
        });
        id
    }

    pub fn font(&self, id: u32) -> Option<&FontState> {
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
        let mut roots = window_state.layers.clone();
        // Include layers created by older callers that did not update the
        // window list. This also makes the contract robust to hand-built test
        // scenes and stale FFI links.
        for layer in self
            .layers
            .iter()
            .filter(|l| l.window == window && l.parent.is_none())
        {
            if !roots.contains(&layer.id) {
                roots.push(layer.id);
            }
        }
        let root_sibling_order = roots.clone();
        self.sort_siblings(&mut roots, None, Some(&root_sibling_order));

        let mut order = Vec::new();
        let mut visited = HashSet::new();
        for id in roots {
            self.append_layer_subtree(window, id, &mut order, &mut visited);
        }
        // A malformed parent link should not hide a layer from rendering.
        // Valid children omitted from a parent's `children` list are found by
        // append_layer_subtree; this final pass is for cycles/invalid links.
        let mut leftovers: Vec<u32> = self
            .layers
            .iter()
            .filter(|l| l.window == window && !visited.contains(&l.id))
            .map(|l| l.id)
            .collect();
        self.sort_siblings(&mut leftovers, None, None);
        for id in leftovers {
            self.append_layer_subtree(window, id, &mut order, &mut visited);
        }
        order
    }

    fn sort_siblings(&self, ids: &mut [u32], parent: Option<u32>, preferred_order: Option<&[u32]>) {
        let sibling_order = preferred_order
            .map(|ids| ids.to_vec())
            .or_else(|| parent.and_then(|id| self.layer(id).map(|l| l.children.clone())));
        ids.sort_by(|a, b| {
            let a_layer = self.layer(*a);
            let b_layer = self.layer(*b);
            let az = a_layer.map_or(0, |l| l.z_order);
            let bz = b_layer.map_or(0, |l| l.z_order);
            let apos = sibling_order
                .as_ref()
                .and_then(|v| v.iter().position(|id| id == a))
                .unwrap_or(usize::MAX);
            let bpos = sibling_order
                .as_ref()
                .and_then(|v| v.iter().position(|id| id == b))
                .unwrap_or(usize::MAX);
            (az, apos, *a).cmp(&(bz, bpos, *b))
        });
    }

    fn append_layer_subtree(
        &self,
        window: u32,
        id: u32,
        order: &mut Vec<u32>,
        visited: &mut HashSet<u32>,
    ) {
        let Some(layer) = self.layer(id) else { return };
        if layer.window != window || !visited.insert(id) {
            return;
        }
        order.push(id);

        let mut children = layer.children.clone();
        let missing_children: Vec<u32> = self
            .layers
            .iter()
            .filter(|candidate| {
                candidate.window == window
                    && candidate.parent == Some(id)
                    && !children.contains(&candidate.id)
            })
            .map(|candidate| candidate.id)
            .collect();
        children.extend(missing_children);
        self.sort_siblings(&mut children, Some(id), None);
        for child in children {
            self.append_layer_subtree(window, child, order, visited);
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
}
