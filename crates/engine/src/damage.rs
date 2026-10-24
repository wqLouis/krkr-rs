//! The damage model: the part of a surface that changed since the last full
//! repaint.
//!
//! The model is a region of a surface and nothing more. It knows no layers,
//! windows, script objects or images: the engine accumulates the damage while
//! the scene is mutated, and the render backend uses the region to repaint
//! less than the whole surface.
//!
//! The region is **conservative**: it covers every pixel whose rendered
//! result may differ from the previous frame, and it may cover more. A
//! region that under-approximates would leave stale pixels on screen, so
//! every change that cannot be bounded asks for a full repaint instead of
//! guessing at an area.
//!
//! Coordinates are VM pixels — the logical screen the script sees. The
//! mapping to device pixels belongs to the engine, which owns it; a backend
//! receives the region as it is and never interprets the screen's meaning.

use crate::scene::Rect;

/// The part of a surface that changed since the last full repaint.
///
/// A region is *empty* (nothing changed), *full* (everything must be
/// repainted) or a list of rectangles that together cover every change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DamageRegion {
    /// Whether the whole surface changed. When it is set, `rects` is ignored.
    full: bool,
    /// The damaged rectangles, in the order they were added.
    rects: Vec<Rect>,
}

impl DamageRegion {
    /// A region with no damage.
    pub fn empty() -> Self {
        Self {
            full: false,
            rects: Vec::new(),
        }
    }

    /// A region that covers the whole surface.
    pub fn full() -> Self {
        Self {
            full: true,
            rects: Vec::new(),
        }
    }

    /// Whether the region covers the whole surface.
    pub fn covers_everything(&self) -> bool {
        self.full
    }

    /// Whether the region holds no damage at all.
    pub fn is_empty(&self) -> bool {
        !self.full && self.rects.is_empty()
    }

    /// The damaged rectangles, in the order they were added.
    ///
    /// A full region holds none: the whole surface is the damage.
    pub fn rects(&self) -> &[Rect] {
        &self.rects
    }

    /// Add a rectangle to the region.
    ///
    /// An empty rectangle adds nothing. A full region stays full.
    pub fn add(&mut self, rect: Rect) {
        if self.full || rect.w == 0 || rect.h == 0 {
            return;
        }
        self.rects.push(rect);
    }

    /// Fold another region into this one.
    ///
    /// A full other makes this region full and drops its rectangles.
    pub fn merge(&mut self, other: &DamageRegion) {
        if other.full {
            self.full = true;
            self.rects.clear();
            return;
        }
        for rect in &other.rects {
            self.add(*rect);
        }
    }

    /// The smallest rectangle that covers every damaged rectangle.
    ///
    /// `None` for an empty region and for a full one: a full region is not
    /// bounded by a rectangle.
    pub fn bounds(&self) -> Option<Rect> {
        if self.full {
            return None;
        }
        let mut bounds: Option<Rect> = None;
        for rect in &self.rects {
            bounds = Some(match bounds {
                None => *rect,
                Some(current) => {
                    let left = current.x.min(rect.x);
                    let top = current.y.min(rect.y);
                    let right = current
                        .x
                        .saturating_add(extent(current.w))
                        .max(rect.x.saturating_add(extent(rect.w)));
                    let bottom = current
                        .y
                        .saturating_add(extent(current.h))
                        .max(rect.y.saturating_add(extent(rect.h)));
                    Rect::new(left, top, span(left, right), span(top, bottom))
                }
            });
        }
        bounds
    }

    /// Drop every recorded rectangle and the full flag.
    pub fn clear(&mut self) {
        self.full = false;
        self.rects.clear();
    }
}

/// Clamp a pixel extent to a non-negative `i32` (a plain `as i32` would wrap
/// for sizes above `i32::MAX`).
fn extent(value: u32) -> i32 {
    value.min(i32::MAX as u32) as i32
}

/// The width between two edges. The larger edge minus the smaller one is
/// non-negative and fits, so the cast is exact.
fn span(from: i32, to: i32) -> u32 {
    to.saturating_sub(from) as u32
}

/// One layer whose content or geometry may have changed.
///
/// The record holds no surface rectangle: the scene knows *what* changed, and
/// the engine resolves it into surface rectangles when it owns both the scene
/// and the projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerDamage {
    /// The layer's id.
    pub layer: u32,
    /// The layer's own rectangle before the change, in its parent's
    /// coordinates. `None` when the change cannot have moved the layer.
    pub previous: Option<Rect>,
}

/// The changes a scene accumulated since the last collection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeLog {
    /// The layers that changed, in the order they changed.
    pub changed: Vec<LayerDamage>,
    /// Whether the whole surface must be repainted.
    pub full: bool,
}

impl ChangeLog {
    /// A log with no changes.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Whether the log holds no change at all.
    pub fn is_empty(&self) -> bool {
        !self.full && self.changed.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Rect;

    fn rect(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect::new(x, y, w, h)
    }

    #[test]
    fn an_empty_region_holds_no_damage() {
        let region = DamageRegion::empty();
        assert!(region.is_empty());
        assert!(!region.covers_everything());
        assert!(region.rects().is_empty());
        assert_eq!(region.bounds(), None);
    }

    #[test]
    fn a_full_region_covers_the_surface() {
        let region = DamageRegion::full();
        assert!(region.covers_everything());
        assert!(!region.is_empty());
        assert!(region.rects().is_empty());
        assert_eq!(region.bounds(), None);
    }

    #[test]
    fn an_empty_rectangle_adds_nothing() {
        let mut region = DamageRegion::empty();
        region.add(rect(0, 0, 0, 10));
        region.add(rect(0, 0, 10, 0));
        assert!(region.is_empty());
    }

    #[test]
    fn a_full_region_ignores_additions() {
        let mut region = DamageRegion::full();
        region.add(rect(1, 2, 3, 4));
        assert!(region.covers_everything());
        assert!(region.rects().is_empty());
    }

    #[test]
    fn the_bounds_cover_every_rectangle() {
        let mut region = DamageRegion::empty();
        region.add(rect(10, 10, 5, 5));
        region.add(rect(-5, 20, 5, 5));
        let bounds = region.bounds().expect("bounds");
        assert_eq!(bounds.x, -5);
        assert_eq!(bounds.y, 10);
        assert_eq!(bounds.w, 20);
        assert_eq!(bounds.h, 15);
    }

    #[test]
    fn merging_a_full_region_makes_the_result_full() {
        let mut region = DamageRegion::empty();
        region.add(rect(0, 0, 4, 4));
        region.merge(&DamageRegion::full());
        assert!(region.covers_everything());
        assert!(region.rects().is_empty());
    }

    #[test]
    fn merging_folds_the_rectangles_in() {
        let mut other = DamageRegion::empty();
        other.add(rect(0, 0, 4, 4));
        other.add(rect(8, 8, 2, 2));
        let mut region = DamageRegion::empty();
        region.merge(&other);
        assert_eq!(region.rects().len(), 2);
        assert!(!region.covers_everything());
    }

    #[test]
    fn clearing_drops_the_rectangles_and_the_full_flag() {
        let mut region = DamageRegion::full();
        region.clear();
        assert!(region.is_empty());
        assert!(!region.covers_everything());
    }
}
