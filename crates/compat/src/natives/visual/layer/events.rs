//! `Layer` event-handler members (group 2, plus the focus/transition
//! handlers).
//!
//! The reference declares these as native methods
//! (`TJS_BEGIN_NATIVE_METHOD_DECL(onClick)` …) that forward to the action
//! owner. The L3 event adapter / `BitmapLayerTreeOwner` (a separate agent)
//! dispatches them by name at runtime, so they are registered here as
//! assignable instance methods with a no-op default — exactly the same shape
//! `Timer.onTimer` and `MenuItem.onClick` use. A script override
//! (`layer.onClick = function …`) shadows the method; the adapter resolves the
//! object member by name and calls it.

use std::ffi::{c_char, c_int, c_void};

use tjs::{NativeInstanceMethodDef, Value};

use crate::native_util::set_void;

/// Define one no-op event handler method.
macro_rules! event_method {
    ($name:ident) => {
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
}

event_method!(on_before_focus);
event_method!(on_blur);
event_method!(on_click);
event_method!(on_double_click);
event_method!(on_focus);
event_method!(on_hit_test);
event_method!(on_key_down);
event_method!(on_key_press);
event_method!(on_key_up);
event_method!(on_mouse_down);
event_method!(on_mouse_enter);
event_method!(on_mouse_leave);
event_method!(on_mouse_move);
event_method!(on_mouse_up);
event_method!(on_mouse_wheel);
event_method!(on_multi_touch);
event_method!(on_node_disabled);
event_method!(on_node_enabled);
event_method!(on_paint);
event_method!(on_search_next_focusable);
event_method!(on_search_prev_focusable);
event_method!(on_touch_down);
event_method!(on_touch_move);
event_method!(on_touch_rotate);
event_method!(on_touch_scaling);
event_method!(on_touch_up);
event_method!(on_transition_completed);

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
