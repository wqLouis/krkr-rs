//! `Layer` event-handler members (group 2, plus the focus/transition
//! handlers).
//!
//! The reference declares these as native methods
//! (`TJS_BEGIN_NATIVE_METHOD_DECL(onClick)` …) that forward to the action
//! owner. The L3 event adapter / `BitmapLayerTreeOwner` (a separate agent)
//! dispatches them by name at runtime, so they are registered here as
//! assignable instance methods with a no-op default — exactly the same shape
//! `Timer.onTimer` and `MenuItem.onClick` use. A script override
//! (`layer.onClick = function …`) shadows the method; the adapter resolves
//! the object member by name and calls it.
//!
//! Argument counts follow the reference exactly: every handler builds its
//! event object behind `TVP_ACTION_INVOKE_BEGIN(argnum, …)`
//! (`reference/cpp/core/base/EventIntf.h:208-211`), whose first statement is
//! `if(numparams < (argnum)) return TJS_E_BADPARAMCOUNT;`. The check sits
//! inside `if(obj.Object)` on the action owner
//! (`reference/cpp/core/visual/LayerIntf.cpp:9970-9984`), and a normally
//! constructed layer always has one: the reference constructor stores
//! `ActionOwner = param[0]` (`LayerIntf.cpp:476`), i.e. the window object
//! passed to `new Layer(window, parent)`. The one exception is `onHitTest`,
//! whose check is unconditional (`LayerIntf.cpp:9960-9961`). Handlers with
//! `argnum == 0` (`onPaint`, `onMouseEnter`, …) can never fail the check.

use std::ffi::{c_char, c_int, c_void};

use tjs::{NativeInstanceMethodDef, Value};

use crate::native_util::{report_error, set_void};

/// Define one event handler method with its reference `TVP_ACTION_INVOKE_BEGIN`
/// argument count.
macro_rules! event_method {
    ($name:ident, $method:literal, 0) => {
        extern "C" fn $name(
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
    };
    ($name:ident, $method:literal, $argnum:expr) => {
        extern "C" fn $name(
            _engine: *mut c_void,
            _instance: *mut c_void,
            argc: c_int,
            _argv: *const Value,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            if argc < $argnum {
                return report_error(
                    out_error,
                    concat!(
                        "Layer.",
                        $method,
                        ": requires ",
                        stringify!($argnum),
                        " arguments"
                    ),
                );
            }
            set_void(out);
            0
        }
    };
}

// The argument counts are the reference `TVP_ACTION_INVOKE_BEGIN(n, …)`
// literals (`reference/cpp/core/visual/LayerIntf.cpp:9944-10472`); each is
// paired with the member the reference registers at the same site.
event_method!(on_before_focus, "onBeforeFocus", 3);
event_method!(on_blur, "onBlur", 1);
event_method!(on_click, "onClick", 2);
event_method!(on_double_click, "onDoubleClick", 2);
event_method!(on_focus, "onFocus", 1);
event_method!(on_hit_test, "onHitTest", 3);
event_method!(on_key_down, "onKeyDown", 3);
event_method!(on_key_press, "onKeyPress", 2);
event_method!(on_key_up, "onKeyUp", 3);
event_method!(on_mouse_down, "onMouseDown", 4);
event_method!(on_mouse_enter, "onMouseEnter", 0);
event_method!(on_mouse_leave, "onMouseLeave", 0);
event_method!(on_mouse_move, "onMouseMove", 3);
event_method!(on_mouse_up, "onMouseUp", 4);
event_method!(on_mouse_wheel, "onMouseWheel", 4);
event_method!(on_multi_touch, "onMultiTouch", 0);
event_method!(on_node_disabled, "onNodeDisabled", 0);
event_method!(on_node_enabled, "onNodeEnabled", 0);
event_method!(on_paint, "onPaint", 0);
event_method!(on_search_next_focusable, "onSearchNextFocusable", 1);
event_method!(on_search_prev_focusable, "onSearchPrevFocusable", 1);
event_method!(on_touch_down, "onTouchDown", 5);
event_method!(on_touch_move, "onTouchMove", 5);
event_method!(on_touch_rotate, "onTouchRotate", 6);
event_method!(on_touch_scaling, "onTouchScaling", 5);
event_method!(on_touch_up, "onTouchUp", 5);
event_method!(on_transition_completed, "onTransitionCompleted", 2);

fn d(name: &'static str, f: tjs::NativeInstanceMethodFn) -> NativeInstanceMethodDef {
    NativeInstanceMethodDef { name, f }
}

/// Every event-handler member as a `NativeInstanceMethodDef`.
pub(super) fn method_defs() -> Vec<NativeInstanceMethodDef> {
    vec![
        d("onBeforeFocus", on_before_focus),
        d("onBlur", on_blur),
        d("onClick", on_click),
        d("onDoubleClick", on_double_click),
        d("onFocus", on_focus),
        d("onHitTest", on_hit_test),
        d("onKeyDown", on_key_down),
        d("onKeyPress", on_key_press),
        d("onKeyUp", on_key_up),
        d("onMouseDown", on_mouse_down),
        d("onMouseEnter", on_mouse_enter),
        d("onMouseLeave", on_mouse_leave),
        d("onMouseMove", on_mouse_move),
        d("onMouseUp", on_mouse_up),
        d("onMouseWheel", on_mouse_wheel),
        d("onMultiTouch", on_multi_touch),
        d("onNodeDisabled", on_node_disabled),
        d("onNodeEnabled", on_node_enabled),
        d("onPaint", on_paint),
        d("onSearchNextFocusable", on_search_next_focusable),
        d("onSearchPrevFocusable", on_search_prev_focusable),
        d("onTouchDown", on_touch_down),
        d("onTouchMove", on_touch_move),
        d("onTouchRotate", on_touch_rotate),
        d("onTouchScaling", on_touch_scaling),
        d("onTouchUp", on_touch_up),
        d("onTransitionCompleted", on_transition_completed),
    ]
}
