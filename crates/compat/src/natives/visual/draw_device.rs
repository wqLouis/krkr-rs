//! The visual draw-device native classes (`BasicDrawDevice`,
//! `PassThroughDrawDevice`).
//!
//! Reference: `reference/cpp/core/visual/impl/BasicDrawDevice.{cpp,h}` and
//! `PassThroughDrawDevice.{cpp,h}`. Both classes are created as script class
//! objects and attached to the `Window` class object
//! (`ScriptMgnIntf.cpp`), so scripts construct them as
//! `new Window.BasicDrawDevice()` / `new Window.PassThroughDrawDevice()` and
//! set `Window.drawDevice` to one of them.
//!
//! The native registration API always puts a class on the VM global object,
//! so this port registers `BasicDrawDevice` / `PassThroughDrawDevice`
//! top-level and then attaches the resulting class object to `Window` under
//! the reference name ([`attach_class`]). That is the same shape the
//! `Motion.*` plugin uses for its subclasses.
//!
//! The reference draw device owns a hardware/software drawer. This port has
//! no draw-device backend, so the device state — the current and preferred
//! drawer type plus the locked source size — lives in the native instance
//! payload and the script-visible members round-trip it. The `interface`
//! property is the raw `iTVPDrawDevice *` reinterpreted as an integer
//! (`BasicDrawDevice.cpp:886-896`, `PassThroughDrawDevice.cpp:532-544`);
//! with no draw-device object on this host, the getter reports the stable
//! native payload address as a non-zero synthetic pointer.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use tjs::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, NativePropertyDef,
    NativeStaticMembers, RetainedValue, TjsValue, Value,
};

use crate::native_util::{args, report_error, set_int, set_void};
use crate::natives::system::value_as_i64;
use crate::{CompatCtx, CompatError};

/// Reference `tTVPDrawDevice::tDrawerType` (`BasicDrawDevice.h`): `dtNone`.
const DT_NONE: i64 = 0;
/// Reference `dtDrawDib`: the plain, double-buffer-free drawer.
const DT_DRAW_DIB: i64 = 1;
/// Reference `dtDBGDI`.
const DT_DB_GDI: i64 = 2;
/// Reference `dtDBDD`.
const DT_DB_DD: i64 = 3;
/// Reference `dtDBD3D`.
const DT_DB_D3D: i64 = 4;

/// The native payload of one draw device.
struct DrawDeviceInst {
    /// Reference `DrawerType`: the drawer currently in use. The reference
    /// constructor initializes `BasicDrawDevice` to `dtDrawDib`
    /// (`BasicDrawDevice.h`) and `PassThroughDrawDevice` to `dtNone` until
    /// `EnsureDrawer` creates the software drawer
    /// (`PassThroughDrawDevice.cpp`).
    drawer: i64,
    /// Reference `PreferredDrawerType`: the drawer the script asked for. The
    /// reference initializes `BasicDrawDevice` to `dtDrawDib` and
    /// `PassThroughDrawDevice` to `TVPPreferredDrawType` (`dtNone`).
    preferred_drawer: i64,
    /// Reference `LockedWidth` / `LockedHeight`: a forced source size used by
    /// `GetSrcSize`. `(0, 0)` is unset (`tTVPDrawDevice`'s member defaults);
    /// `PassThroughDrawDevice` starts at `(-1, -1)`
    /// (`PassThroughDrawDevice.cpp`).
    locked: (i64, i64),
}

impl DrawDeviceInst {
    /// Reference `tTVPDrawDevice::GetSrcSize`: the locked size wins when it is
    /// positive, otherwise the primary layer size. This port has no layer
    /// manager, so an unlocked device reports the reference's zero-initialized
    /// size.
    fn src_size(&self) -> (i64, i64) {
        if self.locked.0 > 0 && self.locked.1 > 0 {
            self.locked
        } else {
            (0, 0)
        }
    }
}

// ---------------------------------------------------------------------------
// Instance lifecycle
// ---------------------------------------------------------------------------

/// Allocate a `BasicDrawDevice` payload (reference constructor defaults).
extern "C" fn basic_draw_device_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(DrawDeviceInst {
        drawer: DT_DRAW_DIB,
        preferred_drawer: DT_DRAW_DIB,
        locked: (0, 0),
    })) as *mut c_void
}

/// Allocate a `PassThroughDrawDevice` payload (reference constructor
/// defaults: `Drawer = nullptr` / `DrawerType = dtNone`, a `dtNone` preferred
/// drawer and `LockedWidth = LockedHeight = -1`).
extern "C" fn pass_through_draw_device_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(DrawDeviceInst {
        drawer: DT_NONE,
        preferred_drawer: DT_NONE,
        locked: (-1, -1),
    })) as *mut c_void
}

/// Free a payload created by one of the `*_create` functions.
extern "C" fn draw_device_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` came from a `*_create` `Box::into_raw` and is freed
    // exactly once by the VM.
    drop(unsafe { Box::from_raw(instance as *mut DrawDeviceInst) });
}

/// The reference constructor bodies are empty (`Construct` returns
/// `TJS_S_OK`), so every class shares this no-op constructor.
extern "C" fn draw_device_ctor(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// Methods
// ---------------------------------------------------------------------------

/// `BasicDrawDevice.recreate()` — the reference body is compiled out
/// (`#if 0`), so the method exists but changes nothing.
extern "C" fn basic_draw_device_recreate(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void(out);
    0
}

/// `PassThroughDrawDevice.recreate()` — reference `SetToRecreateDrawer()`
/// followed by `EnsureDrawer()`: the drawer is destroyed and the software
/// drawer (`dtDrawDib`) is created again. This port models the drawer as the
/// `drawer` value; `EnsureDrawer` consults `GetSrcSize` first, so the locked
/// size is read here too.
extern "C" fn pass_through_draw_device_recreate(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `DrawDeviceInst` payload the VM allocated for this script object; `pass_through_draw_device_recreate` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut DrawDeviceInst) };
    let (src_w, src_h) = inst.src_size();
    log::debug!("PassThroughDrawDevice.recreate: source size {src_w}x{src_h}");
    // `CreateDrawer(bool, bool)` ignores its flags in this reference revision
    // and always builds the software drawer
    // (`PassThroughDrawDevice.cpp`).
    inst.drawer = DT_DRAW_DIB;
    set_void(out);
    0
}

/// `PassThroughDrawDevice.lockTouchSize(width, height)` — reference
/// `SetLockedSize`, which stores the forced source size.
extern "C" fn pass_through_draw_device_lock_touch_size(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(
            out_error,
            "PassThroughDrawDevice.lockTouchSize requires 2 arguments",
        );
    }
    // SAFETY: `instance` is the live `DrawDeviceInst` payload the VM allocated for this script object; `pass_through_draw_device_lock_touch_size` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut DrawDeviceInst) };
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    inst.locked = (value_as_i64(&a[0]), value_as_i64(&a[1]));
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

/// `interface` — the reference returns `reinterpret_cast<tjs_int64>(
/// _this->GetDevice())`, the native `iTVPDrawDevice *` as an integer
/// (`BasicDrawDevice.cpp:886-896`, `PassThroughDrawDevice.cpp:532-544`).
/// This host has no draw-device object, so the getter reports the stable
/// native payload address as a non-zero synthetic pointer.
extern "C" fn draw_device_interface_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int(out, instance as i64);
    0
}

/// `drawer` — the drawer currently in use (reference read-only property).
extern "C" fn draw_device_drawer_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `DrawDeviceInst` payload the VM allocated for this script object; no `&mut` to it is live in `draw_device_drawer_get`.
    let inst = unsafe { &*(instance as *const DrawDeviceInst) };
    set_int(out, inst.drawer);
    0
}

/// `preferredDrawer` getter (reference read/write property).
extern "C" fn draw_device_preferred_drawer_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `DrawDeviceInst` payload the VM allocated for this script object; no `&mut` to it is live in `draw_device_preferred_drawer_get`.
    let inst = unsafe { &*(instance as *const DrawDeviceInst) };
    set_int(out, inst.preferred_drawer);
    0
}

/// `preferredDrawer` setter. The reference casts the value to `tjs_int`
/// first and then to `tDrawerType` without validation
/// (`reference/cpp/core/visual/impl/BasicDrawDevice.cpp:890-891`,
/// `PassThroughDrawDevice.cpp:575-576`): `(tjs_int)` truncates the 64-bit
/// integer to 32 bits, so a value outside the `i32` range reads back
/// truncated while any in-range integer round-trips.
extern "C" fn draw_device_preferred_drawer_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `DrawDeviceInst` payload the VM allocated for this script object; `draw_device_preferred_drawer_set` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut DrawDeviceInst) };
    // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
    inst.preferred_drawer = value_as_i64(unsafe { &*value }) as i32 as i64;
    0
}

/// Define a read-only integer property returning a compile-time constant.
macro_rules! draw_device_constant_prop {
    ($get:ident, $value:expr) => {
        extern "C" fn $get(
            _engine: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
        ) -> c_int {
            set_int(out, $value);
            0
        }
    };
}

// The reference `TVP_REGISTER_PTDD_ENUM` constants. The documented use is
// class-qualified (`Window.PassThroughDrawDevice.dtNone`), so they are
// registered as static members on the class object. The port's instance
// property dispatch requires a native payload, which a class-object access
// does not have, while `tTJSNativeClass::FuncCall` skips static members when
// copying onto instances.
draw_device_constant_prop!(dt_none_get, DT_NONE);
draw_device_constant_prop!(dt_draw_dib_get, DT_DRAW_DIB);
draw_device_constant_prop!(dt_db_gdi_get, DT_DB_GDI);
draw_device_constant_prop!(dt_db_dd_get, DT_DB_DD);
draw_device_constant_prop!(dt_db_d3d_get, DT_DB_D3D);

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register a class and attach its class object to `target`'s class object
/// under `name` (reference `windowclass->PropSet(..., "BasicDrawDevice", ...)`).
fn attach_class(ctx: &CompatCtx, target: &str, name: &str) -> Result<(), CompatError> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let engine = unsafe { ctx.engine() };
    let resolve = |member: &str| -> Result<tjs::DetachedValue, CompatError> {
        match engine.eval_retained(member, "draw_device.attach") {
            Ok(RetainedValue::Object(object)) => Ok(object),
            Ok(RetainedValue::Value(_)) => Err(CompatError::Engine(format!(
                "draw_device: {member} is not a class object"
            ))),
            Err(error) => Err(CompatError::Engine(format!(
                "draw_device: cannot resolve {member}: {error}"
            ))),
        }
    };
    let target_object = resolve(target)?;
    let class_object = resolve(name)?;
    let class_id = class_object.raw_id().as_ptr() as usize as u64;
    let result = engine
        .set_member(target_object.raw_id(), name, &TjsValue::Retained(class_id))
        .map_err(|error| {
            CompatError::Engine(format!(
                "draw_device: cannot attach {name} to {target}: {error}"
            ))
        });
    drop(target_object);
    drop(class_object);
    result
}

/// Attach the shared read-only drawer constants to `class`.
fn register_constants(ctx: &CompatCtx, class: &str) -> Result<(), CompatError> {
    ctx.register_static_members(&NativeStaticMembers {
        class_name: class,
        methods: Vec::new(),
        properties: vec![
            NativePropertyDef {
                name: "dtNone",
                get: Some(dt_none_get),
                set: None,
            },
            NativePropertyDef {
                name: "dtDrawDib",
                get: Some(dt_draw_dib_get),
                set: None,
            },
            NativePropertyDef {
                name: "dtDBGDI",
                get: Some(dt_db_gdi_get),
                set: None,
            },
            NativePropertyDef {
                name: "dtDBDD",
                get: Some(dt_db_dd_get),
                set: None,
            },
            NativePropertyDef {
                name: "dtDBD3D",
                get: Some(dt_db_d3d_get),
                set: None,
            },
        ],
    })
}

/// The instance properties common to both draw devices.
fn common_properties() -> Vec<NativeInstancePropertyDef> {
    vec![
        NativeInstancePropertyDef {
            name: "interface",
            get: Some(draw_device_interface_get),
            set: None,
        },
        NativeInstancePropertyDef {
            name: "drawer",
            get: Some(draw_device_drawer_get),
            set: None,
        },
        NativeInstancePropertyDef {
            name: "preferredDrawer",
            get: Some(draw_device_preferred_drawer_get),
            set: Some(draw_device_preferred_drawer_set),
        },
    ]
}

/// Register the two draw-device classes and attach them to `Window`.
pub(super) fn register_draw_devices(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "BasicDrawDevice",
        create: basic_draw_device_create,
        destroy: draw_device_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "BasicDrawDevice",
                f: draw_device_ctor,
            },
            NativeInstanceMethodDef {
                name: "recreate",
                f: basic_draw_device_recreate,
            },
        ],
        properties: common_properties(),
    })?;
    register_constants(ctx, "BasicDrawDevice")?;

    ctx.register_instance(&NativeInstanceBuilder {
        name: "PassThroughDrawDevice",
        create: pass_through_draw_device_create,
        destroy: draw_device_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "PassThroughDrawDevice",
                f: draw_device_ctor,
            },
            NativeInstanceMethodDef {
                name: "recreate",
                f: pass_through_draw_device_recreate,
            },
            NativeInstanceMethodDef {
                name: "lockTouchSize",
                f: pass_through_draw_device_lock_touch_size,
            },
        ],
        properties: common_properties(),
    })?;
    register_constants(ctx, "PassThroughDrawDevice")?;

    attach_class(ctx, "Window", "BasicDrawDevice")?;
    attach_class(ctx, "Window", "PassThroughDrawDevice")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{DT_DB_D3D, DT_DB_DD, DT_DB_GDI, DT_DRAW_DIB, DT_NONE, DrawDeviceInst};

    #[test]
    fn reference_defaults_match_the_headers() {
        let basic = DrawDeviceInst {
            drawer: DT_DRAW_DIB,
            preferred_drawer: DT_DRAW_DIB,
            locked: (0, 0),
        };
        assert_eq!(basic.drawer, 1);
        assert_eq!(basic.preferred_drawer, 1);
        assert_eq!(basic.src_size(), (0, 0));

        let pass_through = DrawDeviceInst {
            drawer: DT_NONE,
            preferred_drawer: DT_NONE,
            locked: (-1, -1),
        };
        assert_eq!(pass_through.drawer, 0);
        assert_eq!(pass_through.preferred_drawer, 0);
        assert_eq!(pass_through.src_size(), (0, 0));
        assert_eq!((DT_DB_GDI, DT_DB_DD, DT_DB_D3D), (2, 3, 4));
    }
}
