//! Real GPU blend modes for TVP layers (TODO Stage 4).
//!
//! # Mapping (native `Layer.type` → GPU blend)
//!
//! TVP bitmaps are **straight-alpha** RGBA8. The native `tTVPLayerType` enum
//! (`reference/cpp/core/visual/drawable.h:20`) has 29 modes; this module maps
//! all of them and renders as many as a single fixed-function pass can
//! express. See [`LayerBlendMode`] for the per-mode status.
//!
//! | native type                          | value(s)     | GPU blend                                            |
//! |--------------------------------------|--------------|------------------------------------------------------|
//! | `ltBinder`                           | 0            | **drawn nothing** (container, reference `Blt`: "no action") |
//! | `ltOpaque`/`ltCoverRect`             | 1            | replace (no blending)¹                               |
//! | `ltAlpha`/`ltTransparent`            | 2            | source-over                                          |
//! | `ltAdditive`                         | 3            | add: `src*a + dst`                                   |
//! | `ltSubtractive`                      | 4            | reverse-subtract: `dst - src*a`                      |
//! | `ltMultiplicative`                   | 5            | multiply: `src*a*dst + dst*(1-a)`                    |
//! | `ltDodge`                            | 8            | dst-dependent (see below)                            |
//! | `ltDarken`                           | 9            | dst-dependent                                        |
//! | `ltLighten`                          | 10           | dst-dependent                                        |
//! | `ltScreen`                           | 11           | screen: `src*a*(1-dst) + dst`                        |
//! | `ltAddAlpha`                         | 12           | additive-alpha: `src + dst*(1-a)`                    |
//! | `ltPsNormal`                         | 13           | source-over                                          |
//! | `ltPsAdditive`                       | 14           | add                                                  |
//! | `ltPsSubtractive`                    | 15           | reverse-subtract                                     |
//! | `ltPsMultiplicative`                 | 16           | multiply                                             |
//! | `ltPsScreen`                         | 17           | screen                                               |
//! | `ltPsOverlay` … `ltPsExclusion`      | 18–28        | dst-dependent (see below)                            |
//!
//! ¹ A fully-opaque source-over blit writes exactly the source color, so
//! `Replace` (blending disabled) is equivalent *and* cheaper. If window ×
//! layer opacity drops below 1.0 the layer degrades to a plain source-over
//! [`Sprite`] so the opacity still shows (see [`render_path_for`]).
//!
//! # Multiplicative and screen, without a destination read
//!
//! Multiply and screen are the only Photoshop-style modes expressible with
//! wgpu's fixed-function blend factors plus a pre-scaled source:
//!
//! * multiply: `out = (src·a)·dst + dst·(1-a)` — source factor `Dst`,
//!   destination factor `OneMinusSrcAlpha`.
//! * screen: `out = (src·a)·(1-dst) + dst` — source factor `OneMinusDst`,
//!   destination factor `One`.
//!
//! For these two the material's tint RGB is pre-multiplied by the composed
//! alpha in [`crate::sync`], so the colour the fragment shader emits already
//! carries the `·a` (and `·texture.a` for bitmap layers).
//!
//! # Dest-dependent modes
//!
//! Overlay, hard/soft light, colour dodge/burn, lighten/darken, difference
//! and exclusion compute a non-linear function of **both** the source and the
//! destination (`reference/cpp/core/visual/tvpgl.cpp:12575-12720`). A single
//! fragment shader cannot sample the render target it is writing to, so these
//! need a destination-read compositor (render each layer to an offscreen
//! target and blend with a ping-pong pass) — a render-architecture change,
//! not a per-material blend state. They currently fall back to source-over
//! and emit a **one-shot warning** ([`warn_dst_dependent_once`]) so the
//! degradation is never silent. None of the title's scenario tags exercise
//! them (verified against `data.xp3`+`patch.xp3`), but the fallback is
//! recorded so a future pass can pick it up.
//!
//! # How it renders
//!
//! [`LayerBlendMode::SourceOver`] layers stay on the built-in `Sprite`
//! pipeline. All other drawable modes spawn a
//! [`bevy::sprite_render::Mesh2d`] quad with [`LayerBlendMaterial`], a custom
//! `Material2d` whose specialization key ([`Material2d::Data`] = the blend
//! mode) selects the pipeline variant with the matching fixed-function
//! `BlendState`. Materials always report `AlphaMode2d::Blend`, so blended
//! quads land in the same z-sorted transparent 2D phase as sprites and
//! interleave correctly by depth.

use std::collections::HashSet;
use std::sync::Mutex;

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

// Native `tTVPLayerType` values (`reference/cpp/core/visual/drawable.h:20`).
// `tvp-visual` only exports the four originals, so the full set lives here.
/// `ltBinder` — container that draws nothing.
pub const LT_BINDER: i64 = 0;
/// `ltOpaque`/`ltCoverRect`.
pub const LT_OPAQUE: i64 = 1;
/// `ltAlpha`/`ltTransparent`.
pub const LT_ALPHA: i64 = 2;
/// `ltAdditive`.
pub const LT_ADDITIVE: i64 = 3;
/// `ltSubtractive`.
pub const LT_SUBTRACTIVE: i64 = 4;
/// `ltMultiplicative`.
pub const LT_MULTIPLICATIVE: i64 = 5;
/// `ltEffect` (layer-effect callback, not a bitmap blend).
pub const LT_EFFECT: i64 = 6;
/// `ltFilter` (layer-filter callback, not a bitmap blend).
pub const LT_FILTER: i64 = 7;
/// `ltDodge` (colour dodge).
pub const LT_DODGE: i64 = 8;
/// `ltDarken`.
pub const LT_DARKEN: i64 = 9;
/// `ltLighten`.
pub const LT_LIGHTEN: i64 = 10;
/// `ltScreen`.
pub const LT_SCREEN: i64 = 11;
/// `ltAddAlpha` — additive with an explicit alpha channel.
pub const LT_ADD_ALPHA: i64 = 12;
/// `ltPsNormal` — Photoshop-style normal (plain source-over).
pub const LT_PS_NORMAL: i64 = 13;
/// `ltPsAdditive`.
pub const LT_PS_ADDITIVE: i64 = 14;
/// `ltPsSubtractive`.
pub const LT_PS_SUBTRACTIVE: i64 = 15;
/// `ltPsMultiplicative`.
pub const LT_PS_MULTIPLICATIVE: i64 = 16;
/// `ltPsScreen`.
pub const LT_PS_SCREEN: i64 = 17;
/// `ltPsOverlay`.
pub const LT_PS_OVERLAY: i64 = 18;
/// `ltPsHardLight`.
pub const LT_PS_HARD_LIGHT: i64 = 19;
/// `ltPsSoftLight`.
pub const LT_PS_SOFT_LIGHT: i64 = 20;
/// `ltPsColorDodge`.
pub const LT_PS_COLOR_DODGE: i64 = 21;
/// `ltPsColorDodge5`.
pub const LT_PS_COLOR_DODGE5: i64 = 22;
/// `ltPsColorBurn`.
pub const LT_PS_COLOR_BURN: i64 = 23;
/// `ltPsLighten`.
pub const LT_PS_LIGHTEN: i64 = 24;
/// `ltPsDarken`.
pub const LT_PS_DARKEN: i64 = 25;
/// `ltPsDifference`.
pub const LT_PS_DIFFERENCE: i64 = 26;
/// `ltPsDifference5`.
pub const LT_PS_DIFFERENCE5: i64 = 27;
/// `ltPsExclusion`.
pub const LT_PS_EXCLUSION: i64 = 28;

/// The GPU compositing operation a layer needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayerBlendMode {
    /// Straight-alpha source-over: `src*a + dst*(1-a)` — the built-in
    /// `Sprite` pipeline's blending. Covers `ltAlpha`/`ltPsNormal`.
    SourceOver,
    /// Destination replaced by the source (blending disabled) — `ltOpaque`.
    Replace,
    /// Additive: `dst + src*a` — `ltAdditive`/`ltPsAdditive`.
    Additive,
    /// Reverse-subtract: `dst - src*a` — `ltSubtractive`/`ltPsSubtractive`.
    Subtractive,
    /// Additive-alpha: `src + dst*(1-a)` — `ltAddAlpha`.
    AddAlpha,
    /// Multiply: `(src*a)*dst + dst*(1-a)` — `ltMultiplicative`/`ltPsMultiplicative`.
    Multiply,
    /// Screen: `(src*a)*(1-dst) + dst` — `ltScreen`/`ltPsScreen`.
    Screen,
    /// `ltBinder` — a container; the reference `Blt` returns "no action", so
    /// the layer draws nothing (its children are separate scene layers).
    Binder,
    /// `ltDodge`/`ltPsColorDodge` — destination-dependent.
    ColorDodge,
    /// `ltPsColorDodge5` — destination-dependent.
    ColorDodge5,
    /// `ltPsColorBurn` — destination-dependent.
    ColorBurn,
    /// `ltDarken`/`ltPsDarken` — destination-dependent.
    Darken,
    /// `ltLighten`/`ltPsLighten` — destination-dependent.
    Lighten,
    /// `ltPsDifference` — destination-dependent.
    Difference,
    /// `ltPsDifference5` — destination-dependent.
    Difference5,
    /// `ltPsExclusion` — destination-dependent.
    Exclusion,
    /// `ltPsOverlay` — destination-dependent.
    Overlay,
    /// `ltPsHardLight` — destination-dependent.
    HardLight,
    /// `ltPsSoftLight` — destination-dependent.
    SoftLight,
}

impl LayerBlendMode {
    /// True when the mode composites onto the destination with a single
    /// fixed-function [`BlendState`] (plus, for multiply/screen, a
    /// pre-scaled source).
    pub const fn is_fixed_function(self) -> bool {
        matches!(
            self,
            Self::SourceOver
                | Self::Replace
                | Self::Additive
                | Self::Subtractive
                | Self::AddAlpha
                | Self::Multiply
                | Self::Screen
        )
    }

    /// True when the blend function needs to read the destination colour and
    /// therefore cannot be expressed by a single-pass material (see the
    /// module docs). These fall back to source-over with a warning.
    pub const fn is_dst_dependent(self) -> bool {
        !self.is_fixed_function() && !matches!(self, Self::Binder)
    }

    /// True when the fragment shader's RGB output must be pre-multiplied by
    /// the composed alpha before the fixed-function `Dst`/`OneMinusDst`
    /// factors are applied (multiply/screen).
    pub const fn is_premultiplied_source(self) -> bool {
        matches!(self, Self::Multiply | Self::Screen)
    }
}

/// Map a native TVP `Layer.type` value to the GPU blend mode it needs. This
/// is total over the documented `tTVPLayerType` range: every known type maps
/// to its own mode (never silently to source-over). Only truly unknown /
/// out-of-range values degrade to [`LayerBlendMode::SourceOver`].
pub const fn blend_mode_for(blend_type: i64) -> LayerBlendMode {
    use LayerBlendMode as M;
    match blend_type {
        LT_BINDER => M::Binder,
        LT_OPAQUE => M::Replace,
        LT_ALPHA | LT_PS_NORMAL => M::SourceOver,
        LT_ADDITIVE | LT_PS_ADDITIVE => M::Additive,
        LT_SUBTRACTIVE | LT_PS_SUBTRACTIVE => M::Subtractive,
        LT_ADD_ALPHA => M::AddAlpha,
        LT_MULTIPLICATIVE | LT_PS_MULTIPLICATIVE => M::Multiply,
        LT_SCREEN | LT_PS_SCREEN => M::Screen,
        // `ltDodge` maps to the reference `bmDodge` (`TVPColorDodgeBlend`),
        // the same colour-dodge math as `ltPsColorDodge`.
        LT_DODGE | LT_PS_COLOR_DODGE => M::ColorDodge,
        LT_PS_COLOR_DODGE5 => M::ColorDodge5,
        LT_PS_COLOR_BURN => M::ColorBurn,
        LT_DARKEN | LT_PS_DARKEN => M::Darken,
        LT_LIGHTEN | LT_PS_LIGHTEN => M::Lighten,
        LT_PS_DIFFERENCE => M::Difference,
        LT_PS_DIFFERENCE5 => M::Difference5,
        LT_PS_EXCLUSION => M::Exclusion,
        LT_PS_OVERLAY => M::Overlay,
        LT_PS_HARD_LIGHT => M::HardLight,
        LT_PS_SOFT_LIGHT => M::SoftLight,
        // `ltEffect`/`ltFilter` are layer-effect/filter callbacks in the
        // reference, not bitmap blend functions; drawing them source-over
        // matches how a plain `Layer.drawText`/`draw*` result is composited.
        LT_EFFECT | LT_FILTER => M::SourceOver,
        _ => M::SourceOver,
    }
}

/// Straight-alpha source-over, the fallback for modes we cannot express.
const SOURCE_OVER_STATE: BlendState = BlendState {
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
};

/// Fixed-function `BlendState` for a mode, or `None` for
/// [`LayerBlendMode::Replace`] (blending disabled) and
/// [`LayerBlendMode::Binder`] (drawn nothing).
///
/// Straight-alpha factors throughout: TVP bitmaps are straight alpha, so the
/// source contribution is always scaled by `SrcAlpha` (which carries the
/// composed window × layer opacity) unless the mode's equation says
/// otherwise (add-alpha, multiply, screen).
///
/// Destination-dependent modes return the source-over state: the renderer
/// cannot express them in one pass, and [`render_path_for`] already routed
/// them to the plain sprite pipeline; returning source-over here keeps the
/// function total for callers/tests.
pub const fn blend_state_for(mode: LayerBlendMode) -> Option<BlendState> {
    use LayerBlendMode as M;
    match mode {
        M::Binder => None,
        M::Replace => None,
        M::SourceOver => Some(SOURCE_OVER_STATE),
        M::Additive => Some(BlendState {
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
        M::Subtractive => Some(BlendState {
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
        // `src + dst*(1-a)` (reference `TVPAddAlphaBlend_n_a`).
        M::AddAlpha => Some(BlendState {
            color: BlendComponent {
                src_factor: BlendFactor::One,
                dst_factor: BlendFactor::OneMinusSrcAlpha,
                operation: BlendOperation::Add,
            },
            alpha: BlendComponent {
                src_factor: BlendFactor::One,
                dst_factor: BlendFactor::OneMinusSrcAlpha,
                operation: BlendOperation::Add,
            },
        }),
        // `(src*a)*dst + dst*(1-a)`; the shader output RGB is pre-scaled by
        // `a` in `sync` (see `is_premultiplied_source`).
        M::Multiply => Some(BlendState {
            color: BlendComponent {
                src_factor: BlendFactor::Dst,
                dst_factor: BlendFactor::OneMinusSrcAlpha,
                operation: BlendOperation::Add,
            },
            alpha: BlendComponent {
                src_factor: BlendFactor::One,
                dst_factor: BlendFactor::OneMinusSrcAlpha,
                operation: BlendOperation::Add,
            },
        }),
        // `(src*a)*(1-dst) + dst`.
        M::Screen => Some(BlendState {
            color: BlendComponent {
                src_factor: BlendFactor::OneMinusDst,
                dst_factor: BlendFactor::One,
                operation: BlendOperation::Add,
            },
            alpha: BlendComponent {
                src_factor: BlendFactor::One,
                dst_factor: BlendFactor::OneMinusSrcAlpha,
                operation: BlendOperation::Add,
            },
        }),
        // Destination-dependent: not expressible in one pass, route to the
        // source-over equation (never a *wrong* blend).
        M::ColorDodge
        | M::ColorDodge5
        | M::ColorBurn
        | M::Darken
        | M::Lighten
        | M::Difference
        | M::Difference5
        | M::Exclusion
        | M::Overlay
        | M::HardLight
        | M::SoftLight => Some(SOURCE_OVER_STATE),
    }
}

/// Warn exactly once per destination-dependent mode that it is rendering as
/// source-over. Keeps the degradation discoverable instead of silent.
fn warn_dst_dependent_once(mode: LayerBlendMode) {
    static WARNED: Mutex<Option<HashSet<LayerBlendMode>>> = Mutex::new(None);
    let mut guard = WARNED.lock().unwrap_or_else(|p| p.into_inner());
    let warned = guard.get_or_insert_with(HashSet::new);
    if warned.insert(mode) {
        log::warn!(
            "layer blend mode {mode:?} needs a destination-read compositor; \
             rendering it source-over (see render::blend docs)"
        );
    }
}

/// Which render path the sync should use for a layer: the built-in
/// [`Sprite`](bevy::prelude::Sprite) pipeline, a custom-material quad, or
/// nothing at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerRenderPath {
    /// Built-in Sprite (straight-alpha source-over).
    Sprite,
    /// Custom [`LayerBlendMaterial`] quad with this GPU blend mode.
    Material(LayerBlendMode),
    /// Draw nothing (`ltBinder`).
    Skip,
}

/// Resolve the render path from the native blend type and the layer's
/// composed opacity. An `ltOpaque` layer only takes the replace path at
/// full opacity — below that it must still blend, so it stays on a Sprite.
/// `ltBinder` is skipped entirely. Destination-dependent modes fall back to
/// the source-over Sprite with a one-shot warning.
pub fn render_path_for(blend_type: i64, composed_alpha: f32) -> LayerRenderPath {
    let mode = blend_mode_for(blend_type);
    match mode {
        LayerBlendMode::Binder => LayerRenderPath::Skip,
        LayerBlendMode::SourceOver => LayerRenderPath::Sprite,
        LayerBlendMode::Replace => {
            if clamp_opacity(composed_alpha) >= 1.0 {
                LayerRenderPath::Material(LayerBlendMode::Replace)
            } else {
                LayerRenderPath::Sprite
            }
        }
        mode if mode.is_fixed_function() => LayerRenderPath::Material(mode),
        mode => {
            warn_dst_dependent_once(mode);
            LayerRenderPath::Sprite
        }
    }
}

/// Fragment shader for [`LayerBlendMaterial`] (embedded below). The
/// `embedded://` path is `<lib-name>/<path-after-src>`; this crate's lib is
/// `krkr_render`, so the path is `krkr_render/...` (not the package name).
const LAYER_BLEND_SHADER_PATH: &str = "embedded://krkr_render/layer_blend_material.wgsl";

/// Material for layers that need a non-default GPU blend: samples the
/// layer texture (straight alpha), multiplies the tint (which carries the
/// composed window × layer opacity, pre-scaled for multiply/screen), and lets
/// the pipeline variant's fixed-function `BlendState` do the compositing.
///
/// The specialization key ([`Material2d::Data`]) is the blend mode, so Bevy
/// compiles one pipeline variant per mode and entities pick their variant
/// through the material asset alone.
#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
#[bind_group_data(LayerBlendMode)]
pub struct LayerBlendMaterial {
    /// Straight-alpha tint × opacity (linear space). For multiply/screen the
    /// RGB is pre-multiplied by alpha so the fixed-function `Dst` factor
    /// yields `src·a·dst`.
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

    /// The GPU blend mode (the specialization key).
    pub fn mode(&self) -> LayerBlendMode {
        self.mode
    }

    /// The linear tint × opacity (RGB pre-multiplied for multiply/screen).
    pub fn color(&self) -> LinearRgba {
        self.color
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

    /// Every documented native type maps to its own mode — never silently to
    /// source-over except for the truly unknown / out-of-range values.
    #[test]
    fn blend_mode_mapping_is_total() {
        use LayerBlendMode::*;
        let expected = [
            (LT_BINDER, Binder),
            (LT_OPAQUE, Replace),
            (LT_ALPHA, SourceOver),
            (LT_ADDITIVE, Additive),
            (LT_SUBTRACTIVE, Subtractive),
            (LT_MULTIPLICATIVE, Multiply),
            (LT_DODGE, ColorDodge),
            (LT_DARKEN, Darken),
            (LT_LIGHTEN, Lighten),
            (LT_SCREEN, Screen),
            (LT_ADD_ALPHA, AddAlpha),
            (LT_PS_NORMAL, SourceOver),
            (LT_PS_ADDITIVE, Additive),
            (LT_PS_SUBTRACTIVE, Subtractive),
            (LT_PS_MULTIPLICATIVE, Multiply),
            (LT_PS_SCREEN, Screen),
            (LT_PS_OVERLAY, Overlay),
            (LT_PS_HARD_LIGHT, HardLight),
            (LT_PS_SOFT_LIGHT, SoftLight),
            (LT_PS_COLOR_DODGE, ColorDodge),
            (LT_PS_COLOR_DODGE5, ColorDodge5),
            (LT_PS_COLOR_BURN, ColorBurn),
            (LT_PS_LIGHTEN, Lighten),
            (LT_PS_DARKEN, Darken),
            (LT_PS_DIFFERENCE, Difference),
            (LT_PS_DIFFERENCE5, Difference5),
            (LT_PS_EXCLUSION, Exclusion),
        ];
        for (native, mode) in expected {
            assert_eq!(blend_mode_for(native), mode, "type {native}");
        }
        // Effect/filter are callbacks, not blends.
        assert_eq!(blend_mode_for(LT_EFFECT), SourceOver);
        assert_eq!(blend_mode_for(LT_FILTER), SourceOver);
        // Garbage values degrade safely.
        assert_eq!(blend_mode_for(-1), SourceOver);
        assert_eq!(blend_mode_for(9999), SourceOver);
    }

    /// The fixed-function blend states match the documented equations.
    #[test]
    fn blend_states_match_equations() {
        // Replace / Binder disable blending / drawing.
        assert_eq!(blend_state_for(LayerBlendMode::Replace), None);
        assert_eq!(blend_state_for(LayerBlendMode::Binder), None);

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

        // Add-alpha: src + dst*(1-a) — no SrcAlpha on the source.
        let add_alpha = blend_state_for(LayerBlendMode::AddAlpha).unwrap();
        assert_eq!(add_alpha.color.src_factor, BlendFactor::One);
        assert_eq!(add_alpha.color.dst_factor, BlendFactor::OneMinusSrcAlpha);
        assert_eq!(add_alpha.color.operation, BlendOperation::Add);

        // Multiply: (src*a)*dst + dst*(1-a).
        let mul = blend_state_for(LayerBlendMode::Multiply).unwrap();
        assert_eq!(mul.color.src_factor, BlendFactor::Dst);
        assert_eq!(mul.color.dst_factor, BlendFactor::OneMinusSrcAlpha);
        assert_eq!(mul.color.operation, BlendOperation::Add);

        // Screen: (src*a)*(1-dst) + dst.
        let screen = blend_state_for(LayerBlendMode::Screen).unwrap();
        assert_eq!(screen.color.src_factor, BlendFactor::OneMinusDst);
        assert_eq!(screen.color.dst_factor, BlendFactor::One);
        assert_eq!(screen.color.operation, BlendOperation::Add);
    }

    /// The mode classification drives both the render path and the sync's
    /// multiply/screen RGB pre-scaling.
    #[test]
    fn mode_classification() {
        use LayerBlendMode::*;
        for m in [
            SourceOver,
            Replace,
            Additive,
            Subtractive,
            AddAlpha,
            Multiply,
            Screen,
        ] {
            assert!(m.is_fixed_function(), "{m:?} should be fixed-function");
            assert!(!m.is_dst_dependent(), "{m:?} is not dst-dependent");
        }
        for m in [
            ColorDodge,
            ColorDodge5,
            ColorBurn,
            Darken,
            Lighten,
            Difference,
            Difference5,
            Exclusion,
            Overlay,
            HardLight,
            SoftLight,
        ] {
            assert!(!m.is_fixed_function(), "{m:?} is not fixed-function");
            assert!(m.is_dst_dependent(), "{m:?} is dst-dependent");
        }
        assert!(!Binder.is_fixed_function());
        assert!(
            !Binder.is_dst_dependent(),
            "binder draws nothing, not a fallback"
        );

        assert!(Multiply.is_premultiplied_source());
        assert!(Screen.is_premultiplied_source());
        for m in [SourceOver, Additive, Subtractive, AddAlpha, Replace] {
            assert!(!m.is_premultiplied_source(), "{m:?}");
        }
    }

    /// Render-path routing: opaque degrades to Sprite under partial
    /// opacity; binder is skipped; additive/subtractive/multiply/screen
    /// take the material path; dst-dependent modes fall back to Sprite.
    #[test]
    fn render_path_routing() {
        use LayerBlendMode as M;
        use LayerRenderPath::*;

        // ltOpaque at full opacity → replace material.
        assert_eq!(render_path_for(LT_OPAQUE, 1.0), Material(M::Replace));
        // …but partial opacity must still blend → plain sprite.
        assert_eq!(
            render_path_for(LT_OPAQUE, 0.5),
            Sprite,
            "opaque layer under partial opacity needs source-over"
        );
        // Out-of-range alphas are clamped before the decision.
        assert_eq!(render_path_for(LT_OPAQUE, f32::NAN), Sprite);
        assert_eq!(render_path_for(LT_OPAQUE, 3.0), Material(M::Replace));

        // Binder draws nothing.
        assert_eq!(render_path_for(LT_BINDER, 1.0), Skip);

        // Newly ported real GPU blends keep the material path at any opacity.
        assert_eq!(render_path_for(LT_ADD_ALPHA, 0.25), Material(M::AddAlpha));
        assert_eq!(
            render_path_for(LT_MULTIPLICATIVE, 0.25),
            Material(M::Multiply)
        );
        assert_eq!(render_path_for(LT_SCREEN, 1.0), Material(M::Screen));
        assert_eq!(
            render_path_for(LT_PS_MULTIPLICATIVE, 1.0),
            Material(M::Multiply)
        );
        assert_eq!(render_path_for(LT_PS_SCREEN, 1.0), Material(M::Screen));

        // Additive/subtractive keep their GPU blend at any opacity.
        assert_eq!(render_path_for(LT_ADDITIVE, 0.25), Material(M::Additive));
        assert_eq!(
            render_path_for(LT_SUBTRACTIVE, 1.0),
            Material(M::Subtractive)
        );

        // Destination-dependent modes fall back to the sprite pipeline.
        for t in [
            LT_DODGE,
            LT_DARKEN,
            LT_LIGHTEN,
            LT_PS_OVERLAY,
            LT_PS_HARD_LIGHT,
        ] {
            assert_eq!(render_path_for(t, 1.0), Sprite, "type {t}");
        }

        // Everything else stays on the untouched Sprite pipeline.
        for t in [LT_ALPHA, LT_PS_NORMAL, LT_EFFECT, LT_FILTER, 99] {
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
        assert_eq!(mat.mode(), LayerBlendMode::Additive);
        // Texture is kept for binding.
        assert_eq!(mat.color_texture.as_ref(), Some(&tex));
    }
}
