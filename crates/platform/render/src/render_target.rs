//! Offscreen render-target objects and descriptors.
//!
//! A [`RenderTarget`] is an immutable, backend-agnostic handle to an RGBA8
//! offscreen surface. Like a [`crate::Texture`], the CPU/GPU resource it names
//! lives in the L2-owned [`crate::RenderState`], keyed by the handle's
//! identity; cloning a `RenderTarget` clones the handle, never the pixels.
//!
//! The target is created by [`crate::create_render_target`], filled by
//! [`crate::render_to_target`] and read back by
//! [`crate::read_render_target`]. That readback is the primitive behind the
//! `Bitmap.getPixel` / `Layer.saveLayerImage` compatibility surface and the
//! CPU-correctness baseline.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::texture::TextureFormat;

/// Monotonic source of [`RenderTarget`] identities.
///
/// A counter, rather than the backing allocation address, removes the ABA
/// hazard where a dropped-and-freed handle could be swapped for a new one at
/// the same address and silently reuse a stale target.
static NEXT_TARGET_ID: AtomicU64 = AtomicU64::new(1);

/// Description for creating a [`RenderTarget`].
///
/// Offscreen targets are always 8-bit RGBA; [`TextureFormat::R8Unorm`] is
/// rejected because a colour target needs an alpha-capable format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RenderTargetDesc {
    /// Width in pixels; must be non-zero.
    pub width: u32,
    /// Height in pixels; must be non-zero.
    pub height: u32,
    /// Pixel format; only the RGBA8 variants are valid.
    pub format: TextureFormat,
}

impl RenderTargetDesc {
    /// Creates a single-sample RGBA8 target description in the preferred
    /// format.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            format: TextureFormat::Rgba8Srgb,
        }
    }

    /// Returns a copy of this description with a different format.
    pub fn with_format(mut self, format: TextureFormat) -> Self {
        self.format = format;
        self
    }

    /// Returns `true` if the description has no positive extent.
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Returns the number of bytes in the target image.
    pub fn byte_len(&self) -> u64 {
        (self.width as u64) * (self.height as u64) * u64::from(self.format.bytes_per_pixel())
    }
}

impl Default for RenderTargetDesc {
    fn default() -> Self {
        Self::new(1, 1)
    }
}

/// Immutable, backend-agnostic offscreen render-target handle.
///
/// Two targets compare equal (and hash equal) only when they share the same
/// identity, even if their descriptors match.
#[derive(Clone)]
pub struct RenderTarget {
    inner: Arc<RenderTargetInner>,
}

/// Shared, immutable contents of a [`RenderTarget`] handle.
#[derive(Debug)]
struct RenderTargetInner {
    id: u64,
    desc: RenderTargetDesc,
}

impl RenderTarget {
    /// Creates a new target handle from a validated descriptor.
    pub(crate) fn new(desc: RenderTargetDesc) -> Self {
        Self {
            inner: Arc::new(RenderTargetInner {
                id: NEXT_TARGET_ID.fetch_add(1, Ordering::Relaxed),
                desc,
            }),
        }
    }

    /// Returns the descriptor this target was created with.
    pub fn desc(&self) -> &RenderTargetDesc {
        &self.inner.desc
    }

    /// Returns the target width in pixels.
    pub fn width(&self) -> u32 {
        self.inner.desc.width
    }

    /// Returns the target height in pixels.
    pub fn height(&self) -> u32 {
        self.inner.desc.height
    }

    /// Returns a process-unique key for the handle's backing allocation.
    pub(crate) fn key(&self) -> u64 {
        self.inner.id
    }
}

impl fmt::Debug for RenderTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RenderTarget")
            .field("key", &self.key())
            .field("desc", &self.inner.desc)
            .finish()
    }
}

impl PartialEq for RenderTarget {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for RenderTarget {}

impl Hash for RenderTarget {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key().hash(state);
    }
}
