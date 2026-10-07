//! The abstract render tree consumed by the bottom render pipeline.
//!
//! The tree is defined here (L1) so the pipeline can traverse it, but it is
//! owned by the middle layer (L2). It carries drawable structure and blend
//! semantics only: no TVP properties, no engine state.

use api::blend::BlendMode;
use api::geometry::{PixelRegion, RectF, Transform2D};

use crate::texture::Texture;

/// Identifier of a node in a [`RenderScene`] arena.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RenderNodeId(pub u32);

impl RenderNodeId {
    /// Creates a node id from a raw arena index.
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// Returns the raw arena index.
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// Drawable payload of a [`RenderNode`].
#[derive(Clone, Debug, Default, PartialEq)]
pub enum RenderContent {
    /// Container only; contributes its transform but draws nothing.
    #[default]
    None,
    /// Flat fill colour `[r, g, b, a]`.
    Solid([f32; 4]),
    /// Textured quad sourced from a texture object.
    Texture(Texture),
    /// GPU glyph run (optional accelerated text path).
    Glyphs(GlyphRun),
}

/// Clip applied to a node and its subtree.
#[derive(Clone, Debug, PartialEq)]
pub enum Clip {
    /// Axis-aligned rectangle in the node's coordinate space.
    Rect(RectF),
    /// Alpha mask sampled over the node's destination rectangle.
    Mask(Texture),
}

/// One positioned glyph inside a [`GlyphRun`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlyphPlacement {
    /// Unicode code point being drawn.
    pub codepoint: char,
    /// Pen x offset in the run's local space.
    pub x: f32,
    /// Pen y offset in the run's local space.
    pub y: f32,
    /// Pen advance after this glyph.
    pub advance: f32,
    /// Sub-rectangle inside the run's atlas texture.
    pub atlas: PixelRegion,
}

impl GlyphPlacement {
    /// Creates a glyph placement.
    pub const fn new(codepoint: char, x: f32, y: f32, advance: f32, atlas: PixelRegion) -> Self {
        Self {
            codepoint,
            x,
            y,
            advance,
            atlas,
        }
    }
}

/// A run of glyphs sharing one atlas texture and one style.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GlyphRun {
    /// Atlas texture containing the glyph coverage, when the glyphs are
    /// drawn on the GPU.
    pub atlas: Option<Texture>,
    /// Font size in pixels used when the glyphs were rasterized.
    pub size_px: f32,
    /// Text colour `[r, g, b, a]`.
    pub color: [f32; 4],
    /// Positioned glyphs, in pen order.
    pub glyphs: Vec<GlyphPlacement>,
}

impl GlyphRun {
    /// Returns `true` if the run contains no glyphs.
    pub fn is_empty(&self) -> bool {
        self.glyphs.is_empty()
    }

    /// Returns the number of glyphs in the run.
    pub fn len(&self) -> usize {
        self.glyphs.len()
    }
}

/// One node in the render tree.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderNode {
    /// Parent node, if any.
    pub parent: Option<RenderNodeId>,
    /// Child nodes in insertion order; render order is decided by `z`.
    pub children: Vec<RenderNodeId>,
    /// Drawable payload.
    pub content: RenderContent,
    /// Node-local transform (translate/scale/rotate/pivot).
    pub transform: Transform2D,
    /// Destination rectangle in the parent's coordinate space.
    pub dst: RectF,
    /// Source sub-rectangle for textures (sprite-sheet frames, clipping).
    pub src: PixelRegion,
    /// Node opacity in `[0, 1]`.
    pub opacity: f32,
    /// Per-node straight-alpha tint `[r, g, b, a]` applied to textured quads.
    ///
    /// The default `[1, 1, 1, 1]` leaves the sampled texture unchanged. L2
    /// sets this to the text colour for glyph nodes so the glyph atlas can
    /// cache colour-independent coverage and recolour without re-rasterising.
    pub color: [f32; 4],
    /// Whether the node and its subtree are drawn.
    pub visible: bool,
    /// Sort key among siblings; lower values draw first.
    pub z: i32,
    /// Composition mode over the destination.
    pub blend: BlendMode,
    /// Optional clip applied to this node and its subtree.
    pub clip: Option<Clip>,
    /// `holdAlpha` / main-vs-province selection hook.
    pub hold_alpha: bool,
}

impl Default for RenderNode {
    fn default() -> Self {
        Self {
            parent: None,
            children: Vec::new(),
            content: RenderContent::None,
            transform: Transform2D::IDENTITY,
            dst: RectF::ZERO,
            src: PixelRegion::ZERO,
            opacity: 1.0,
            color: [1.0, 1.0, 1.0, 1.0],
            visible: true,
            z: 0,
            blend: BlendMode::default(),
            clip: None,
            hold_alpha: false,
        }
    }
}

/// A complete render tree plus an optional clear colour.
///
/// The middle layer owns the value and performs a field-level sync from its
/// layer system; the pipeline owns traversal and drawing.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderScene {
    /// Root node id.
    pub root: RenderNodeId,
    /// Node arena.
    pub nodes: Vec<RenderNode>,
    /// Optional background clear colour `[r, g, b, a]`.
    pub background: Option<[f32; 4]>,
}

impl Default for RenderScene {
    fn default() -> Self {
        Self {
            root: RenderNodeId(0),
            nodes: Vec::new(),
            background: None,
        }
    }
}

impl RenderScene {
    /// Creates an empty scene rooted at `root`.
    pub fn new(root: RenderNodeId) -> Self {
        Self {
            root,
            nodes: Vec::new(),
            background: None,
        }
    }

    /// Creates a scene containing a single default root node.
    pub fn with_root_node() -> Self {
        Self {
            root: RenderNodeId(0),
            nodes: vec![RenderNode::default()],
            background: None,
        }
    }

    /// Returns the node for `id`, or `None` when the id is out of range.
    pub fn node(&self, id: RenderNodeId) -> Option<&RenderNode> {
        self.nodes.get(id.index() as usize)
    }

    /// Returns a mutable reference to the node for `id`.
    pub fn node_mut(&mut self, id: RenderNodeId) -> Option<&mut RenderNode> {
        self.nodes.get_mut(id.index() as usize)
    }

    /// Appends a node to the arena and returns its id.
    pub fn push(&mut self, node: RenderNode) -> RenderNodeId {
        let id = RenderNodeId(self.nodes.len() as u32);
        self.nodes.push(node);
        id
    }

    /// Returns `true` if the arena contains no nodes.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Returns the number of nodes in the arena.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }
}

/// Sort key used to order siblings for drawing: lower `z` draws first.
///
/// Kept here so the null and Vulkan backends cannot drift in ordering
/// semantics.
pub(crate) fn child_sort_key(scene: &RenderScene, child: RenderNodeId) -> i32 {
    scene.node(child).map_or(0, |node| node.z)
}

/// Returns `true` when `children` are already in nondecreasing `z` order.
///
/// Drawing can iterate the borrowed slice directly in that common case,
/// avoiding a clone and sort of every node's children on every frame.
pub(crate) fn children_are_sorted(scene: &RenderScene, children: &[RenderNodeId]) -> bool {
    children
        .windows(2)
        .all(|pair| child_sort_key(scene, pair[0]) <= child_sort_key(scene, pair[1]))
}
