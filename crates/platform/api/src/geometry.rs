//! Two-dimensional geometry primitives shared across the middle-layer API.
//!
//! These are plain values. The only logic they carry is pure, self-contained
//! geometry math (containment, intersection, union and affine composition);
//! no backend, I/O or engine state is involved. The helpers exist so the
//! middle layer and the bottom core agree on coordinate conventions instead
//! of each re-implementing them.

/// Axis-aligned rectangle in floating-point coordinates.
///
/// The origin `(x, y)` is the rectangle's minimum corner; `width` and
/// `height` are its extents. A rectangle whose width or height is not
/// strictly positive is treated as empty by the helper methods.
///
/// This is the type used for destination rectangles in the render tree and
/// for any logical (non-integer) region.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RectF {
    /// Left edge (minimum x).
    pub x: f32,
    /// Top edge (minimum y).
    pub y: f32,
    /// Horizontal extent.
    pub width: f32,
    /// Vertical extent.
    pub height: f32,
}

impl RectF {
    /// The empty rectangle at the origin.
    pub const ZERO: Self = Self {
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 0.0,
    };

    /// Creates a rectangle from its origin and extent.
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Creates a rectangle from a minimum corner and a maximum corner.
    ///
    /// The extents are `max - min`; callers must pass a maximum that is not
    /// smaller than the minimum, otherwise the rectangle is empty.
    pub const fn from_min_max(min_x: f32, min_y: f32, max_x: f32, max_y: f32) -> Self {
        Self {
            x: min_x,
            y: min_y,
            width: max_x - min_x,
            height: max_y - min_y,
        }
    }

    /// Returns the minimum corner `(x, y)`.
    pub const fn min(&self) -> (f32, f32) {
        (self.x, self.y)
    }

    /// Returns the maximum corner `(x + width, y + height)`.
    pub const fn max(&self) -> (f32, f32) {
        (self.x + self.width, self.y + self.height)
    }

    /// Returns the right edge (`x + width`).
    pub const fn right(&self) -> f32 {
        self.x + self.width
    }

    /// Returns the bottom edge (`y + height`).
    pub const fn bottom(&self) -> f32 {
        self.y + self.height
    }

    /// Returns the center point.
    pub const fn center(&self) -> (f32, f32) {
        (self.x + self.width * 0.5, self.y + self.height * 0.5)
    }

    /// Returns `true` if the rectangle has no positive extent.
    pub const fn is_empty(&self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }

    /// Returns `true` if `point` lies inside the half-open rectangle
    /// `[x, right) × [y, bottom)`.
    pub fn contains(&self, point: (f32, f32)) -> bool {
        point.0 >= self.x && point.0 < self.right() && point.1 >= self.y && point.1 < self.bottom()
    }

    /// Returns the overlapping rectangle, or `None` when the rectangles do
    /// not overlap.
    pub fn intersection(&self, other: &Self) -> Option<Self> {
        let min_x = self.x.max(other.x);
        let min_y = self.y.max(other.y);
        let max_x = self.right().min(other.right());
        let max_y = self.bottom().min(other.bottom());
        if max_x > min_x && max_y > min_y {
            Some(Self::new(min_x, min_y, max_x - min_x, max_y - min_y))
        } else {
            None
        }
    }

    /// Returns the smallest rectangle containing both inputs.
    pub fn union(&self, other: &Self) -> Self {
        let min_x = self.x.min(other.x);
        let min_y = self.y.min(other.y);
        let max_x = self.right().max(other.right());
        let max_y = self.bottom().max(other.bottom());
        Self::new(min_x, min_y, max_x - min_x, max_y - min_y)
    }
}

/// Integer pixel region: an origin plus an extent, in whole pixels.
///
/// This is the type used for source sub-rectangles (sprite-sheet frames,
/// dirty upload regions, clipping). Extents are non-negative and edges are
/// computed with saturating arithmetic so degenerate/out-of-range regions
/// cannot overflow.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PixelRegion {
    /// Left edge, in pixels.
    pub x: u32,
    /// Top edge, in pixels.
    pub y: u32,
    /// Width, in pixels.
    pub width: u32,
    /// Height, in pixels.
    pub height: u32,
}

impl PixelRegion {
    /// The empty region at the origin.
    pub const ZERO: Self = Self {
        x: 0,
        y: 0,
        width: 0,
        height: 0,
    };

    /// Creates a region from its origin and extent.
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Creates a region from a minimum corner and an exclusive maximum
    /// corner. Inverted bounds produce an empty region.
    pub const fn from_min_max(min_x: u32, min_y: u32, max_x: u32, max_y: u32) -> Self {
        Self {
            x: min_x,
            y: min_y,
            width: max_x.saturating_sub(min_x),
            height: max_y.saturating_sub(min_y),
        }
    }

    /// Returns the exclusive right edge (`x + width`), saturating.
    pub const fn right(&self) -> u32 {
        self.x.saturating_add(self.width)
    }

    /// Returns the exclusive bottom edge (`y + height`), saturating.
    pub const fn bottom(&self) -> u32 {
        self.y.saturating_add(self.height)
    }

    /// Returns the region's area in pixels, widening to `u64`.
    pub const fn area(&self) -> u64 {
        (self.width as u64) * (self.height as u64)
    }

    /// Returns `true` if the region has no positive extent.
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Returns `true` if `point` lies inside the half-open region
    /// `[x, right) × [y, bottom)`.
    pub const fn contains(&self, point: (u32, u32)) -> bool {
        point.0 >= self.x && point.0 < self.right() && point.1 >= self.y && point.1 < self.bottom()
    }

    /// Returns the overlapping region, or `None` when the regions do not
    /// overlap.
    pub fn intersection(&self, other: &Self) -> Option<Self> {
        let min_x = self.x.max(other.x);
        let min_y = self.y.max(other.y);
        let max_x = self.right().min(other.right());
        let max_y = self.bottom().min(other.bottom());
        if max_x > min_x && max_y > min_y {
            Some(Self::new(min_x, min_y, max_x - min_x, max_y - min_y))
        } else {
            None
        }
    }

    /// Returns the smallest region containing both inputs.
    pub fn union(&self, other: &Self) -> Self {
        let min_x = self.x.min(other.x);
        let min_y = self.y.min(other.y);
        let max_x = self.right().max(other.right());
        let max_y = self.bottom().max(other.bottom());
        Self::new(
            min_x,
            min_y,
            max_x.saturating_sub(min_x),
            max_y.saturating_sub(min_y),
        )
    }
}

/// Affine transform stored as a 2×3 row-major matrix.
///
/// The mapping is
/// `x' = m11·x + m12·y + tx`, `y' = m21·x + m22·y + ty`.
/// It is a plain value; use [`Affine2::then`] to compose transforms and
/// [`Affine2::transform_point`] to apply one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine2 {
    /// Row 0, column 0 (x scale / rotation cosine).
    pub m11: f32,
    /// Row 0, column 1 (x shear / negative rotation sine).
    pub m12: f32,
    /// Row 1, column 0 (y shear / rotation sine).
    pub m21: f32,
    /// Row 1, column 1 (y scale / rotation cosine).
    pub m22: f32,
    /// Translation along x.
    pub tx: f32,
    /// Translation along y.
    pub ty: f32,
}

impl Affine2 {
    /// The identity transform.
    pub const IDENTITY: Self = Self {
        m11: 1.0,
        m12: 0.0,
        m21: 0.0,
        m22: 1.0,
        tx: 0.0,
        ty: 0.0,
    };

    /// Returns the identity transform.
    pub const fn identity() -> Self {
        Self::IDENTITY
    }

    /// Returns a translation transform.
    pub const fn translation(tx: f32, ty: f32) -> Self {
        Self {
            tx,
            ty,
            ..Self::IDENTITY
        }
    }

    /// Returns a non-uniform scale transform.
    pub const fn scale(sx: f32, sy: f32) -> Self {
        Self {
            m11: sx,
            m22: sy,
            ..Self::IDENTITY
        }
    }

    /// Returns a rotation by `radians` around the origin.
    pub fn rotation(radians: f32) -> Self {
        let (sin, cos) = radians.sin_cos();
        Self {
            m11: cos,
            m12: -sin,
            m21: sin,
            m22: cos,
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// Builds the affine matrix equivalent to a [`Transform2D`].
    ///
    /// The composed transform is
    /// `T(translate) · T(pivot) · R(rotation) · S(scale) · T(-pivot)`,
    /// i.e. scale and rotation happen about `pivot`, then the result is
    /// translated.
    pub fn from_transform2d(transform: &Transform2D) -> Self {
        let (sin, cos) = transform.rotation.sin_cos();
        let (sx, sy) = transform.scale;
        let m11 = cos * sx;
        let m12 = -sin * sy;
        let m21 = sin * sx;
        let m22 = cos * sy;
        let (px, py) = transform.pivot;
        let (tx, ty) = transform.translate;
        Self {
            m11,
            m12,
            m21,
            m22,
            tx: tx + px - (m11 * px + m12 * py),
            ty: ty + py - (m21 * px + m22 * py),
        }
    }

    /// Applies `self` first and then `next`: `next ∘ self`.
    pub fn then(&self, next: &Self) -> Self {
        Self {
            m11: next.m11 * self.m11 + next.m12 * self.m21,
            m12: next.m11 * self.m12 + next.m12 * self.m22,
            m21: next.m21 * self.m11 + next.m22 * self.m21,
            m22: next.m21 * self.m12 + next.m22 * self.m22,
            tx: next.m11 * self.tx + next.m12 * self.ty + next.tx,
            ty: next.m21 * self.tx + next.m22 * self.ty + next.ty,
        }
    }

    /// Transforms a point, including translation.
    pub fn transform_point(&self, point: (f32, f32)) -> (f32, f32) {
        (
            self.m11 * point.0 + self.m12 * point.1 + self.tx,
            self.m21 * point.0 + self.m22 * point.1 + self.ty,
        )
    }

    /// Transforms a direction vector, ignoring translation.
    pub fn transform_vector(&self, vector: (f32, f32)) -> (f32, f32) {
        (
            self.m11 * vector.0 + self.m12 * vector.1,
            self.m21 * vector.0 + self.m22 * vector.1,
        )
    }

    /// Returns the determinant of the linear part.
    pub fn determinant(&self) -> f32 {
        self.m11 * self.m22 - self.m12 * self.m21
    }

    /// Returns the inverse transform, or `None` when the linear part is
    /// singular (non-invertible).
    pub fn inverse(&self) -> Option<Self> {
        let det = self.determinant();
        if det.abs() <= f32::EPSILON {
            return None;
        }
        let inv_det = 1.0 / det;
        let m11 = self.m22 * inv_det;
        let m12 = -self.m12 * inv_det;
        let m21 = -self.m21 * inv_det;
        let m22 = self.m11 * inv_det;
        Some(Self {
            m11,
            m12,
            m21,
            m22,
            tx: -(m11 * self.tx + m12 * self.ty),
            ty: -(m21 * self.tx + m22 * self.ty),
        })
    }
}

/// Node-local 2-D transform: translation, scale, rotation and pivot.
///
/// This mirrors the decomposition used by the render tree so the middle
/// layer can keep human-readable layer transforms and the bottom core can
/// expand them to a matrix with [`Affine2::from_transform2d`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform2D {
    /// Translation applied after scaling/rotation.
    pub translate: (f32, f32),
    /// Per-axis scale factor.
    pub scale: (f32, f32),
    /// Rotation in radians.
    pub rotation: f32,
    /// Point that scaling and rotation are performed about.
    pub pivot: (f32, f32),
}

impl Transform2D {
    /// The identity transform.
    pub const IDENTITY: Self = Self {
        translate: (0.0, 0.0),
        scale: (1.0, 1.0),
        rotation: 0.0,
        pivot: (0.0, 0.0),
    };

    /// Returns the identity transform.
    pub const fn identity() -> Self {
        Self::IDENTITY
    }

    /// Returns `true` if this transform maps points to themselves.
    ///
    /// The pivot is irrelevant when both scale is one and rotation is zero,
    /// so it is not part of the comparison.
    pub const fn is_identity(&self) -> bool {
        self.translate.0 == 0.0
            && self.translate.1 == 0.0
            && self.scale.0 == 1.0
            && self.scale.1 == 1.0
            && self.rotation == 0.0
    }

    /// Expands this decomposition into an affine matrix.
    pub fn to_affine(&self) -> Affine2 {
        Affine2::from_transform2d(self)
    }
}

impl Default for Transform2D {
    fn default() -> Self {
        Self::IDENTITY
    }
}
