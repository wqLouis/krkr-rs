//! The generic effect draw the middle layer hands to the renderer.
//!
//! An [`EffectDraw`] is one layer's worth of custom-shader work: a list of
//! [`EffectPass`]es, each a program, the textures it samples, the program's
//! uniform words and the rectangle it covers. The draw is a *value*: the
//! layer tree holds it, the projection turns it into render nodes, and the
//! renderer executes it.
//!
//! The types here name no renderer vocabulary: a pass names the program it
//! runs, and the scene's pipeline table resolves that to the backend's
//! pipeline. The textures are named by the bitmap ids the layer tree already
//! owns, so the projection resolves them through the same lookup it uses for
//! every other bitmap.

use api::shader::EffectUniforms;

use crate::scene::Rect;
use crate::shader::Program;

/// One texture an effect pass samples.
///
/// The pass samples the whole bitmap; the region of it the program reads is
/// expressed in the program's own uniform rectangle, so the renderer never
/// has to know how a program addresses its sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct EffectTexture {
    /// The bitmap the pass samples.
    pub bitmap: u32,
}

impl EffectTexture {
    /// A texture that samples the whole of `bitmap`.
    pub const fn new(bitmap: u32) -> Self {
        Self { bitmap }
    }
}

/// One pass of an effect draw.
///
/// A pass is one program over its own textures and uniforms, covering one
/// rectangle. A draw that needs more than one placement (a scroll places two
/// sources) has one pass per placement.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct EffectPass {
    /// The program the pass is drawn with.
    pub program: Program,
    /// The textures the pass samples, in the program's binding order.
    pub textures: Vec<EffectTexture>,
    /// The program's uniform words.
    pub uniforms: EffectUniforms,
    /// The rectangle the pass covers, in the draw's own pixel space.
    pub dst: Rect,
}

impl EffectPass {
    /// A pass of `program` over `textures`, carrying `uniforms` and covering
    /// `dst`.
    pub fn new(
        program: Program,
        textures: Vec<EffectTexture>,
        uniforms: EffectUniforms,
        dst: Rect,
    ) -> Self {
        Self {
            program,
            textures,
            uniforms,
            dst,
        }
    }
}

/// A whole effect draw: the passes that composite it.
///
/// The draw replaces the layer's own image while it runs. `covers_layer`
/// records whether it stands in for the layer's whole subtree (drawn over the
/// layer's rectangle, holding its children) or for the layer's own image
/// (drawn at the image's placement, with the children live beside it).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct EffectDraw {
    /// The passes, in draw order.
    pub passes: Vec<EffectPass>,
    /// The size of the draw's own pixel space, in pixels.
    ///
    /// The passes' rectangles are in this space, and the projection places
    /// them where the layer's image would have been.
    pub size: (u32, u32),
    /// Whether the draw stands in for the layer's whole subtree.
    pub covers_layer: bool,
}

impl EffectDraw {
    /// A draw of `passes` over a pixel space of `size`.
    pub fn new(passes: Vec<EffectPass>, size: (u32, u32), covers_layer: bool) -> Self {
        Self {
            passes,
            size,
            covers_layer,
        }
    }

    /// The bitmaps the draw's passes sample, in pass order.
    pub fn bitmaps(&self) -> impl Iterator<Item = u32> + '_ {
        self.passes
            .iter()
            .flat_map(|pass| pass.textures.iter())
            .map(|texture| texture.bitmap)
    }
}
