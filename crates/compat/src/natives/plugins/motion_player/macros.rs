//! Callback-shape macros for the `motion_player` native surface.
//!
//! Every registered member has a distinct `extern "C"` function pointer, but
//! the bodies are all one of a handful of shapes. These macros expand a
//! one-line closure into the correct `tjs2_value` callback so the member
//! tables stay readable.

/// Generate a `f64` property getter.
#[macro_export]
macro_rules! mp_get_real {
    ($name:ident, $ty:ty, $f:expr) => {
        extern "C" fn $name(
            engine: *mut ::std::ffi::c_void,
            instance: *mut ::std::ffi::c_void,
            out: *mut ::tjs::Value,
            out_error: *mut *mut ::std::ffi::c_char,
            _objthis: *mut ::std::ffi::c_void,
        ) -> ::std::os::raw::c_int {
            $crate::natives::plugins::motion_player::helpers::get_real::<$ty>(
                engine, instance, out, out_error, $f,
            )
        }
    };
}

/// Generate a property getter for a member that holds a TJS array.
///
/// The closure returns the array's entries, or `None` when the member holds
/// no value.
#[macro_export]
macro_rules! mp_get_value_array {
    ($name:ident, $ty:ty, $f:expr) => {
        extern "C" fn $name(
            engine: *mut ::std::ffi::c_void,
            instance: *mut ::std::ffi::c_void,
            out: *mut ::tjs::Value,
            out_error: *mut *mut ::std::ffi::c_char,
            _objthis: *mut ::std::ffi::c_void,
        ) -> ::std::os::raw::c_int {
            $crate::natives::plugins::motion_player::helpers::get_value_array::<$ty>(
                engine, instance, out, out_error, $f,
            )
        }
    };
}

/// Generate a `f64` property setter.
#[macro_export]
macro_rules! mp_set_real {
    ($name:ident, $ty:ty, $f:expr) => {
        extern "C" fn $name(
            engine: *mut ::std::ffi::c_void,
            instance: *mut ::std::ffi::c_void,
            value: *const ::tjs::Value,
            out_error: *mut *mut ::std::ffi::c_char,
            _objthis: *mut ::std::ffi::c_void,
        ) -> ::std::os::raw::c_int {
            $crate::natives::plugins::motion_player::helpers::set_real_member::<$ty>(
                engine, instance, value, out_error, $f,
            )
        }
    };
}

/// Generate an integer property getter.
#[macro_export]
macro_rules! mp_get_int {
    ($name:ident, $ty:ty, $f:expr) => {
        extern "C" fn $name(
            engine: *mut ::std::ffi::c_void,
            instance: *mut ::std::ffi::c_void,
            out: *mut ::tjs::Value,
            out_error: *mut *mut ::std::ffi::c_char,
            _objthis: *mut ::std::ffi::c_void,
        ) -> ::std::os::raw::c_int {
            $crate::natives::plugins::motion_player::helpers::get_int::<$ty>(
                engine, instance, out, out_error, $f,
            )
        }
    };
}

/// Generate an integer property setter.
#[macro_export]
macro_rules! mp_set_int {
    ($name:ident, $ty:ty, $f:expr) => {
        extern "C" fn $name(
            engine: *mut ::std::ffi::c_void,
            instance: *mut ::std::ffi::c_void,
            value: *const ::tjs::Value,
            out_error: *mut *mut ::std::ffi::c_char,
            _objthis: *mut ::std::ffi::c_void,
        ) -> ::std::os::raw::c_int {
            $crate::natives::plugins::motion_player::helpers::set_int_member::<$ty>(
                engine, instance, value, out_error, $f,
            )
        }
    };
}

/// Generate a boolean property getter.
#[macro_export]
macro_rules! mp_get_bool {
    ($name:ident, $ty:ty, $f:expr) => {
        extern "C" fn $name(
            engine: *mut ::std::ffi::c_void,
            instance: *mut ::std::ffi::c_void,
            out: *mut ::tjs::Value,
            out_error: *mut *mut ::std::ffi::c_char,
            _objthis: *mut ::std::ffi::c_void,
        ) -> ::std::os::raw::c_int {
            $crate::natives::plugins::motion_player::helpers::get_bool::<$ty>(
                engine, instance, out, out_error, $f,
            )
        }
    };
}

/// Generate a boolean property setter.
#[macro_export]
macro_rules! mp_set_bool {
    ($name:ident, $ty:ty, $f:expr) => {
        extern "C" fn $name(
            engine: *mut ::std::ffi::c_void,
            instance: *mut ::std::ffi::c_void,
            value: *const ::tjs::Value,
            out_error: *mut *mut ::std::ffi::c_char,
            _objthis: *mut ::std::ffi::c_void,
        ) -> ::std::os::raw::c_int {
            $crate::natives::plugins::motion_player::helpers::set_bool_member::<$ty>(
                engine, instance, value, out_error, $f,
            )
        }
    };
}

/// Generate a string property getter.
#[macro_export]
macro_rules! mp_get_string {
    ($name:ident, $ty:ty, $f:expr) => {
        extern "C" fn $name(
            engine: *mut ::std::ffi::c_void,
            instance: *mut ::std::ffi::c_void,
            out: *mut ::tjs::Value,
            out_error: *mut *mut ::std::ffi::c_char,
            _objthis: *mut ::std::ffi::c_void,
        ) -> ::std::os::raw::c_int {
            $crate::natives::plugins::motion_player::helpers::get_string::<$ty>(
                engine, instance, out, out_error, $f,
            )
        }
    };
}

/// Generate a string property setter.
#[macro_export]
macro_rules! mp_set_string {
    ($name:ident, $ty:ty, $f:expr) => {
        extern "C" fn $name(
            engine: *mut ::std::ffi::c_void,
            instance: *mut ::std::ffi::c_void,
            value: *const ::tjs::Value,
            out_error: *mut *mut ::std::ffi::c_char,
            _objthis: *mut ::std::ffi::c_void,
        ) -> ::std::os::raw::c_int {
            $crate::natives::plugins::motion_player::helpers::set_string_member::<$ty>(
                engine, instance, value, out_error, $f,
            )
        }
    };
}

/// Generate a retained-value property getter.
#[macro_export]
macro_rules! mp_get_variant {
    ($name:ident, $ty:ty, $f:expr) => {
        extern "C" fn $name(
            engine: *mut ::std::ffi::c_void,
            instance: *mut ::std::ffi::c_void,
            out: *mut ::tjs::Value,
            out_error: *mut *mut ::std::ffi::c_char,
            _objthis: *mut ::std::ffi::c_void,
        ) -> ::std::os::raw::c_int {
            $crate::natives::plugins::motion_player::helpers::get_variant::<$ty>(
                engine, instance, out, out_error, $f,
            )
        }
    };
}

/// Generate a raw-variant property setter.
#[macro_export]
macro_rules! mp_set_variant {
    ($name:ident, $ty:ty, $f:expr) => {
        extern "C" fn $name(
            engine: *mut ::std::ffi::c_void,
            instance: *mut ::std::ffi::c_void,
            value: *const ::tjs::Value,
            out_error: *mut *mut ::std::ffi::c_char,
            _objthis: *mut ::std::ffi::c_void,
        ) -> ::std::os::raw::c_int {
            $crate::natives::plugins::motion_player::helpers::set_variant_member::<$ty>(
                engine, instance, value, out_error, $f,
            )
        }
    };
}

/// Generate a `void`-returning instance method.
#[macro_export]
macro_rules! mp_method_void {
    ($name:ident, $ty:ty, $f:expr) => {
        extern "C" fn $name(
            engine: *mut ::std::ffi::c_void,
            instance: *mut ::std::ffi::c_void,
            argc: ::std::os::raw::c_int,
            argv: *const ::tjs::Value,
            out: *mut ::tjs::Value,
            out_error: *mut *mut ::std::ffi::c_char,
            _objthis: *mut ::std::ffi::c_void,
        ) -> ::std::os::raw::c_int {
            $crate::natives::plugins::motion_player::helpers::method_void::<$ty>(
                engine, instance, argc, argv, out, out_error, $f,
            )
        }
    };
}

/// Generate an integer-returning instance method.
#[macro_export]
macro_rules! mp_method_int {
    ($name:ident, $ty:ty, $f:expr) => {
        extern "C" fn $name(
            engine: *mut ::std::ffi::c_void,
            instance: *mut ::std::ffi::c_void,
            argc: ::std::os::raw::c_int,
            argv: *const ::tjs::Value,
            out: *mut ::tjs::Value,
            out_error: *mut *mut ::std::ffi::c_char,
            _objthis: *mut ::std::ffi::c_void,
        ) -> ::std::os::raw::c_int {
            $crate::natives::plugins::motion_player::helpers::method_int::<$ty>(
                engine, instance, argc, argv, out, out_error, $f,
            )
        }
    };
}

/// Generate a `f64`-returning instance method.
#[macro_export]
macro_rules! mp_method_real {
    ($name:ident, $ty:ty, $f:expr) => {
        extern "C" fn $name(
            engine: *mut ::std::ffi::c_void,
            instance: *mut ::std::ffi::c_void,
            argc: ::std::os::raw::c_int,
            argv: *const ::tjs::Value,
            out: *mut ::tjs::Value,
            out_error: *mut *mut ::std::ffi::c_char,
            _objthis: *mut ::std::ffi::c_void,
        ) -> ::std::os::raw::c_int {
            $crate::natives::plugins::motion_player::helpers::method_real::<$ty>(
                engine, instance, argc, argv, out, out_error, $f,
            )
        }
    };
}

/// Generate a boolean-returning instance method.
#[macro_export]
macro_rules! mp_method_bool {
    ($name:ident, $ty:ty, $f:expr) => {
        extern "C" fn $name(
            engine: *mut ::std::ffi::c_void,
            instance: *mut ::std::ffi::c_void,
            argc: ::std::os::raw::c_int,
            argv: *const ::tjs::Value,
            out: *mut ::tjs::Value,
            out_error: *mut *mut ::std::ffi::c_char,
            _objthis: *mut ::std::ffi::c_void,
        ) -> ::std::os::raw::c_int {
            $crate::natives::plugins::motion_player::helpers::method_bool::<$ty>(
                engine, instance, argc, argv, out, out_error, $f,
            )
        }
    };
}

/// Generate a string-returning instance method.
#[macro_export]
macro_rules! mp_method_string {
    ($name:ident, $ty:ty, $f:expr) => {
        extern "C" fn $name(
            engine: *mut ::std::ffi::c_void,
            instance: *mut ::std::ffi::c_void,
            argc: ::std::os::raw::c_int,
            argv: *const ::tjs::Value,
            out: *mut ::tjs::Value,
            out_error: *mut *mut ::std::ffi::c_char,
            _objthis: *mut ::std::ffi::c_void,
        ) -> ::std::os::raw::c_int {
            $crate::natives::plugins::motion_player::helpers::method_string::<$ty>(
                engine, instance, argc, argv, out, out_error, $f,
            )
        }
    };
}

/// Generate a retained-value-returning instance method.
#[macro_export]
macro_rules! mp_method_variant {
    ($name:ident, $ty:ty, $f:expr) => {
        extern "C" fn $name(
            engine: *mut ::std::ffi::c_void,
            instance: *mut ::std::ffi::c_void,
            argc: ::std::os::raw::c_int,
            argv: *const ::tjs::Value,
            out: *mut ::tjs::Value,
            out_error: *mut *mut ::std::ffi::c_char,
            _objthis: *mut ::std::ffi::c_void,
        ) -> ::std::os::raw::c_int {
            $crate::natives::plugins::motion_player::helpers::method_variant::<$ty>(
                engine, instance, argc, argv, out, out_error, $f,
            )
        }
    };
}
