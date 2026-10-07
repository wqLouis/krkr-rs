//! Texture objects and descriptors.
//!
//! A [`Texture`] is an immutable, backend-agnostic handle created by the
//! render pipeline. The CPU/GPU resource it names lives in the L2-owned
//! [`crate::RenderState`], keyed by the handle's identity. Cloning a
//! `Texture` clones the handle, never the image.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Storage format of a [`Texture`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TextureFormat {
    /// 8-bit RGBA with sRGB transfer characteristics; the canonical image
    /// format of the pipeline.
    #[default]
    Rgba8Srgb,
    /// 8-bit RGBA, linear; used for masks and raw data textures.
    Rgba8Unorm,
    /// 8-bit single-channel coverage; used for glyph atlases.
    R8Unorm,
}

impl TextureFormat {
    /// Returns the number of bytes occupied by one pixel in this format.
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Rgba8Srgb | Self::Rgba8Unorm => 4,
            Self::R8Unorm => 1,
        }
    }

    /// Returns `true` if the format stores an alpha channel.
    pub const fn has_alpha(self) -> bool {
        matches!(self, Self::Rgba8Srgb | Self::Rgba8Unorm)
    }
}

/// Description used to create a [`Texture`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextureDesc {
    /// Width in pixels; must be non-zero.
    pub width: u32,
    /// Height in pixels; must be non-zero.
    pub height: u32,
    /// Pixel format.
    pub format: TextureFormat,
    /// Number of mip levels; must be at least one.
    pub mip_levels: u32,
}

impl TextureDesc {
    /// Creates a single-mip description in the preferred format.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            format: TextureFormat::default(),
            mip_levels: 1,
        }
    }

    /// Returns a copy of this description with a different format.
    pub fn with_format(mut self, format: TextureFormat) -> Self {
        self.format = format;
        self
    }

    /// Returns a copy of this description with a different mip count.
    pub fn with_mip_levels(mut self, mip_levels: u32) -> Self {
        self.mip_levels = mip_levels;
        self
    }

    /// Returns `true` if the description has no positive extent.
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Returns the number of bytes in the base (level 0) image.
    pub fn base_byte_len(&self) -> u64 {
        (self.width as u64) * (self.height as u64) * u64::from(self.format.bytes_per_pixel())
    }
}

impl Default for TextureDesc {
    fn default() -> Self {
        Self::new(1, 1)
    }
}

/// Immutable, backend-agnostic texture handle.
///
/// Two textures compare equal (and hash equal) only when they share the same
/// identity, even if their descriptors match.
#[derive(Clone)]
pub struct Texture {
    inner: Arc<TextureInner>,
}

/// Shared, immutable contents of a [`Texture`] handle.
#[derive(Debug)]
struct TextureInner {
    desc: TextureDesc,
}

impl Texture {
    /// Creates a new texture handle from a validated descriptor.
    pub(crate) fn new(desc: TextureDesc) -> Self {
        Self {
            inner: Arc::new(TextureInner { desc }),
        }
    }

    /// Returns the descriptor this texture was created with.
    pub fn desc(&self) -> &TextureDesc {
        &self.inner.desc
    }

    /// Returns the texture width in pixels.
    pub fn width(&self) -> u32 {
        self.inner.desc.width
    }

    /// Returns the texture height in pixels.
    pub fn height(&self) -> u32 {
        self.inner.desc.height
    }

    /// Returns a process-unique key for the handle's backing allocation.
    pub(crate) fn key(&self) -> usize {
        Arc::as_ptr(&self.inner) as usize
    }
}

impl fmt::Debug for Texture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Texture")
            .field("key", &self.key())
            .field("desc", &self.inner.desc)
            .finish()
    }
}

impl PartialEq for Texture {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for Texture {}

impl Hash for Texture {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key().hash(state);
    }
}
