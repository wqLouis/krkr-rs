//! The engine's own effect programs, compiled from `crates/engine/shaders`.
//!
//! Each program is a pair of SPIR-V stages plus the descriptor it must be drawn
//! with: the texture bindings, the uniform word count and the word the renderer
//! overwrites with the composed draw opacity. The `.glsl` source next to each
//! `.spv` documents the block layout; the constants below are that layout's
//! contract with every program here.
//!
//! The programs are data, not a code path: the kind-aware code names the
//! [`Program`] it wants, and the projection turns the resulting draw into
//! scene nodes.

use api::shader::{BindingDesc, SamplerKind};

use crate::shader::ProgramDesc;

/// The programs the engine ships.
///
/// A program is one shader pair plus the descriptor it must be drawn with.
/// The kind-aware code names the program it wants; the pipeline table resolves
/// that name to a pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Program {
    /// The cross-fade program (`crossfade.frag`).
    CrossFade,
    /// The scroll program (`scroll.frag`).
    Scroll,
}

impl Program {
    /// The descriptor the program is registered with.
    pub fn desc(self) -> ProgramDesc<'static> {
        match self {
            Self::CrossFade => crossfade(),
            Self::Scroll => scroll(),
        }
    }

    /// The program's diagnostic name.
    pub fn name(self) -> &'static str {
        match self {
            Self::CrossFade => "crossfade",
            Self::Scroll => "scroll",
        }
    }
}

/// Shared vertex stage: the unit quad through the per-draw `mvp`.
pub const VERTEX: &[u8] = include_bytes!("../../shaders/effect.vert.spv");
/// Cross-fade fragment stage (`crossfade.frag`).
pub const CROSSFADE_FRAGMENT: &[u8] = include_bytes!("../../shaders/crossfade.frag.spv");
/// Scroll fragment stage (`scroll.frag`).
pub const SCROLL_FRAGMENT: &[u8] = include_bytes!("../../shaders/scroll.frag.spv");

/// Uniform words every effect program's block declares.
///
/// Four `vec4`s: two texture rectangles, one parameters `vec4` and one spare
/// `vec4` whose `w` carries the composed opacity.
pub const UNIFORM_WORDS: u32 = 16;
/// Word the renderer overwrites with the frame's composed draw opacity.
pub const OPACITY_UNIFORM: u32 = 15;
/// Word a program reads its own per-frame parameter from (the phase weight for
/// the cross-fade).
pub const PARAM_UNIFORM: u32 = 8;
/// Word the first source's texture rectangle starts at.
///
/// The rectangle is `(u0, v0, u1, v1)` in normalized texture coordinates: the
/// program maps the quad's own coordinates through it, so a pass can sample a
/// sub-rectangle of its bitmap without the renderer knowing how the program
/// addresses its sources.
pub const RECT0_UNIFORM: u32 = 0;
/// Word the second source's texture rectangle starts at.
pub const RECT1_UNIFORM: u32 = 4;

/// Bindings of the cross-fade: the two sources, both sampled bilinearly.
pub const CROSSFADE_BINDINGS: [BindingDesc; 2] = [
    BindingDesc::texture(1, SamplerKind::LinearClamp),
    BindingDesc::texture(2, SamplerKind::LinearClamp),
];

/// Bindings of a scroll pass: the one source it places.
pub const SCROLL_BINDINGS: [BindingDesc; 1] = [BindingDesc::texture(1, SamplerKind::LinearClamp)];

/// The cross-fade program.
///
/// Source 1 is bound at 1 and source 2 at 2; the fragment stage lerps them per
/// channel at [`PARAM_UNIFORM`] and scales the alpha by [`OPACITY_UNIFORM`].
pub fn crossfade() -> ProgramDesc<'static> {
    let mut desc = ProgramDesc::new("crossfade", VERTEX, CROSSFADE_FRAGMENT);
    desc.bindings = &CROSSFADE_BINDINGS;
    desc.uniform_words = UNIFORM_WORDS;
    desc.opacity_uniform = Some(OPACITY_UNIFORM);
    desc
}

/// The scroll program: one source, placed by the draw's own geometry.
pub fn scroll() -> ProgramDesc<'static> {
    let mut desc = ProgramDesc::new("scroll", VERTEX, SCROLL_FRAGMENT);
    desc.bindings = &SCROLL_BINDINGS;
    desc.uniform_words = UNIFORM_WORDS;
    desc.opacity_uniform = Some(OPACITY_UNIFORM);
    desc
}
