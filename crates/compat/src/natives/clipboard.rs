//! `Clipboard` native class.
//!
//! Behavioral reference: the reference `Clipboard`
//! (`reference/cpp/core/utils/ClipboardIntf.cpp` plus the platform
//! `ClipboardImpl.cpp`). The reference class is a stateless, static surface
//! over the platform clipboard: it has no native instance and only three
//! class-level members, all of which are registered here:
//!
//! * `Clipboard` — the (no-op) constructor the parity checker counts;
//! * `hasFormat(format)` — the reference static method (`cbfText == 1`);
//! * `asText` — the reference static text property (get/set).
//!
//! # The host seam (D11)
//!
//! L3 owns no window system: the L1/L4 host owns the real clipboard and
//! installs a [`ClipboardBackend`] through
//! [`CompatHost::set_clipboard_backend`](crate::CompatHost::set_clipboard_backend).
//! The backend lives in [`CompatServices::state`], never in a process global.
//! When no backend is installed every member raises a **typed** TJS error; the
//! native never fabricates a `false`/void/empty value (D11). This differs
//! deliberately from the reference's non-platform stub, which returns
//! `false`/no-text — the layered port refuses to guess when the host has not
//! installed a clipboard.
//!
//! The backend expresses exactly the three platform functions
//! `TVPClipboardHasFormat` / `TVPClipboardGetText` / `TVPClipboardSetText`.
//!
//! # Reference semantics preserved
//!
//! * `hasFormat` returns `false` for any format other than `cbfText` (the
//!   reference's `switch` default), without consulting the backend.
//! * `asText` reads as **void** when the backend reports no text, matching the
//!   reference's "returns void if the clipboard does not have text data".

use std::ffi::{c_char, c_int, c_void};

use tjs::{NativeClassBuilder, NativeMethodDef, NativePropertyDef, Value};

use crate::native_util::{args, report_error, set_bool, set_void, value_as_string};
use crate::natives::system::{resolve, set_string_out, value_as_i64};
use crate::{CompatCtx, CompatError, CompatServices};

/// The clipboard format selector used by `Clipboard.hasFormat`.
///
/// Mirrors the reference `tTVPClipboardFormat` (`cbfText = 1`). The reference
/// treats every other value as "no such format", so [`Self::from_code`]
/// returns `None` for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum ClipboardFormat {
    /// `cbfText`: ANSI or Unicode text.
    Text = 1,
}

impl ClipboardFormat {
    /// The numeric code scripts pass to `hasFormat`.
    #[must_use]
    pub fn code(self) -> i64 {
        self as i64
    }

    /// Map a script integer to a known format, or `None` when the code has no
    /// reference counterpart.
    #[must_use]
    pub fn from_code(code: i64) -> Option<Self> {
        match code {
            1 => Some(Self::Text),
            _ => None,
        }
    }
}

/// The L1-owned clipboard operations the `Clipboard` native composes.
///
/// The host implements this over its real clipboard and installs it with
/// [`CompatHost::set_clipboard_backend`](crate::CompatHost::set_clipboard_backend).
/// The methods take `&self` so a platform backend may use interior mutability;
/// L3 only ever borrows the installed value.
pub trait ClipboardBackend {
    /// Whether the clipboard currently holds `format`
    /// (`TVPClipboardHasFormat`).
    fn has_format(&self, format: ClipboardFormat) -> bool;

    /// The clipboard text, or `None` when it holds no text
    /// (`TVPClipboardGetText`). `None` maps to the reference's void `asText`.
    fn get_text(&self) -> Option<String>;

    /// Replace the clipboard text (`TVPClipboardSetText`).
    fn set_text(&self, text: &str);
}

/// Adapter that lets a host install three plain closures as a backend.
///
/// Every `(has_format, get_text, set_text)` tuple of closures implements
/// [`ClipboardBackend`], so a host can call
/// [`CompatHost::set_clipboard_backend`](crate::CompatHost::set_clipboard_backend)
/// without naming the trait; a platform host may instead implement the trait
/// directly.
impl<H, G, S> ClipboardBackend for (H, G, S)
where
    H: Fn(i64) -> bool,
    G: Fn() -> Option<String>,
    S: Fn(String),
{
    fn has_format(&self, format: ClipboardFormat) -> bool {
        (self.0)(format.code())
    }

    fn get_text(&self) -> Option<String> {
        (self.1)()
    }

    fn set_text(&self, text: &str) {
        (self.2)(text.to_string());
    }
}

/// `Clipboard` dispatch state, keyed by type inside
/// [`CompatServices::state`].
#[derive(Default)]
struct ClipboardState {
    /// The host backend, or `None` until one is installed.
    backend: Option<Box<dyn ClipboardBackend>>,
}

/// Install the host clipboard backend.
///
/// Called by
/// [`CompatHost::set_clipboard_backend`](crate::CompatHost::set_clipboard_backend).
pub(crate) fn set_backend(services: &CompatServices, backend: Box<dyn ClipboardBackend>) {
    services.state::<ClipboardState>().backend = Some(backend);
}

/// `Clipboard.hasFormat(format)` → bool
extern "C" fn native_has_format(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Clipboard.hasFormat requires 1 argument");
    }
    let format = ClipboardFormat::from_code(value_as_i64(&unsafe { args(argv, argc) }[0]));
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "Clipboard.hasFormat: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let has = {
        let state = services.state::<ClipboardState>();
        let Some(backend) = state.backend.as_deref() else {
            return report_error(
                out_error,
                "Clipboard.hasFormat: no clipboard backend is installed",
            );
        };
        match format {
            Some(format) => backend.has_format(format),
            // Unknown formats have no representation (reference default).
            None => false,
        }
    };
    set_bool(out, has);
    0
}

/// `Clipboard.asText` getter → string, or void when the clipboard has no text.
extern "C" fn prop_as_text_get(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Clipboard.asText: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let text = {
        let state = services.state::<ClipboardState>();
        let Some(backend) = state.backend.as_deref() else {
            return report_error(
                out_error,
                "Clipboard.asText: no clipboard backend is installed",
            );
        };
        backend.get_text()
    };
    match text {
        Some(text) => set_string_out(&ctx, out, &text),
        None => set_void(out),
    }
    0
}

/// `Clipboard.asText = text` setter.
extern "C" fn prop_as_text_set(
    engine: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Clipboard.asText: compat context is not bound");
    };
    if value.is_null() {
        return report_error(out_error, "Clipboard.asText: missing value");
    }
    // SAFETY: the trampoline passes a valid value slot for the call.
    let text = value_as_string(unsafe { &*value });
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let state = services.state::<ClipboardState>();
    let Some(backend) = state.backend.as_deref() else {
        return report_error(
            out_error,
            "Clipboard.asText: no clipboard backend is installed",
        );
    };
    backend.set_text(&text);
    0
}

/// Register the `Clipboard` native class.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_class(&NativeClassBuilder {
        name: "Clipboard",
        methods: vec![NativeMethodDef {
            name: "hasFormat",
            f: native_has_format,
        }],
        properties: vec![NativePropertyDef {
            name: "asText",
            get: Some(prop_as_text_get),
            set: Some(prop_as_text_set),
        }],
    })
}
