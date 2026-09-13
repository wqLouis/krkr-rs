//! Real GPU blend modes for TVP layers (TODO Stage 4).
//!
//! # Mapping (native `Layer.type` → GPU blend)
//!
//! TVP bitmaps are **straight-alpha** RGBA8, so every fixed-function blend
//! below uses straight-alpha factors (`SrcAlpha` scales the source):
//!
//! | native type                          | value(s)     | GPU blend                                  |
//! |--------------------------------------|--------------|--------------------------------------------|
//! | `ltOpaque`                           | 1            | replace (no blending)¹                     |
//! | `ltAlpha`/`ltTransparent`            | 2            | source-over: `src*a + dst*(1-a)`           |
//! | `ltAdditive`                         | 3            | add: `src*a + dst`                         |
//! | `ltSubtractive`                      | 4            | reverse-subtract: `dst - src*a`            |
//! | `ltPsNormal`                         | 13           | source-over                                |
//! | `ltPsAdditive`                       | 14           | add                                        |
//! | `ltPsSubtractive`                    | 15           | reverse-subtract                           |
//! | `ltAddAlpha`                         | 12           | source-over (alpha channel = added amount) |
//! | anything else (mult/screen/PS …)     | 5–11, 16–28  | falls back to source-over²                 |
//!
//! ¹ A fully-opaque source-over blit writes exactly the source color, so
//! `Replace` (blending disabled) is equivalent *and* cheaper. If window ×
//! layer opacity drops below 1.0 the layer silently degrades to a plain
//! source-over [`Sprite`] so the opacity still shows (see
//! [`render_path_for`]).
//!
//! ² Multiplicative/screen/Photoshop modes are not expressible with a single
//! fixed-function pass; they need shader math and fall back to source-over
//! rather than rendering something wrong.
//!
//! # How it renders
//!
//! [`LayerBlendMode::SourceOver`] layers stay on the built-in `Sprite`
//! pipeline (straight-alpha, unchanged from the previous milestone). All
//! other modes spawn a [`bevy::sprite_render::Mesh2d`] quad with
//! [`LayerBlendMaterial`], a custom `Material2d` whose specialization key
//! ([`Material2d::Data`] = the blend mode) selects the pipeline variant with
//! the matching fixed-function `BlendState`. Materials always report
//! `AlphaMode2d::Blend`, so blended quads land in the same z-sorted
//! transparent 2D phase as sprites and interleave correctly by depth.

use bevy::app::{App, Plugin};
use bevy::asset::{Asset, AssetPlugin, Assets, Handle, embedded_asset};
use bevy::color::LinearRgba;
use bevy::image::Image;
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::reflect::TypePath;
use bevy::render::render_resource::{
    AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState, RenderPipelineDescriptor,
    SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;
use bevy::sprite_render::{AlphaMode2d, Material2d, Material2dKey, Material2dPlugin};

use crate::sync::clamp_opacity;

// Native type values not exposed as consts by tvp-visual (drawable.h):
/// `ltAddAlpha` — additive with an explicit alpha channel.
pub const LT_ADD_ALPHA: i64 = 12;
/// `ltPsNormal` — Photoshop-style normal (plain source-over).
pub const LT_PS_NORMAL: i64 = 13;
/// `ltPsAdditive`.
pub const LT_PS_ADDITIVE: i64 = 14;
/// `ltPsSubtractive`.
pub const LT_PS_SUBTRACTIVE: i64 = 15;

/// The GPU compositing operation a layer needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayerBlendMode {
    /// Straight-alpha source-over: `src*a + dst*(1-a)` — the built-in
    /// Sprite pipeline's blending.
    SourceOver,
    /// Destination replaced by the source (blending disabled) — `ltOpaque`.
    Replace,
    /// Additive: `dst + src*a`.
    Additive,
    /// Reverse-subtract: `dst - src*a`.
    Subtractive,
}

/// Map a native TVP `Layer.type` value to the GPU blend mode it needs.
/// Unknown/unimplemented types (multiplicative, screen, PS modes beyond
/// normal/add/sub, …) degrade to [`LayerBlendMode::SourceOver`].
pub fn blend_mode_for(blend_type: i64) -> LayerBlendMode {
    match blend_type {
        tvp_visual::scene::LT_OPAQUE => LayerBlendMode::Replace,
        tvp_visual::scene::LT_ADDITIVE | LT_PS_ADDITIVE => LayerBlendMode::Additive,
        tvp_visual::scene::LT_SUBTRACTIVE | LT_PS_SUBTRACTIVE => LayerBlendMode::Subtractive,
        // ltAlpha=2, ltTransparent=2, ltAddAlpha=12, ltPsNormal=13 and every
        // unimplemented mode all composite as plain source-over.
        _ => LayerBlendMode::SourceOver,
    }
}

/// Fixed-function `BlendState` for a mode, or `None` for
/// [`LayerBlendMode::Replace`] (blending disabled). Straight-alpha factors
/// throughout: TVP bitmaps are straight alpha, so the source contribution is
/// always scaled by `SrcAlpha` (which also carries window × layer opacity).
pub const fn blend_state_for(mode: LayerBlendMode) -> Option<BlendState> {
    match mode {
        LayerBlendMode::Replace => None,
        LayerBlendMode::SourceOver => Some(BlendState {
            color: BlendComponent {
                src_factor: BlendFactor::SrcAlpha,
                dst_factor: BlendFactor::OneMinusSrcAlpha,
                operation: BlendOperation::Add,
            },
            alpha: BlendComponent {
                src_factor: BlendFactor::One,
                dst_factor: BlendFactor::OneMinusSrcAlpha,
                operation: BlendOperation::Add,
            },
        }),
        LayerBlendMode::Additive => Some(BlendState {
            color: BlendComponent {
                src_factor: BlendFactor::SrcAlpha,
                dst_factor: BlendFactor::One,
                operation: BlendOperation::Add,
            },
            alpha: BlendComponent {
                src_factor: BlendFactor::One,
                dst_factor: BlendFactor::One,
                operation: BlendOperation::Add,
            },
        }),
        LayerBlendMode::Subtractive => Some(BlendState {
            color: BlendComponent {
                src_factor: BlendFactor::SrcAlpha,
                dst_factor: BlendFactor::One,
                operation: BlendOperation::ReverseSubtract,
            },
            alpha: BlendComponent {
                src_factor: BlendFactor::One,
                dst_factor: BlendFactor::One,
                operation: BlendOperation::ReverseSubtract,
            },
        }),
    }
}

/// Which render path the sync should use for a layer: the built-in
/// [`Sprite`](bevy::prelude::Sprite) pipeline or a custom-material quad.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerRenderPath {
    /// Built-in Sprite (straight-alpha source-over).
    Sprite,
    /// Custom [`LayerBlendMaterial`] quad with this GPU blend mode.
    Material(LayerBlendMode),
}

/// Resolve the render path from the native blend type and the layer's
/// composed opacity. An `ltOpaque` layer only takes the replace path at
/// full opacity — below that it must still blend, so it stays on a Sprite.
pub fn render_path_for(blend_type: i64, composed_alpha: f32) -> LayerRenderPath {
    match blend_mode_for(blend_type) {
        LayerBlendMode::SourceOver => LayerRenderPath::Sprite,
        mode @ (LayerBlendMode::Additive | LayerBlendMode::Subtractive) => {
            LayerRenderPath::Material(mode)
        }
        LayerBlendMode::Replace => {
            if clamp_opacity(composed_alpha) >= 1.0 {
                LayerRenderPath::Material(LayerBlendMode::Replace)
            } else {
                LayerRenderPath::Sprite
            }
        }
    }
}

/// Fragment shader for [`LayerBlendMaterial`] (embedded below). The
/// `embedded://` path is `<lib-name>/<path-after-src>`; this crate's lib is
/// `krkr_render`, so the path is `krkr_render/...` (not the package name).
const LAYER_BLEND_SHADER_PATH: &str = "embedded://krkr_render/layer_blend_material.wgsl";

/// Material for layers that need a non-default GPU blend: samples the
/// layer texture (straight alpha), multiplies the tint (which carries the
/// composed window × layer opacity), and lets the pipeline variant's
/// fixed-function `BlendState` do the compositing.
///
/// The specialization key ([`Material2d::Data`]) is the blend mode, so Bevy
/// compiles one pipeline variant per mode and entities pick their variant
/// through the material asset alone.
#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
#[bind_group_data(LayerBlendMode)]
pub struct LayerBlendMaterial {
    /// Straight-alpha tint × opacity (linear space).
    #[uniform(0)]
    color: LinearRgba,
    /// The GPU blend this instance composites with (not a shader binding;
    /// it keys the pipeline specialization).
    mode: LayerBlendMode,
    /// The layer's texture. Fill-only layers bind a shared 1×1 white
    /// texture so the tint alone drives the color.
    #[texture(1)]
    #[sampler(2)]
    color_texture: Option<Handle<Image>>,
}

impl LayerBlendMaterial {
    pub fn new(color: LinearRgba, color_texture: Handle<Image>, mode: LayerBlendMode) -> Self {
        Self {
            color,
            mode,
            color_texture: Some(color_texture),
        }
    }

    /// The bound layer texture, if any.
    pub fn texture(&self) -> Option<&Handle<Image>> {
        self.color_texture.as_ref()
    }
}

// Consumed by the `#[bind_group_data(LayerBlendMode)]` derive, which emits
// `fn bind_group_data(&self) -> Self::Data { self.into() }`.
impl From<&LayerBlendMaterial> for LayerBlendMode {
    fn from(material: &LayerBlendMaterial) -> Self {
        material.mode
    }
}

impl Material2d for LayerBlendMaterial {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Path(LAYER_BLEND_SHADER_PATH.into())
    }

    // Always transparent-phase: blended quads z-sort together with sprites,
    // so mixed sprite/material layer stacks keep their back-to-front order.
    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Blend
    }

    fn specialize(
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        key: Material2dKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        if let Some(fragment) = descriptor.fragment.as_mut()
            && let Some(Some(target)) = fragment.targets.first_mut()
        {
            target.blend = blend_state_for(key.bind_group_data);
        }
        Ok(())
    }
}

/// Registers the embedded shader and the [`LayerBlendMaterial`] pipeline.
/// Headless apps (no `AssetPlugin`) skip the shader/pipeline registration —
/// there is no renderer to load either — and only get the in-memory
/// `Assets<LayerBlendMaterial>` store the sync writes into.
pub struct LayerBlendPlugin;

impl Plugin for LayerBlendPlugin {
    fn build(&self, app: &mut App) {
        if app.is_plugin_added::<AssetPlugin>() {
            embedded_asset!(app, "layer_blend_material.wgsl");
            app.add_plugins(Material2dPlugin::<LayerBlendMaterial>::default());
        } else {
            // Headless (MinimalPlugins, no asset pipeline): the sync still
            // creates material assets for blended layers; give it the store.
            app.insert_resource(Assets::<LayerBlendMaterial>::default());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every documented native type maps to the right GPU blend.
    #[test]
    fn blend_mode_mapping() {
        use LayerBlendMode::*;
        assert_eq!(blend_mode_for(tvp_visual::scene::LT_OPAQUE), Replace);
        assert_eq!(blend_mode_for(tvp_visual::scene::LT_ALPHA), SourceOver);
        assert_eq!(
            blend_mode_for(tvp_visual::scene::LT_ADDITIVE),
            Additive,
            "ltAdditive=3 must become a real GPU add"
        );
        assert_eq!(
            blend_mode_for(tvp_visual::scene::LT_SUBTRACTIVE),
            Subtractive,
            "ltSubtractive=4 must become a real GPU reverse-subtract"
        );
        assert_eq!(blend_mode_for(LT_ADD_ALPHA), SourceOver);
        assert_eq!(blend_mode_for(LT_PS_NORMAL), SourceOver);
        assert_eq!(blend_mode_for(LT_PS_ADDITIVE), Additive);
        assert_eq!(blend_mode_for(LT_PS_SUBTRACTIVE), Subtractive);
        // Unimplemented / exotic modes fall back to source-over, never to a
        // wrong blend.
        for fallback in [0, 5, 6, 7, 8, 9, 10, 11, 16, 17, 18, 20, 22, 25, 28] {
            assert_eq!(blend_mode_for(fallback), SourceOver, "type {fallback}");
        }
        // Garbage values too.
        assert_eq!(blend_mode_for(-1), SourceOver);
        assert_eq!(blend_mode_for(9999), SourceOver);
    }

    /// The fixed-function blend states match the documented equations.
    #[test]
    fn blend_states_match_equations() {
        // Replace disables blending entirely.
        assert_eq!(blend_state_for(LayerBlendMode::Replace), None);

        let over = blend_state_for(LayerBlendMode::SourceOver).unwrap();
        assert_eq!(over.color.src_factor, BlendFactor::SrcAlpha);
        assert_eq!(over.color.dst_factor, BlendFactor::OneMinusSrcAlpha);
        assert_eq!(over.color.operation, BlendOperation::Add);

        let add = blend_state_for(LayerBlendMode::Additive).unwrap();
        assert_eq!(add.color.src_factor, BlendFactor::SrcAlpha);
        assert_eq!(add.color.dst_factor, BlendFactor::One);
        assert_eq!(add.color.operation, BlendOperation::Add);

        // Reverse-subtract computes dst - src (wgpu subtract order).
        let sub = blend_state_for(LayerBlendMode::Subtractive).unwrap();
        assert_eq!(sub.color.src_factor, BlendFactor::SrcAlpha);
        assert_eq!(sub.color.dst_factor, BlendFactor::One);
        assert_eq!(sub.color.operation, BlendOperation::ReverseSubtract);
    }

    /// Render-path routing: opaque degrades to Sprite under partial
    /// opacity; additive/subtractive always take the material path.
    #[test]
    fn render_path_routing() {
        use LayerBlendMode as M;
        use LayerRenderPath::*;

        // ltOpaque at full opacity → replace material.
        assert_eq!(
            render_path_for(tvp_visual::scene::LT_OPAQUE, 1.0),
            Material(M::Replace)
        );
        // …but partial opacity must still blend → plain sprite.
        assert_eq!(
            render_path_for(tvp_visual::scene::LT_OPAQUE, 0.5),
            Sprite,
            "opaque layer under partial opacity needs source-over"
        );
        // Out-of-range alphas are clamped before the decision.
        assert_eq!(
            render_path_for(tvp_visual::scene::LT_OPAQUE, f32::NAN),
            Sprite
        );
        assert_eq!(
            render_path_for(tvp_visual::scene::LT_OPAQUE, 3.0),
            Material(M::Replace)
        );

        // Additive/subtractive keep their GPU blend at any opacity (the
        // SrcAlpha factor scales the contribution).
        assert_eq!(
            render_path_for(tvp_visual::scene::LT_ADDITIVE, 0.25),
            Material(M::Additive)
        );
        assert_eq!(
            render_path_for(tvp_visual::scene::LT_SUBTRACTIVE, 1.0),
            Material(M::Subtractive)
        );

        // Everything else stays on the untouched Sprite pipeline.
        for t in [tvp_visual::scene::LT_ALPHA, LT_ADD_ALPHA, LT_PS_NORMAL, 11] {
            assert_eq!(render_path_for(t, 0.7), Sprite, "type {t}");
        }
    }

    /// The material reports its mode back as the specialization data (this
    /// is the `From<&LayerBlendMaterial>` impl the derive consumes).
    #[test]
    fn material_binds_group_data_to_mode() {
        let tex = Handle::default();
        let mat = LayerBlendMaterial::new(LinearRgba::WHITE, tex.clone(), LayerBlendMode::Additive);
        assert_eq!(LayerBlendMode::from(&mat), LayerBlendMode::Additive);
        // Texture is kept for binding.
        assert_eq!(mat.color_texture.as_ref(), Some(&tex));
    }
}
