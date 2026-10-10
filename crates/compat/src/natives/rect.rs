//! `Rect` native class (geometry value object).
//!
//! Behavioral reference: `reference/cpp/core/visual/RectItf.cpp` (the
//! reference `tTJSNC_Rect` class and `tTJSNI_Rect` instance) and the
//! `tTVPRect` geometry in `reference/cpp/core/visual/ComplexRect.h`.
//!
//! `Rect` is a small script value object: its entire state is the four
//! 32-bit edges stored in the per-object [`RectData`] payload. It composes
//! no engine or scene state — the members are plain integer accessors and
//! the intersection/union helpers are pure functions over the four edges.
//!
//! Cross-object methods (`clip`, `union`, `intersects`, `included`, `equal`,
//! the one-argument copy constructor) receive another *script object*, not
//! another payload: they resolve the argument through the engine's
//! cross-object native-instance lookup
//! ([`Tjs2Engine::native_instance_payload`]), which is the exact counterpart
//! of the reference's `NativeInstanceSupport(TJS_NIS_GETINSTANCE,
//! tTJSNC_Rect::ClassID, ...)` (`visual/RectItf.cpp:14`, `135`, …): only a
//! live native `Rect` is accepted, `null` follows the reference
//! `clo.Object == nullptr` branch, and a plain script object — even one with
//! `left`/`top`/`right`/`bottom` members — is the reference's
//! `TJS_E_INVALIDPARAM` error rather than a duck-typed success.

use std::ffi::{c_char, c_int, c_void};

use tjs::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, Tjs2Engine,
    VAL_NULL, VAL_OBJECT, Value,
};

use crate::native_util::{args, report_error, set_bool, set_int, set_void};
use crate::natives::system::{resolve, try_value_as_i64};
use crate::{CompatCtx, CompatError};

// ---------------------------------------------------------------------------
// Per-object payload
// ---------------------------------------------------------------------------

/// Per-object `Rect` payload: the reference `tTVPRect` — four 32-bit edges.
///
/// `#[repr(C)]` keeps the fields contiguous and in reference order so the
/// `nativeArray` property can hand out the address of the first edge, just
/// like the reference `tTVPRect::array` union member.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RectData {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl RectData {
    /// `right - left` (reference `tTVPRect::get_width`).
    fn width(self) -> i32 {
        self.right.wrapping_sub(self.left)
    }

    /// `bottom - top` (reference `tTVPRect::get_height`).
    fn height(self) -> i32 {
        self.bottom.wrapping_sub(self.top)
    }

    /// Set the width, keeping `left` fixed (reference `set_width`).
    fn set_width(&mut self, width: i32) {
        self.right = self.left.wrapping_add(width);
    }

    /// Set the height, keeping `top` fixed (reference `set_height`).
    fn set_height(&mut self, height: i32) {
        self.bottom = self.top.wrapping_add(height);
    }

    /// Move the rectangle to `(x, y)`, preserving its size (reference
    /// `set_offsets`).
    fn set_offsets(&mut self, x: i32, y: i32) {
        let width = self.width();
        let height = self.height();
        self.left = x;
        self.top = y;
        self.right = x.wrapping_add(width);
        self.bottom = y.wrapping_add(height);
    }

    /// Translate the rectangle by `(x, y)` (reference `add_offsets`).
    fn add_offsets(&mut self, x: i32, y: i32) {
        self.left = self.left.wrapping_add(x);
        self.right = self.right.wrapping_add(x);
        self.top = self.top.wrapping_add(y);
        self.bottom = self.bottom.wrapping_add(y);
    }

    /// Set the size, keeping `(left, top)` fixed (reference `set_size`).
    fn set_size(&mut self, width: i32, height: i32) {
        self.set_width(width);
        self.set_height(height);
    }

    /// Reset every edge to zero (reference `clear`).
    fn clear(&mut self) {
        *self = Self::default();
    }

    /// `left >= right || top >= bottom` (reference `is_empty`).
    fn is_empty(self) -> bool {
        self.left >= self.right || self.top >= self.bottom
    }

    /// Intersect in place with `other`; returns whether a non-empty
    /// intersection remains (reference `TVPIntersectRect`).
    ///
    /// On an empty intersection `self` is left unchanged.
    fn clip(&mut self, other: Self) -> bool {
        let left = self.left.max(other.left);
        let top = self.top.max(other.top);
        let right = self.right.min(other.right);
        let bottom = self.bottom.min(other.bottom);
        if right > left && bottom > top {
            self.left = left;
            self.top = top;
            self.right = right;
            self.bottom = bottom;
            true
        } else {
            false
        }
    }

    /// Union in place with `other`; returns whether the union is non-empty
    /// (reference `TVPUnionRect`).
    ///
    /// On an empty union (both rectangles empty) `self` is left unchanged.
    fn union(&mut self, other: Self) -> bool {
        let left = self.left.min(other.left);
        let top = self.top.min(other.top);
        let right = self.right.max(other.right);
        let bottom = self.bottom.max(other.bottom);
        if right > left && bottom > top {
            self.left = left;
            self.top = top;
            self.right = right;
            self.bottom = bottom;
            true
        } else {
            false
        }
    }

    /// Whether `self` and `other` overlap (reference `intersects_with`).
    fn intersects(self, other: Self) -> bool {
        if other.is_empty() || self.is_empty() {
            return false;
        }
        !(self.left >= other.right
            || self.top >= other.bottom
            || self.right <= other.left
            || self.bottom <= other.top)
    }

    /// Whether `self` lies entirely inside `other` (reference `included_in`).
    fn included(self, other: Self) -> bool {
        if other.is_empty() || self.is_empty() {
            return false;
        }
        other.left <= self.left
            && other.top <= self.top
            && other.right >= self.right
            && other.bottom >= self.bottom
    }

    /// Half-open point containment: `left <= x < right && top <= y < bottom`
    /// (reference `tTJSNI_Rect::Included(tjs_int, tjs_int)`).
    fn contains_pos(self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

/// Borrow the live payload behind a native-instance pointer.
///
/// # Safety
/// `instance` must be the non-null payload the VM passed to a `Rect`
/// callback (allocated by [`rect_create`]) and must stay live for the
/// returned borrow.
unsafe fn payload(instance: *mut c_void) -> &'static mut RectData {
    // SAFETY: the caller guarantees `instance` is a live `RectData`.
    unsafe { &mut *(instance as *mut RectData) }
}

/// Resolve a script argument to another rectangle's edges.
///
/// Reference semantics (`visual/RectItf.cpp`):
///
/// * `null` (a `tvtObject` with a null pointer, `tjs2_abi.cpp:525-527`)
///   yields `Ok(None)`: the binary methods reach `if(clo.Object)` and skip
///   silently (`RectItf.cpp:129-131`), while the copy constructor reports
///   `TJS_E_INVALIDPARAM` (`RectItf.cpp:12-13`) — the callers split on it.
/// * A non-object argument is the reference's `AsObjectClosureNoAddRef`
///   conversion throw (`tjsVariant.h:728-736`) — a typed error.
/// * An object argument must be a live native `Rect`
///   (`NativeInstanceSupport`); anything else — including a plain dictionary
///   that happens to carry `left`/`top`/`right`/`bottom` — is the
///   reference's `TJS_E_INVALIDPARAM` (`RectItf.cpp:14-16`).
fn rect_arg(host: &Tjs2Engine, value: &Value) -> Result<Option<RectData>, String> {
    if value.ty == VAL_NULL {
        return Ok(None);
    }
    if value.ty != VAL_OBJECT || value.object_handle().is_null() {
        return Err("Rect: argument is not a Rect object".into());
    }
    let object = host
        .retain_object_arg(value)
        .map_err(|error| format!("Rect: argument cannot be retained: {error}"))?;
    let payload = host
        .native_instance_payload(object.raw_id(), "Rect")
        .map_err(|error| format!("Rect: argument lookup failed: {error}"))?;
    let Some(payload) = payload else {
        return Err("Rect: argument is not a Rect object".into());
    };
    // SAFETY: the payload is the engine-owned `RectData` allocated by
    // `rect_create`; it stays valid while `object` holds the retained id and
    // the single-threaded VM runs (no re-entry between the lookup and this
    // copy).
    Ok(Some(unsafe { *(payload.as_ptr() as *const RectData) }))
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// `AsInteger`-strict numeric argument read: converts like the reference
/// `operator tjs_int` (`tjsVariant.h:933-935`), which raises on objects
/// instead of silently reading `0`, and reports the typed callback error.
macro_rules! rect_int_arg {
    ($out_error:expr, $what:expr, $value:expr) => {
        match try_value_as_i64($value) {
            Ok(value) => value as i32,
            Err(error) => return report_error($out_error, &format!("Rect.{}: {error}", $what)),
        }
    };
}

/// Allocate the default (all-zero) `Rect` payload.
extern "C" fn rect_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(RectData::default())) as *mut c_void
}

/// Free a payload created by [`rect_create`].
extern "C" fn rect_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if !instance.is_null() {
        // SAFETY: `instance` came from `rect_create` and is released once.
        drop(unsafe { Box::from_raw(instance as *mut RectData) });
    }
}

/// `new Rect()` / `new Rect(src)` / `new Rect(left, top, right, bottom)`.
///
/// Argument counts other than 0, 1 and 4 are ignored, matching the
/// reference `Construct` (the fresh payload keeps its all-zero default,
/// `visual/RectItf.cpp:7-32`). Numeric arguments use the reference
/// `operator tjs_int` → `AsInteger` conversion, which raises on objects
/// (`tjsVariant.h:862-877`) instead of silently reading `0`.
extern "C" fn rect_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Rect: compat context is not bound");
    };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries and a
    // live payload for this constructor call.
    let arguments = unsafe { args(argv, argc) };
    let data = unsafe { payload(instance) };
    match argc {
        // `new Rect()` leaves the all-zero default.
        0 => {}
        // Copy constructor: `new Rect(src)`.
        1 => match rect_arg(resolved.engine(), &arguments[0]) {
            Ok(Some(source)) => *data = source,
            // `null` and non-Rect objects are the reference's
            // `TJS_E_INVALIDPARAM` (`RectItf.cpp:12-16`); non-objects are
            // its conversion throw.
            Ok(None) | Err(_) => {
                return report_error(out_error, "Rect: argument is not a Rect object");
            }
        },
        // `new Rect(left, top, right, bottom)`.
        4 => {
            let edges = [
                try_value_as_i64(&arguments[0]),
                try_value_as_i64(&arguments[1]),
                try_value_as_i64(&arguments[2]),
                try_value_as_i64(&arguments[3]),
            ];
            let mut values = [0_i32; 4];
            for (index, edge) in edges.into_iter().enumerate() {
                match edge {
                    Ok(value) => values[index] = value as i32,
                    Err(error) => {
                        return report_error(out_error, &format!("Rect: edge {index}: {error}"));
                    }
                }
            }
            *data = RectData {
                left: values[0],
                top: values[1],
                right: values[2],
                bottom: values[3],
            };
        }
        _ => {}
    }
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// Methods
// ---------------------------------------------------------------------------

/// Shared body of the binary rectangle methods.
///
/// `op` receives the receiver payload and the resolved argument rectangle.
/// `null` matches the reference's silent `if(clo.Object)` skip (void
/// result); every other failed resolution raises like the reference
/// (`AsObjectClosureNoAddRef` throw / `TJS_E_INVALIDPARAM`).
fn binary_rect_op(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    op: fn(&mut RectData, RectData) -> bool,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Rect: compat context is not bound");
    };
    if argc < 1 {
        return report_error(out_error, "Rect: method requires 1 argument");
    }
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    match rect_arg(resolved.engine(), &arguments[0]) {
        Ok(Some(source)) => {
            // SAFETY: `instance` is the live receiver payload.
            let data = unsafe { payload(instance) };
            set_bool(out, op(data, source));
            0
        }
        Ok(None) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `Rect.isEmpty()` → bool.
extern "C" fn rect_is_empty(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live receiver payload.
    set_bool(out, unsafe { payload(instance) }.is_empty());
    0
}

/// `Rect.setSize(width, height)`.
extern "C" fn rect_set_size(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    if arguments.len() < 2 {
        return report_error(out_error, "Rect.setSize: 2 arguments required");
    }
    let width = rect_int_arg!(out_error, "setSize", &arguments[0]);
    let height = rect_int_arg!(out_error, "setSize", &arguments[1]);
    // SAFETY: `instance` is the live receiver payload.
    unsafe { payload(instance) }.set_size(width, height);
    set_void(out);
    0
}

/// `Rect.setOffset(x, y)`.
extern "C" fn rect_set_offset(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    if arguments.len() < 2 {
        return report_error(out_error, "Rect.setOffset: 2 arguments required");
    }
    let x = rect_int_arg!(out_error, "setOffset", &arguments[0]);
    let y = rect_int_arg!(out_error, "setOffset", &arguments[1]);
    // SAFETY: `instance` is the live receiver payload.
    unsafe { payload(instance) }.set_offsets(x, y);
    set_void(out);
    0
}

/// `Rect.addOffset(x, y)`.
extern "C" fn rect_add_offset(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    if arguments.len() < 2 {
        return report_error(out_error, "Rect.addOffset: 2 arguments required");
    }
    let x = rect_int_arg!(out_error, "addOffset", &arguments[0]);
    let y = rect_int_arg!(out_error, "addOffset", &arguments[1]);
    // SAFETY: `instance` is the live receiver payload.
    unsafe { payload(instance) }.add_offsets(x, y);
    set_void(out);
    0
}

/// `Rect.clear()`.
extern "C" fn rect_clear(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live receiver payload.
    unsafe { payload(instance) }.clear();
    set_void(out);
    0
}

/// `Rect.set(left, top, right, bottom)`.
extern "C" fn rect_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    if arguments.len() < 4 {
        return report_error(out_error, "Rect.set: 4 arguments required");
    }
    // SAFETY: `instance` is the live receiver payload.
    *unsafe { payload(instance) } = RectData {
        left: rect_int_arg!(out_error, "set", &arguments[0]),
        top: rect_int_arg!(out_error, "set", &arguments[1]),
        right: rect_int_arg!(out_error, "set", &arguments[2]),
        bottom: rect_int_arg!(out_error, "set", &arguments[3]),
    };
    set_void(out);
    0
}

/// `Rect.clip(src)` → bool.
extern "C" fn rect_clip(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    binary_rect_op(engine, instance, argc, argv, out, out_error, |this, src| {
        this.clip(src)
    })
}

/// `Rect.union(src)` → bool.
extern "C" fn rect_union(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    binary_rect_op(engine, instance, argc, argv, out, out_error, |this, src| {
        this.union(src)
    })
}

/// `Rect.intersects(src)` → bool.
extern "C" fn rect_intersects(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    binary_rect_op(engine, instance, argc, argv, out, out_error, |this, src| {
        this.intersects(src)
    })
}

/// `Rect.included(src)` → bool.
extern "C" fn rect_included(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    binary_rect_op(engine, instance, argc, argv, out, out_error, |this, src| {
        this.included(src)
    })
}

/// `Rect.equal(src)` → bool.
extern "C" fn rect_equal(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    binary_rect_op(engine, instance, argc, argv, out, out_error, |this, src| {
        *this == src
    })
}

/// `Rect.includedPos(x, y)` → bool.
extern "C" fn rect_included_pos(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    if arguments.len() < 2 {
        return report_error(out_error, "Rect.includedPos: 2 arguments required");
    }
    let x = rect_int_arg!(out_error, "includedPos", &arguments[0]);
    let y = rect_int_arg!(out_error, "includedPos", &arguments[1]);
    // SAFETY: `instance` is the live receiver payload.
    set_bool(out, unsafe { payload(instance) }.contains_pos(x, y));
    0
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

/// Generate an integer edge getter reading `RectData::$field`.
macro_rules! rect_edge_getter {
    ($name:ident, $field:ident) => {
        extern "C" fn $name(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is the live receiver payload.
            set_int(out, i64::from(unsafe { payload(instance) }.$field));
            0
        }
    };
}

/// Generate an integer edge setter writing `RectData::$field`.
macro_rules! rect_edge_setter {
    ($name:ident, $field:ident) => {
        extern "C" fn $name(
            _engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `value` is the valid setter argument for this call.
            let value = unsafe { &*value };
            let edge = rect_int_arg!(out_error, stringify!($field), value);
            // SAFETY: `instance` is the live receiver payload.
            unsafe { payload(instance) }.$field = edge;
            0
        }
    };
}

rect_edge_getter!(rect_get_left, left);
rect_edge_getter!(rect_get_top, top);
rect_edge_getter!(rect_get_right, right);
rect_edge_getter!(rect_get_bottom, bottom);
rect_edge_setter!(rect_set_left, left);
rect_edge_setter!(rect_set_top, top);
rect_edge_setter!(rect_set_right, right);
rect_edge_setter!(rect_set_bottom, bottom);

/// `Rect.width` getter.
extern "C" fn rect_get_width(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live receiver payload.
    set_int(out, i64::from(unsafe { payload(instance) }.width()));
    0
}

/// `Rect.width` setter (keeps `left` fixed).
extern "C" fn rect_set_width(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `value` is the valid setter argument for this call.
    let width = rect_int_arg!(out_error, "width", unsafe { &*value });
    // SAFETY: `instance` is the live receiver payload.
    unsafe { payload(instance) }.set_width(width);
    0
}

/// `Rect.height` getter.
extern "C" fn rect_get_height(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live receiver payload.
    set_int(out, i64::from(unsafe { payload(instance) }.height()));
    0
}

/// `Rect.height` setter (keeps `top` fixed).
extern "C" fn rect_set_height(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `value` is the valid setter argument for this call.
    let height = rect_int_arg!(out_error, "height", unsafe { &*value });
    // SAFETY: `instance` is the live receiver payload.
    unsafe { payload(instance) }.set_height(height);
    0
}

/// `Rect.nativeArray` getter: the address of the first edge, matching the
/// reference `tTVPRect::array` pointer. Read-only.
extern "C" fn rect_get_native_array(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int(out, instance as usize as i64);
    0
}

/// The `Rect` instance property list (reference `width`, `height`, `left`,
/// `top`, `right`, `bottom`, `nativeArray`).
fn rect_properties() -> Vec<NativeInstancePropertyDef> {
    vec![
        NativeInstancePropertyDef {
            name: "width",
            get: Some(rect_get_width),
            set: Some(rect_set_width),
        },
        NativeInstancePropertyDef {
            name: "height",
            get: Some(rect_get_height),
            set: Some(rect_set_height),
        },
        NativeInstancePropertyDef {
            name: "left",
            get: Some(rect_get_left),
            set: Some(rect_set_left),
        },
        NativeInstancePropertyDef {
            name: "top",
            get: Some(rect_get_top),
            set: Some(rect_set_top),
        },
        NativeInstancePropertyDef {
            name: "right",
            get: Some(rect_get_right),
            set: Some(rect_set_right),
        },
        NativeInstancePropertyDef {
            name: "bottom",
            get: Some(rect_get_bottom),
            set: Some(rect_set_bottom),
        },
        NativeInstancePropertyDef {
            name: "nativeArray",
            get: Some(rect_get_native_array),
            set: None,
        },
    ]
}

/// The `Rect` instance method list (the class-name member is the
/// constructor).
fn rect_methods() -> Vec<NativeInstanceMethodDef> {
    vec![
        NativeInstanceMethodDef {
            name: "Rect",
            f: rect_ctor,
        },
        NativeInstanceMethodDef {
            name: "isEmpty",
            f: rect_is_empty,
        },
        NativeInstanceMethodDef {
            name: "setSize",
            f: rect_set_size,
        },
        NativeInstanceMethodDef {
            name: "setOffset",
            f: rect_set_offset,
        },
        NativeInstanceMethodDef {
            name: "addOffset",
            f: rect_add_offset,
        },
        NativeInstanceMethodDef {
            name: "clear",
            f: rect_clear,
        },
        NativeInstanceMethodDef {
            name: "set",
            f: rect_set,
        },
        NativeInstanceMethodDef {
            name: "clip",
            f: rect_clip,
        },
        NativeInstanceMethodDef {
            name: "union",
            f: rect_union,
        },
        NativeInstanceMethodDef {
            name: "intersects",
            f: rect_intersects,
        },
        NativeInstanceMethodDef {
            name: "included",
            f: rect_included,
        },
        NativeInstanceMethodDef {
            name: "includedPos",
            f: rect_included_pos,
        },
        NativeInstanceMethodDef {
            name: "equal",
            f: rect_equal,
        },
    ]
}

/// Register the `Rect` class.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "Rect",
        create: rect_create,
        destroy: rect_destroy,
        invalidate: None,
        methods: rect_methods(),
        properties: rect_properties(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_matches_the_reference_tvprect() {
        let mut r = RectData {
            left: 1,
            top: 2,
            right: 11,
            bottom: 22,
        };
        assert_eq!(r.width(), 10);
        assert_eq!(r.height(), 20);
        assert!(!r.is_empty());

        r.set_size(4, 5);
        assert_eq!((r.right, r.bottom), (5, 7));

        r.set_offsets(100, 200);
        assert_eq!((r.left, r.top, r.right, r.bottom), (100, 200, 104, 205));

        r.add_offsets(-100, -200);
        assert_eq!((r.left, r.top, r.right, r.bottom), (0, 0, 4, 5));

        r.set_width(10);
        r.set_height(10);
        assert_eq!((r.right, r.bottom), (10, 10));

        r.clear();
        assert!(r.is_empty());
        assert_eq!(r, RectData::default());
    }

    #[test]
    fn clip_union_and_relations_match_the_reference() {
        let a = RectData {
            left: 0,
            top: 0,
            right: 10,
            bottom: 10,
        };
        let b = RectData {
            left: 5,
            top: 5,
            right: 15,
            bottom: 15,
        };

        let mut clipped = a;
        assert!(clipped.clip(b));
        assert_eq!(
            clipped,
            RectData {
                left: 5,
                top: 5,
                right: 10,
                bottom: 10,
            }
        );

        // A disjoint clip leaves the receiver unchanged and returns false.
        let c = RectData {
            left: 20,
            top: 20,
            right: 30,
            bottom: 30,
        };
        let mut disjoint = a;
        assert!(!disjoint.clip(c));
        assert_eq!(disjoint, a);

        let mut united = a;
        assert!(united.union(b));
        assert_eq!(
            united,
            RectData {
                left: 0,
                top: 0,
                right: 15,
                bottom: 15,
            }
        );

        // The union of two empty rectangles is empty and returns false.
        let mut empty = RectData::default();
        assert!(!empty.union(RectData::default()));

        assert!(a.intersects(b));
        assert!(!a.intersects(c));
        assert!(a.included(RectData {
            left: -1,
            top: -1,
            right: 11,
            bottom: 11,
        }));
        assert!(!b.included(a));
        assert!(a.contains_pos(0, 0));
        assert!(!a.contains_pos(10, 10));
        assert_eq!(a, a);
    }
}
