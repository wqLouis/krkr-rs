//! `add_font` plugin — a port of `reference/cpp/plugins/addFont.cpp`.
//!
//! The reference `NCB_ATTACH_CLASS(FontEx, System)` adds one static member to
//! the core `System` class:
//!
//! * `System.addFont(fontFileName[, extract])` reads a font file through the
//!   TVP storage and registers every face in it for later text rendering,
//!   returning the number of faces. The result is `void` when the storage
//!   name does not resolve and `0` when the file exists but is not a usable
//!   font.
//!
//! `extract` is accepted and ignored, exactly as in the reference.
//!
//! # Registration seam
//!
//! The reference stores the faces in the process-wide `TVPFontNames` map the
//! renderer consults when a `Font.face` is set. Compat has no process global
//! so the map is the per-engine [`registry::FontRegistry`] dispatch
//! state. It keeps the L2 `engine_api::TextEngine` each file was parsed
//! with, so any in-crate text consumer can resolve a face with
//! [`registry::FontRegistry::lookup`] and load the returned bytes/collection
//! index into its own engine.
//!
//! The visual `Font` native (`natives/visual/font.rs`) and the
//! `TextRenderBase` plugin (`plugins/text_render/metrics.rs`) consult
//! [`registry::FontRegistry::lookup`] before falling back to reading the face
//! string as a storage name, so a face registered by `System.addFont`
//! resolves by family name or by the storage name it was read from.
#![allow(unsafe_code)]

/// The face registry shared with in-crate text consumers.
pub(crate) mod registry;

use std::ffi::{c_char, c_int, c_void};

use tjs::{NativeMethodDef, NativeStaticMembers, Value};

use crate::native_util::{args, report_error, set_int, set_void, value_as_string};
use crate::natives::system::resolve;
use crate::storage::strip_file_media;
use crate::{CompatCtx, CompatError};

/// `System.addFont(fontFileName[, extract])`.
extern "C" fn system_add_font(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "System.addFont: compat context is not bound");
    };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    let Some(name) = arguments.first() else {
        return report_error(out_error, "System.addFont requires a font file name");
    };
    let name = value_as_string(name);
    if name.is_empty() {
        // The reference's placed-path lookup yields nothing for an empty
        // name, leaving the result void.
        set_void(out);
        return 0;
    }
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let Some(storage) = services.storage() else {
        return report_error(out_error, "System.addFont: no storage attached");
    };
    // SAFETY: the host keeps the attached storage alive for the binding's
    // lifetime (the `attach_storage` contract).
    let storage = unsafe { &*storage };
    // The media prefix is L3 syntax; the L2 namespace resolves the bare name.
    let lookup = strip_file_media(&name);
    match storage.exists(lookup) {
        Ok(true) => {}
        // The reference resolves the path first; a name that does not resolve
        // leaves the result void.
        Ok(false) => {
            set_void(out);
            return 0;
        }
        Err(error) => return report_error(out_error, &format!("System.addFont: {error}")),
    }
    // The reference runs `TVPEnumFontsProc(filename)` on *every* call
    // (`reference/cpp/plugins/addFont.cpp:22-28`) — the file is re-read and
    // re-enumerated, and a source whose contents changed reports the new
    // face count — so there is deliberately no cached-count shortcut here.
    let bytes = match storage.read(lookup) {
        Ok(bytes) => bytes,
        // The reference returns 0 when the file exists but cannot be opened.
        Err(error) => {
            log::debug!("System.addFont({name:?}): could not read the font file: {error}");
            set_int(out, 0);
            return 0;
        }
    };
    set_int(out, registry::register(services, &name, &bytes));
    0
}

/// Register the `System.addFont` static member added by `addFont.dll`.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_static_members(&NativeStaticMembers {
        class_name: "System",
        methods: vec![NativeMethodDef {
            name: "addFont",
            f: system_add_font,
        }],
        properties: vec![],
    })
}
