//! `motion_player` plugin — full port of the reference `motionplayer.dll`
//! surface (`Motion`, `Player`, `ResourceManager`, `SeparateLayerAdaptor`,
//! `D3DAdaptor`, `SourceCache`, `ObjSource`, `Point`, `Circle`, `Rect`,
//! `Quad`, `LayerGetter`).
//!
//! Behavioural reference: `reference/cpp/plugins/motionplayer/main.cpp`
//! (registration), `Player.*` (player core / timeline / layers / particles),
//! `MotionNode.h`, `NodeTree.cpp`, `ResourceManager.*`, `manual.tjs`.
//!
//! The reference is a large PSB-driven mesh/physics renderer. This port keeps
//! the complete script-visible surface and the playback model (timelines,
//! variables, transitions, wind/outer force, layer queries, particles) and
//! composes the L2 [`engine::Scene`] layer system for drawing instead of
//! re-implementing the D3D rasterizer. PSB data is parsed by [`psb`] and
//! mapped to the runtime model in [`model`].
//!
//! # Coordination note
//!
//! The reference splits the classes across two DLLs (`motionplayer.dll` and
//! `emoteplayer.dll`); the compat VM rejects duplicate class names, so
//! [`register`] is idempotent: it skips any class that already exists and
//! attaches its subclasses to the existing `Motion` object. Running alongside
//! a separate `emote_player` port therefore cannot make installation fail.
//!
//! Only `Player`, `Motion`, `D3DEmotePlayer` and `D3DEmoteModule` are
//! top-level classes in the reference (`reference/cpp/plugins/motionplayer/
//! main.cpp:128,427,565,588`). The remaining classes are registered through
//! the global-publishing ABI and their global is deleted right after they are
//! aliased into `Motion`, so they exist only as `Motion.*`.
#![allow(unsafe_code)]

#[macro_use]
mod macros;

pub(crate) mod adaptors;
pub(crate) mod animator;
pub(crate) mod emote;
pub(crate) mod helpers;
pub(crate) mod model;
pub(crate) mod player;
pub(crate) mod psb;
pub(crate) mod resource;

use std::ffi::c_void;

use tjs::{
    DetachedValue, NativeClassBuilder, NativePropertyDef, RetainedValue, TjsValue, VAL_INTEGER,
    VAL_NULL, VAL_OBJECT, VAL_REAL, VAL_STRING, Value,
};

use crate::native_util::value_as_string;
use crate::{CompatCtx, CompatError};

/// Per-engine scratch state shared by the `motion_player` callbacks.
///
/// The C++ trampoline copies a returned string immediately after the callback
/// returns, so the scratch buffer only has to outlive the call; keeping it in
/// the service state preserves the single-process-wide-binding rule.
#[derive(Default)]
pub(crate) struct MotionState {
    /// Scratch bytes for a returned string result.
    pub(crate) string: Vec<u8>,
}

/// A TJS value stored on a native instance across calls.
#[derive(Default)]
pub(crate) enum StoredValue {
    /// `void`
    #[default]
    Void,
    /// TJS `null`
    Null,
    /// Integer.
    Integer(i64),
    /// Real.
    Real(f64),
    /// String.
    String(String),
    /// Retained object.
    Object(DetachedValue),
}

impl StoredValue {
    /// Capture a raw callback value.
    pub(crate) fn from_value(value: &Value, ctx: &CompatCtx) -> Self {
        match value.ty {
            VAL_NULL => Self::Null,
            VAL_INTEGER => Self::Integer(value.integer),
            VAL_REAL => Self::Real(value.real),
            VAL_STRING => Self::String(value_as_string(value)),
            VAL_OBJECT => {
                // SAFETY: the caller resolved `ctx` from the installed binding.
                unsafe { ctx.engine() }
                    .retain_object_arg(value)
                    .map(Self::Object)
                    .unwrap_or(Self::Void)
            }
            _ => Self::Void,
        }
    }

    /// The TJS object id, if this value is an object.
    pub(crate) fn object_id(&self) -> Option<usize> {
        match self {
            Self::Object(module) => Some(module.raw_id() as usize),
            _ => None,
        }
    }

    /// Convert into a [`RetainedValue`] for a getter result.
    pub(crate) fn to_retained(&self, ctx: &CompatCtx) -> Result<RetainedValue, String> {
        match self {
            Self::Void | Self::Null => Ok(RetainedValue::Value(TjsValue::Void)),
            Self::Integer(value) => Ok(RetainedValue::Value(TjsValue::Integer(*value))),
            Self::Real(value) => Ok(RetainedValue::Value(TjsValue::Real(*value))),
            Self::String(value) => Ok(RetainedValue::Value(TjsValue::String(value.clone()))),
            Self::Object(module) => {
                // SAFETY: the caller resolved `ctx` from the installed binding.
                unsafe { ctx.engine() }
                    .duplicate_retained(module.raw_id())
                    .map(RetainedValue::Object)
                    .map_err(|error| error.to_string())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Motion namespace constants
// ---------------------------------------------------------------------------

/// The `Motion` namespace constant values registered by the reference.
const CONSTANTS: &[(&str, i64)] = &[
    ("LayerTypeObj", 0),
    ("LayerTypeShape", 1),
    ("LayerTypeLayout", 2),
    ("LayerTypeMotion", 3),
    ("LayerTypeParticle", 4),
    ("LayerTypeCamera", 5),
    ("ShapeTypePoint", 0),
    ("ShapeTypeCircle", 1),
    ("ShapeTypeRect", 2),
    ("ShapeTypeQuad", 3),
    ("PlayFlagForce", 1),
    ("PlayFlagChain", 2),
    ("PlayFlagAsCan", 4),
    ("PlayFlagJoin", 8),
    ("PlayFlagStealth", 16),
    ("TransformOrderFlip", 0),
    ("TransformOrderAngle", 1),
    ("TransformOrderZoom", 2),
    ("TransformOrderSlant", 3),
    ("CoordinateRecutangularXY", 0),
    ("CoordinateRecutangularXZ", 1),
    ("MaskModeStencil", 0),
    ("MaskModeAlpha", 1),
];

macro_rules! const_getter {
    ($name:ident, $value:expr) => {
        extern "C" fn $name(
            _engine: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut std::ffi::c_char,
        ) -> std::os::raw::c_int {
            crate::native_util::set_int(out, $value);
            0
        }
    };
}

const_getter!(motion_layertype_obj, 0);
const_getter!(motion_layertype_shape, 1);
const_getter!(motion_layertype_layout, 2);
const_getter!(motion_layertype_motion, 3);
const_getter!(motion_layertype_particle, 4);
const_getter!(motion_layertype_camera, 5);
const_getter!(motion_shapetype_point, 0);
const_getter!(motion_shapetype_circle, 1);
const_getter!(motion_shapetype_rect, 2);
const_getter!(motion_shapetype_quad, 3);
const_getter!(motion_playflag_force, 1);
const_getter!(motion_playflag_chain, 2);
const_getter!(motion_playflag_ascan, 4);
const_getter!(motion_playflag_join, 8);
const_getter!(motion_playflag_stealth, 16);
const_getter!(motion_transform_flip, 0);
const_getter!(motion_transform_angle, 1);
const_getter!(motion_transform_zoom, 2);
const_getter!(motion_transform_slant, 3);
const_getter!(motion_coord_xy, 0);
const_getter!(motion_coord_xz, 1);
const_getter!(motion_mask_stencil, 0);
const_getter!(motion_mask_alpha, 1);

fn motion_properties() -> Vec<NativePropertyDef> {
    let getters: std::collections::BTreeMap<&'static str, tjs::NativePropertyGetFn> = [
        (
            "LayerTypeObj",
            motion_layertype_obj as tjs::NativePropertyGetFn,
        ),
        ("LayerTypeShape", motion_layertype_shape),
        ("LayerTypeLayout", motion_layertype_layout),
        ("LayerTypeMotion", motion_layertype_motion),
        ("LayerTypeParticle", motion_layertype_particle),
        ("LayerTypeCamera", motion_layertype_camera),
        ("ShapeTypePoint", motion_shapetype_point),
        ("ShapeTypeCircle", motion_shapetype_circle),
        ("ShapeTypeRect", motion_shapetype_rect),
        ("ShapeTypeQuad", motion_shapetype_quad),
        ("PlayFlagForce", motion_playflag_force),
        ("PlayFlagChain", motion_playflag_chain),
        ("PlayFlagAsCan", motion_playflag_ascan),
        ("PlayFlagJoin", motion_playflag_join),
        ("PlayFlagStealth", motion_playflag_stealth),
        ("TransformOrderFlip", motion_transform_flip),
        ("TransformOrderAngle", motion_transform_angle),
        ("TransformOrderZoom", motion_transform_zoom),
        ("TransformOrderSlant", motion_transform_slant),
        ("CoordinateRecutangularXY", motion_coord_xy),
        ("CoordinateRecutangularXZ", motion_coord_xz),
        ("MaskModeStencil", motion_mask_stencil),
        ("MaskModeAlpha", motion_mask_alpha),
    ]
    .into_iter()
    .collect();
    CONSTANTS
        .iter()
        .filter_map(|(name, _)| {
            getters.get(name).map(|get| NativePropertyDef {
                name,
                get: Some(*get),
                set: None,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Idempotent registration helpers
// ---------------------------------------------------------------------------

/// Whether the global `name` is already defined.
fn global_exists(ctx: &CompatCtx, name: &str) -> bool {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let engine = unsafe { ctx.engine() };
    match engine.eval(&format!("typeof {name}"), "motion_player") {
        Ok(TjsValue::String(kind)) => kind != "undefined",
        Ok(_) => true,
        Err(_) => false,
    }
}

/// Whether `object.member` is already defined.
fn member_exists(ctx: &CompatCtx, object: &str, member: &str) -> bool {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let engine = unsafe { ctx.engine() };
    let expression = format!("typeof {object}.{member}");
    match engine.eval(&expression, "motion_player") {
        Ok(TjsValue::String(kind)) => kind != "undefined",
        _ => false,
    }
}

/// Attach `Motion.<public>` = the global class `internal`.
///
/// Returns whether the class object was found and attached; a missing
/// `internal` global is an ordinary `false` so the caller can decide whether
/// to delete its registration global.
fn attach_named(
    ctx: &CompatCtx,
    public: &str,
    internal: &str,
    motion_id: tjs::Tjs2ValueId,
) -> Result<bool, String> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let engine = unsafe { ctx.engine() };
    let Ok(RetainedValue::Object(class)) = engine.eval_retained(internal, "motion_player") else {
        return Ok(false);
    };
    let id = class.raw_id() as usize as u64;
    engine
        .set_member(motion_id, public, &TjsValue::Retained(id))
        .map_err(|error| error.to_string())?;
    drop(class);
    Ok(true)
}

/// Register the plugin's native surface on the VM.
///
/// Idempotent: classes that another `Motion` implementation already registered
/// are left untouched, so installing the plugin family cannot fail with a
/// duplicate-class error.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    // Motion namespace. If it already exists, only fill in missing members.
    if !global_exists(ctx, "Motion") {
        ctx.register_class(&NativeClassBuilder {
            name: "Motion",
            methods: vec![],
            properties: motion_properties(),
        })?;
    }

    // `Player` is a top-level class in the reference and is aliased into
    // `Motion` by the PostRegist callback.
    if !global_exists(ctx, "Player") {
        player::register(ctx)?;
    }
    // The namespace-only classes are `Motion.*` subclasses in the reference:
    // `NCB_REGISTER_SUBCLASS_DELAY` only defines their members and
    // `NCB_SUBCLASS` attaches them under `Motion` as a `TJS_STATICMEMBER`
    // (`reference/cpp/core/plugin/ncbind.hpp:1963-1970`, `:1982-1987`,
    // `reference/cpp/plugins/motionplayer/main.cpp:427-441`). The ABI always
    // publishes a global for a registration, so the port registers each here
    // and deletes that global after aliasing it into `Motion`; idempotency
    // therefore keys on the `Motion` member, not the deleted global.
    if !member_exists(ctx, "Motion", "ResourceManager") {
        resource::register(ctx)?;
    }
    if !member_exists(ctx, "Motion", "SeparateLayerAdaptor") {
        adaptors::register_adaptors(ctx)?;
    }
    if !member_exists(ctx, "Motion", "Point") {
        adaptors::register_geometry(ctx)?;
    }
    if !member_exists(ctx, "Motion", "EmotePlayer") {
        emote::register_emote_player(ctx)?;
    }
    // `D3DEmotePlayer` and `D3DEmoteModule` are genuine top-level classes in
    // the reference (`reference/cpp/plugins/motionplayer/main.cpp:565,588`).
    if !global_exists(ctx, "D3DEmotePlayer") {
        emote::register_d3d_emote_player(ctx)?;
    }
    if !global_exists(ctx, "D3DEmoteModule") {
        emote::register_d3d_emote_module(ctx)?;
    }

    // Keep the `Motion` and global objects alive while aliasing the
    // subclasses and deleting their registration globals.
    let (motion_handle, global_handle) = {
        // SAFETY: a `CompatCtx` is only created for a bound, live engine.
        let engine = unsafe { ctx.engine() };
        let motion = match engine.eval_retained("Motion", "motion_player") {
            Ok(RetainedValue::Object(object)) => Some(object),
            _ => None,
        };
        let global = match engine.eval_retained("global", "motion_player") {
            Ok(RetainedValue::Object(object)) => Some(object),
            _ => None,
        };
        (motion, global)
    };
    let motion_id = motion_handle
        .as_ref()
        .map(DetachedValue::raw_id)
        .unwrap_or(std::ptr::null_mut());
    let global_id = global_handle
        .as_ref()
        .map(DetachedValue::raw_id)
        .unwrap_or(std::ptr::null_mut());

    if !motion_id.is_null() {
        // (public `Motion.*` name, registration class name, keeps a global).
        // `Rect` is registered as `MotionRect` because the core VM already
        // owns the `Rect` class; `Player`, `D3DEmotePlayer` and
        // `D3DEmoteModule` are the only classes that keep their global
        // (`reference/cpp/plugins/motionplayer/main.cpp:128,565,588`).
        for (public, internal, top_level) in [
            ("Player", "Player", true),
            ("ResourceManager", "ResourceManager", false),
            ("SeparateLayerAdaptor", "SeparateLayerAdaptor", false),
            ("D3DAdaptor", "D3DAdaptor", false),
            ("SourceCache", "SourceCache", false),
            ("ObjSource", "ObjSource", false),
            ("Point", "Point", false),
            ("Circle", "Circle", false),
            ("Rect", "MotionRect", false),
            ("Quad", "Quad", false),
            ("LayerGetter", "LayerGetter", false),
            ("EmotePlayer", "EmotePlayer", false),
            ("D3DEmotePlayer", "D3DEmotePlayer", true),
            ("D3DEmoteModule", "D3DEmoteModule", true),
        ] {
            if !member_exists(ctx, "Motion", public) {
                let attached = attach_named(ctx, public, internal, motion_id).unwrap_or(false);
                // The global exists only because the registration ABI always
                // publishes one; the reference exposes these classes solely
                // under `Motion`. `delete_member`'s absent-member error
                // (`TJS_E_MEMBERNOTFOUND`, `tjsObject.cpp:1546`) is harmless:
                // it can only occur when an earlier install already removed
                // the global.
                if attached && !top_level && !global_id.is_null() {
                    let _ = ctx.delete_member(global_id, internal);
                }
            }
        }
    }
    Ok(())
}
