//! Precompiled SPIR-V shader modules.
//!
//! The GLSL sources live next to this crate under `shaders/` and are compiled
//! with `glslc --target-env=vulkan1.0 -O` as part of preparing the source
//! tree. Embedding the resulting `.spv` blobs keeps the crate free of a build
//! dependency on a shader compiler while still shipping real compiled code.
//!
//! Push-constant block layout (shared by every module):
//!
//! | offset | field     | bytes |
//! |--------|-----------|-------|
//! | 0      | `mat4 mvp`| 64    |
//! | 64     | `vec4 tint`| 16   |
//! | 80     | `vec4 uv_rect`| 16 |
//! | 96     | `uint mode`| 4    |
//! | 100    | `float opacity`| 4 |
//! | 104    | `vec2 target_size`| 8 |

/// Vertex stage for the unit quad.
pub(crate) const QUAD_VERT: &[u8] = include_bytes!("../../shaders/quad.vert.spv");
/// Fragment stage that samples the bound texture.
pub(crate) const TEXTURED_FRAG: &[u8] = include_bytes!("../../shaders/textured.frag.spv");
/// Fragment stage that outputs the tint directly.
pub(crate) const SOLID_FRAG: &[u8] = include_bytes!("../../shaders/solid.frag.spv");
/// Fragment stage that blends a source over a copy of the destination.
pub(crate) const DST_BLEND_FRAG: &[u8] = include_bytes!("../../shaders/dst_blend.frag.spv");

/// Converts a little-endian SPIR-V byte blob into 32-bit words.
pub(crate) fn to_words(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| u32::from_le_bytes(*chunk))
        .collect()
}
