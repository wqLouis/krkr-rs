//! `EmotePlayer` / `D3DEmotePlayer` / `D3DEmoteModule` registration.
//!
//! Behavioural reference: `reference/cpp/plugins/motionplayer/EmotePlayer.h`
//! and the `emoteplayer.dll` registration block in `main.cpp`. The reference
//! `EmotePlayer` is a thin shell over `Player`; this port reuses the
//! [`super::player`] surface (the class is registered with the same callbacks
//! plus the EmotePlayer shell members), which is exactly the inherited shape.
//! `D3DEmoteModule` is a standalone settings object.
#![allow(unsafe_code)]

use std::ffi::c_void;

use tjs::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, NativePropertyDef,
    Value,
};

use super::resource::MotionStore;
use crate::native_util::{args, report_error, set_bool, set_int, set_void};
use crate::natives::system::{resolve, value_as_bool, value_as_i64};
use crate::{CompatCtx, CompatError};

/// `Motion.EmotePlayer` — the `Player` surface plus the EmotePlayer shell.
pub(crate) fn register_emote_player(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    super::player::register_named(ctx, "EmotePlayer", true)
}

/// `D3DEmotePlayer` — the Windows D3D subclass of `EmotePlayer`.
pub(crate) fn register_d3d_emote_player(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    super::player::register_named(ctx, "D3DEmotePlayer", true)
}

/// The `D3DEmoteModule` settings.
///
/// The reference declares every backing field `inline static`
/// (`reference/cpp/plugins/motionplayer/D3DEmoteModule.h:43-48`), so all
/// instances of the class share one value and the initial values are the
/// class-wide defaults (`maskMode` starts at 1 = `MaskModeAlpha`). The port
/// keeps the shared state per engine in [`super::resource::MotionStore`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct ModuleSettings {
    /// `MaskModeAlpha` in the reference (`D3DEmoteModule.h:43`).
    pub(crate) mask_mode: i64,
    pub(crate) mask_region_clipping: bool,
    pub(crate) mip_map_enabled: bool,
    /// `getAlphaOp`/`setAlphaOp` are `int` accessors
    /// (`reference/cpp/plugins/motionplayer/D3DEmoteModule.h:24-25`).
    pub(crate) alpha_op: i64,
    pub(crate) protect_translucent: bool,
    pub(crate) pixelate_division: i64,
    pub(crate) max_texture_size: i64,
}

impl Default for ModuleSettings {
    fn default() -> Self {
        Self {
            mask_mode: 1,
            mask_region_clipping: false,
            mip_map_enabled: false,
            alpha_op: 0,
            protect_translucent: false,
            pixelate_division: 1,
            max_texture_size: 0,
        }
    }
}

/// The per-instance payload of `D3DEmoteModule`.
///
/// The reference class stores nothing per instance (all state is static,
/// `D3DEmoteModule.h:43-48`); the payload exists only because the native
/// instance machinery requires a `create`/`destroy` pair.
pub(crate) struct ModuleInst;

extern "C" fn module_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(ModuleInst)) as *mut c_void
}

extern "C" fn module_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` came from `module_create` and is released once.
    drop(unsafe { Box::from_raw(instance as *mut ModuleInst) });
}

extern "C" fn module_ctor(
    engine: *mut c_void,
    _instance: *mut c_void,
    _argc: std::os::raw::c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut c_void,
) -> std::os::raw::c_int {
    if resolve(engine).is_none() {
        return report_error(out_error, super::helpers::NO_BINDING);
    }
    set_void(out);
    0
}

/// Integer-backed class-level setting (`maskMode`, `pixelateDivision`,
/// `alphaOp`, `maxTextureSize`).
macro_rules! module_int_member {
    ($get:ident, $set:ident, $field:ident) => {
        extern "C" fn $get(
            engine: *mut c_void,
            _instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut std::ffi::c_char,
            _objthis: *mut c_void,
        ) -> std::os::raw::c_int {
            let Some(res) = resolve(engine) else {
                return report_error(out_error, super::helpers::NO_BINDING);
            };
            let ctx = res.ctx();
            // SAFETY: the caller resolved `ctx` from the installed binding.
            let settings = unsafe { ctx.services() }.state::<MotionStore>().module;
            set_int(out, settings.$field);
            0
        }

        extern "C" fn $set(
            engine: *mut c_void,
            _instance: *mut c_void,
            value: *const Value,
            out_error: *mut *mut std::ffi::c_char,
            _objthis: *mut c_void,
        ) -> std::os::raw::c_int {
            let Some(res) = resolve(engine) else {
                return report_error(out_error, super::helpers::NO_BINDING);
            };
            // SAFETY: the VM passes a valid value for the duration of the call.
            let parsed = if value.is_null() {
                0
            } else {
                value_as_i64(unsafe { &*value })
            };
            let ctx = res.ctx();
            // SAFETY: the caller resolved `ctx` from the installed binding.
            let mut store = unsafe { ctx.services() }.state::<MotionStore>();
            store.module.$field = parsed;
            0
        }
    };
}

/// Boolean-backed class-level setting (`maskRegionClipping`,
/// `mipMapEnabled`, `protectTranslucentTextureColor`).
macro_rules! module_bool_member {
    ($get:ident, $set:ident, $field:ident) => {
        extern "C" fn $get(
            engine: *mut c_void,
            _instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut std::ffi::c_char,
            _objthis: *mut c_void,
        ) -> std::os::raw::c_int {
            let Some(res) = resolve(engine) else {
                return report_error(out_error, super::helpers::NO_BINDING);
            };
            let ctx = res.ctx();
            // SAFETY: the caller resolved `ctx` from the installed binding.
            let settings = unsafe { ctx.services() }.state::<MotionStore>().module;
            set_bool(out, settings.$field);
            0
        }

        extern "C" fn $set(
            engine: *mut c_void,
            _instance: *mut c_void,
            value: *const Value,
            out_error: *mut *mut std::ffi::c_char,
            _objthis: *mut c_void,
        ) -> std::os::raw::c_int {
            let Some(res) = resolve(engine) else {
                return report_error(out_error, super::helpers::NO_BINDING);
            };
            // SAFETY: the VM passes a valid value for the duration of the call.
            let parsed = if value.is_null() {
                false
            } else {
                value_as_bool(unsafe { &*value })
            };
            let ctx = res.ctx();
            // SAFETY: the caller resolved `ctx` from the installed binding.
            let mut store = unsafe { ctx.services() }.state::<MotionStore>();
            store.module.$field = parsed;
            0
        }
    };
}

module_int_member!(module_get_mask, module_set_mask, mask_mode);
module_bool_member!(
    module_get_clipping,
    module_set_clipping,
    mask_region_clipping
);
module_bool_member!(module_get_mipmap, module_set_mipmap, mip_map_enabled);
module_int_member!(module_get_alpha, module_set_alpha, alpha_op);
module_bool_member!(module_get_protect, module_set_protect, protect_translucent);
module_int_member!(module_get_pixelate, module_set_pixelate, pixelate_division);

/// `setMaxTextureSize(w, h)` — a static two-argument stub in the reference
/// (`reference/cpp/plugins/motionplayer/D3DEmoteModule.h:37-40`, registered
/// with `NCB_METHOD` at `main.cpp:585`), so calls with fewer than two
/// arguments fail the ncbind arity gate (`ncbind.hpp:1178`). The port keeps
/// the first argument readable through its (port-only) `maxTextureSize`
/// getter.
extern "C" fn module_set_max_texture(
    engine: *mut c_void,
    _instance: *mut c_void,
    argc: std::os::raw::c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut c_void,
) -> std::os::raw::c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, super::helpers::NO_BINDING);
    };
    // SAFETY: the trampoline guarantees `argc` valid entries.
    let arguments = unsafe { args(argv, argc) };
    if arguments.len() < 2 {
        return report_error(out_error, "setMaxTextureSize: bad parameter count");
    }
    let width = value_as_i64(&arguments[0]);
    let ctx = res.ctx();
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let mut store = unsafe { ctx.services() }.state::<MotionStore>();
    store.module.max_texture_size = width;
    set_void(out);
    0
}

extern "C" fn module_get_max_texture(
    engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut c_void,
) -> std::os::raw::c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, super::helpers::NO_BINDING);
    };
    let ctx = res.ctx();
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let settings = unsafe { ctx.services() }.state::<MotionStore>().module;
    set_int(out, settings.max_texture_size);
    0
}

macro_rules! module_const_getter {
    ($name:ident, $value:expr) => {
        extern "C" fn $name(
            _engine: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut std::ffi::c_char,
        ) -> std::os::raw::c_int {
            set_int(out, $value);
            0
        }
    };
}

module_const_getter!(module_timeline_parallel, 1);
module_const_getter!(module_timeline_sequential, 2);

/// Register the top-level `D3DEmoteModule` class.
pub(crate) fn register_d3d_emote_module(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "D3DEmoteModule",
        create: module_create,
        destroy: module_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "D3DEmoteModule",
                f: module_ctor,
            },
            NativeInstanceMethodDef {
                name: "setMaxTextureSize",
                f: module_set_max_texture,
            },
        ],
        properties: vec![
            NativeInstancePropertyDef {
                name: "maskMode",
                get: Some(module_get_mask),
                set: Some(module_set_mask),
            },
            NativeInstancePropertyDef {
                name: "maskRegionClipping",
                get: Some(module_get_clipping),
                set: Some(module_set_clipping),
            },
            NativeInstancePropertyDef {
                name: "mipMapEnabled",
                get: Some(module_get_mipmap),
                set: Some(module_set_mipmap),
            },
            NativeInstancePropertyDef {
                name: "alphaOp",
                get: Some(module_get_alpha),
                set: Some(module_set_alpha),
            },
            NativeInstancePropertyDef {
                name: "protectTranslucentTextureColor",
                get: Some(module_get_protect),
                set: Some(module_set_protect),
            },
            NativeInstancePropertyDef {
                name: "pixelateDivision",
                get: Some(module_get_pixelate),
                set: Some(module_set_pixelate),
            },
            NativeInstancePropertyDef {
                name: "maxTextureSize",
                get: Some(module_get_max_texture),
                set: None,
            },
        ],
    })?;
    ctx.register_static_members(&tjs::NativeStaticMembers {
        class_name: "D3DEmoteModule",
        methods: vec![],
        properties: vec![
            NativePropertyDef {
                name: "TimelinePlayFlagParallel",
                get: Some(module_timeline_parallel),
                set: None,
            },
            NativePropertyDef {
                name: "TimelinePlayFlagSequential",
                get: Some(module_timeline_sequential),
                set: None,
            },
        ],
    })?;
    Ok(())
}
