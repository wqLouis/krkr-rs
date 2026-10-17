//! VM-pixel → device-pixel translation for the presented frame.
//!
//! Scripts and the layer tree describe the scene in VM pixels: the size the
//! game declared for its window. The framebuffer is in device pixels, which on
//! most displays is a different size. [`PixelTranslation`] is the single
//! mapping between the two spaces — a uniform scale of
//! `min(device / vm)` per axis plus a centred offset, so the whole game
//! surface is visible and the leftover margin stays clear.
//!
//! The mapping is applied to the projected tree by
//! [`translate_render_scene`]. Input and readback travel the other way: a
//! pointer position becomes a VM point with [`PixelTranslation::to_vm_point`],
//! and one VM pixel resolves to the device pixels that cover it with
//! [`PixelTranslation::vm_pixel_region`].
//!
//! Both sizes are passed in; nothing here reads a platform value or keeps
//! global state. A size pair that cannot produce a mapping is a typed error
//! rather than a substituted default.

use api::geometry::{PixelRegion, RectF};
use render::{Clip, RenderScene};

/// A size pair that cannot produce a uniform fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TranslateError {
    /// One of the sizes has a zero dimension, so there is nothing to fit.
    #[error("cannot fit a {vm:?} game surface into a {device:?} framebuffer: a dimension is zero")]
    EmptyDimension {
        /// The VM (game) size that was fitted.
        vm: (u32, u32),
        /// The device (framebuffer) size it was fitted into.
        device: (u32, u32),
    },
}

/// VM pixels ↔ device pixels for one pair of surface sizes.
///
/// The mapping is axis-aligned and uniform: one `scale` for both axes, then
/// `offset` to centre the result inside the framebuffer. It is derived from
/// the two sizes and never changes with the frame clock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PixelTranslation {
    /// Device pixels per VM pixel, the same on both axes.
    pub scale: f32,
    /// Position of the scaled VM surface's top-left corner in the framebuffer.
    pub offset: (f32, f32),
    /// The VM (script-visible) size.
    pub vm_size: (u32, u32),
    /// The framebuffer size in device pixels.
    pub device_size: (u32, u32),
}

impl PixelTranslation {
    /// Builds the uniform fit of `vm_size` inside `device_size`.
    ///
    /// The scale is the smaller of the two per-axis ratios, so the whole VM
    /// surface fits on both axes; the offset centres the scaled surface, which
    /// leaves the remaining margin as an even letterbox or pillarbox. The
    /// ratios are divided in `f64` and narrowed once each, so the scale keeps
    /// the precision of the division.
    ///
    /// # Errors
    ///
    /// Returns [`TranslateError::EmptyDimension`] when either size has a zero
    /// dimension: an empty surface has no uniform fit and no substitute scale
    /// is invented for it.
    pub fn fit(vm_size: (u32, u32), device_size: (u32, u32)) -> Result<Self, TranslateError> {
        if vm_size.0 == 0 || vm_size.1 == 0 || device_size.0 == 0 || device_size.1 == 0 {
            return Err(TranslateError::EmptyDimension {
                vm: vm_size,
                device: device_size,
            });
        }
        let scale_x = f64::from(device_size.0) / f64::from(vm_size.0);
        let scale_y = f64::from(device_size.1) / f64::from(vm_size.1);
        let scale = scale_x.min(scale_y);
        let offset_x = (f64::from(device_size.0) - f64::from(vm_size.0) * scale) / 2.0;
        let offset_y = (f64::from(device_size.1) - f64::from(vm_size.1) * scale) / 2.0;
        Ok(Self {
            scale: scale as f32,
            offset: (offset_x as f32, offset_y as f32),
            vm_size,
            device_size,
        })
    }

    /// Whether the mapping leaves VM pixels where they are.
    ///
    /// True when the VM surface covers the framebuffer exactly, so the
    /// projected tree can be drawn unchanged.
    pub fn is_identity(&self) -> bool {
        self.scale == 1.0 && self.offset == (0.0, 0.0)
    }

    /// Maps a point from VM pixels to device pixels.
    pub fn to_device_point(&self, p: (f32, f32)) -> (f32, f32) {
        (
            p.0 * self.scale + self.offset.0,
            p.1 * self.scale + self.offset.1,
        )
    }

    /// Maps a rectangle from VM pixels to device pixels.
    ///
    /// The origin is scaled and offset; the extent is only scaled, so the
    /// rectangle keeps its shape and travels with the surface.
    pub fn to_device_rect(&self, r: RectF) -> RectF {
        let (x, y) = self.to_device_point((r.x, r.y));
        RectF::new(x, y, r.width * self.scale, r.height * self.scale)
    }

    /// Maps a point from device pixels back to VM pixels.
    ///
    /// This is the exact inverse of [`PixelTranslation::to_device_point`].
    pub fn to_vm_point(&self, p: (f32, f32)) -> (f32, f32) {
        (
            (p.0 - self.offset.0) / self.scale,
            (p.1 - self.offset.1) / self.scale,
        )
    }

    /// The device pixels one VM pixel covers.
    ///
    /// Text and other rasterized content are rasterized at this multiple of
    /// their VM size, so an edge that lands on a VM pixel boundary also lands
    /// on a device pixel boundary.
    pub fn raster_scale(&self) -> f32 {
        self.scale
    }

    /// The device-pixel region covered by the VM pixel at `(x, y)`.
    ///
    /// The VM pixel spans `[x·s+ox, (x+1)·s+ox)` on each axis. The start is
    /// floored and the end ceilinged, so the reported region covers every
    /// device pixel the VM pixel touches; the region is at least one pixel
    /// wide on each axis and clipped to `device_size`. At a scale of one with
    /// no offset it is exactly the single VM pixel.
    pub fn vm_pixel_region(&self, x: u32, y: u32) -> PixelRegion {
        let (left, width) = self.axis_span(x, self.offset.0, self.device_size.0);
        let (top, height) = self.axis_span(y, self.offset.1, self.device_size.1);
        PixelRegion::new(left, top, width, height)
    }

    /// One axis of [`PixelTranslation::vm_pixel_region`]: the first device
    /// pixel covering VM pixel `pos` and how many pixels the span covers.
    fn axis_span(&self, pos: u32, offset: f32, device: u32) -> (u32, u32) {
        let scale = f64::from(self.scale);
        let offset = f64::from(offset);
        let start = (f64::from(pos) * scale + offset).floor();
        let end = ((f64::from(pos) + 1.0) * scale + offset).ceil();
        let limit = f64::from(device);
        let start = start.clamp(0.0, limit) as u32;
        let end = end.clamp(0.0, limit) as u32;
        let width = end.saturating_sub(start).max(1);
        // Keep the span inside the framebuffer. A mapping built by `fit` has a
        // non-zero `device`, so this leaves at least one pixel.
        let start = start.min(device.saturating_sub(1));
        let width = width.min(device.saturating_sub(start));
        (start, width)
    }
}

/// Rewrites a projected tree from VM pixels into device pixels.
///
/// Every node's `dst` and every rectangle clip is mapped. A mask clip and a
/// node's `src` sub-rectangle address texture pixels, which are already in
/// their own space, so they are left alone. At an identity mapping the tree is
/// unchanged.
pub fn translate_render_scene(scene: &mut RenderScene, translation: &PixelTranslation) {
    if translation.is_identity() {
        return;
    }
    for node in &mut scene.nodes {
        node.dst = translation.to_device_rect(node.dst);
        if let Some(Clip::Rect(clip)) = node.clip.as_mut() {
            *clip = translation.to_device_rect(*clip);
        }
    }
}
