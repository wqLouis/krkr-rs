//! Logical scene model for the TVP visual natives.
//!
//! This is the **shared contract** between the natives (`crates/tvp-visual`)
//! and the Bevy renderer (`crates/render`): natives mutate the scene under a
//! write lock, render systems read it under a read lock. Everything is
//! id-based; invalid ids are no-ops. See WAVE3.md.
//!
//! No Bevy types here — this crate stays engine-agnostic.

use std::collections::HashMap;

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
    /// Z order among parent-less layers of the same window.
    pub z_order: i32,
    /// Solid fill (RGBA, straight alpha) used when there is no bitmap.
    pub fill_color: Option<[u8; 4]>,
    pub hit_threshold: i32,
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
#[derive(Debug, Clone, Copy, Default)]
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
            z_order: 0,
            fill_color: None,
            hit_threshold: 0,
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

    /// All layers of a window sorted back -> front (parents before children,
    /// then by insertion/z-order among siblings). Simple stable sort by
    /// (z_order, insertion index).
    pub fn window_layer_order(&self, window: u32) -> Vec<u32> {
        let mut order: Vec<(u32, i32, usize)> = self
            .layers
            .iter()
            .enumerate()
            .filter(|(_, l)| l.window == window && l.parent.is_none())
            .map(|(i, l)| (l.id, l.z_order, i))
            .collect();
        order.sort_by_key(|&(_, z, i)| (z, i as i64));
        order.into_iter().map(|(id, _, _)| id).collect()
    }
}

/// Bitmap cache for `Bitmap(name)` reuse, keyed by normalized storage name.
#[derive(Default)]
pub struct BitmapCache {
    pub by_name: HashMap<String, u32>,
}
