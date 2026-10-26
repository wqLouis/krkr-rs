//! The generic effect draw of a transition kind.
//!
//! This is the only kind-aware code of the custom-shader path: it maps a
//! [`TransKind`] and the transition's current phase to the [`EffectDraw`] the
//! projection draws. A kind this build has no program for returns `None`, and
//! the caller blends that kind on the CPU.
//!
//! The draw is expressed in the transition's own pixel space: the sources are
//! the transition's two images, and a pass's rectangle is where that source
//! lands inside the frame. The projection places the frame where the layer's
//! own image would have been.
//!
//! The builder names the *program* each pass runs, not a pipeline: which
//! pipeline that program is drawn with is the scene's business, and a draw
//! that names a program the scene has no pipeline for is not drawn.

use api::shader::EffectUniforms;

use crate::scene::Rect;
use crate::shader::builtin::{self, Program};
use crate::shader::{EffectDraw, EffectPass, EffectTexture};
use crate::trans::{DestAlpha, Trans, TransKind};

/// The weight the cross-fade blends at, for a phase.
///
/// The phase runs to 255 and the blend divides by 256, so the weight is
/// `phase / 256` — the same fraction the CPU blend's `>> 8` computes.
fn phase_weight(phase: i32) -> f32 {
    phase as f32 / 256.0
}

/// The normalized texture rectangle of `src` inside a bitmap of `size`.
///
/// The rectangle is `(u0, v0, u1, v1)`, the form the programs map their quad
/// coordinates through.
fn normalized_uv(src: Rect, size: (u32, u32)) -> [f32; 4] {
    let width = size.0.max(1) as f32;
    let height = size.1.max(1) as f32;
    [
        src.x as f32 / width,
        src.y as f32 / height,
        (src.x as f32 + src.w as f32) / width,
        (src.y as f32 + src.h as f32) / height,
    ]
}

/// The mix the cross-fade program runs, for the destination's alpha
/// handling.
///
/// The program covers the four channels alike for a destination that ignores
/// its alpha and for one that stores an additive alpha; a destination that
/// stores a straight alpha per pixel gets the mix that covers the source with
/// the share of the destination its two alphas give it.
fn blend_method(dest_alpha: DestAlpha) -> f32 {
    match dest_alpha {
        DestAlpha::Opaque | DestAlpha::Additive => 0.0,
        DestAlpha::Straight => 1.0,
    }
}

/// Builds the uniform block of a pass, or `None` when the block is not
/// usable.
///
/// The layout is the one every shipped program declares: two source
/// rectangles, a parameters `vec4` and the spare `vec4` whose `w` the
/// renderer overwrites with the composed draw opacity.
fn pass_uniforms(
    first: [f32; 4],
    second: [f32; 4],
    weight: f32,
    method: f32,
) -> Option<EffectUniforms> {
    let mut words = [0.0f32; builtin::UNIFORM_WORDS as usize];
    let first_words = builtin::RECT0_UNIFORM as usize;
    let second_words = builtin::RECT1_UNIFORM as usize;
    words[first_words..first_words + 4].copy_from_slice(&first);
    words[second_words..second_words + 4].copy_from_slice(&second);
    words[builtin::PARAM_UNIFORM as usize] = weight;
    words[builtin::METHOD_UNIFORM as usize] = method;
    match EffectUniforms::from_words(&words) {
        Ok(uniforms) => Some(uniforms),
        Err(error) => {
            log::error!("transition effect: the uniform block is not usable: {error}");
            None
        }
    }
}

impl Trans {
    /// The frame `src1`/`src2` composite into at the current phase, drawn
    /// with the generic effect path.
    ///
    /// `size` is the shared source size and the draw's own pixel space.
    /// `covers_layer` records whether the draw stands in for the layer's whole
    /// subtree rather than for its own image.
    ///
    /// `None` when this build has no program for the kind: the caller then
    /// blends the frame on the CPU, which is the engine's own per-pixel
    /// blend. A draw whose program the scene has no pipeline for is not drawn
    /// either, and the caller blends the frame on the CPU as well.
    pub fn effect_draw(
        &self,
        src1: u32,
        src2: u32,
        size: (u32, u32),
        covers_layer: bool,
    ) -> Option<EffectDraw> {
        if size.0 == 0 || size.1 == 0 {
            return None;
        }
        match self.kind {
            TransKind::CrossFade => {
                // The endpoints are exact copies of one source, so the other
                // source draws nothing there. Binding that one source in both
                // slots reproduces the copy at any weight.
                let (first, second) = if self.phase <= 0 {
                    (src1, src1)
                } else if self.phase >= self.phase_max {
                    (src2, src2)
                } else {
                    (src1, src2)
                };
                let full = [0.0, 0.0, 1.0, 1.0];
                let uniforms = pass_uniforms(
                    full,
                    full,
                    phase_weight(self.phase),
                    blend_method(self.dest_alpha),
                )?;
                let pass = EffectPass::new(
                    Program::CrossFade,
                    vec![EffectTexture::new(first), EffectTexture::new(second)],
                    uniforms,
                    Rect::new(0, 0, size.0, size.1),
                );
                Some(EffectDraw::new(vec![pass], size, covers_layer))
            }
            TransKind::Scroll { from, stay } => {
                let (width, height) = (
                    size.0.min(i32::MAX as u32) as i32,
                    size.1.min(i32::MAX as u32) as i32,
                );
                let (first, second) =
                    super::scroll_placements(self.progress(), (width, height), from, stay);
                let frame = (0, 0, width, height);
                let mut passes = Vec::with_capacity(2);
                for (placement, bitmap) in [(first, src1), (second, src2)] {
                    // A source outside the frame contributes no pixel.
                    let Some((dst, src)) = super::scroll_source_rects(frame, placement) else {
                        continue;
                    };
                    let uniforms = pass_uniforms(normalized_uv(src, size), [0.0; 4], 0.0, 0.0)?;
                    passes.push(EffectPass::new(
                        Program::Scroll,
                        vec![EffectTexture::new(bitmap)],
                        uniforms,
                        dst,
                    ));
                }
                if passes.is_empty() {
                    return None;
                }
                Some(EffectDraw::new(passes, size, covers_layer))
            }
            TransKind::Universal { .. }
            | TransKind::Mask
            | TransKind::Wave { .. }
            | TransKind::Mosaic { .. }
            | TransKind::Turn { .. }
            | TransKind::RotateZoom { .. }
            | TransKind::RotateVanish { .. }
            | TransKind::RotateSwap { .. }
            | TransKind::Ripple { .. } => None,
        }
    }
}
