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
//! renderer consults when a `Font.face` is set. The faces live in the L2
//! text service the engine owns, so every text consumer (the visual `Font`
//! native, the `TextRenderBase` plugin, the layer-ex draw plugin) reaches the
//! same registry: a face registered by `System.addFont` resolves by family
//! name and by the storage name it was read from.
//!
//! The font map is keyed by name and a name that is registered again
//! replaces the face behind it, so the newest registration of a name wins.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use tjs::{NativeMethodDef, NativeStaticMembers, Value};

use crate::engine_api::TextEngine;
use crate::native_util::{args, report_error, set_int, set_void, value_as_string};
use crate::natives::system::resolve;
use crate::resource::resource_state;
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
    // The font files the script looked for are read together: they sit next
    // to each other in the archive's data area, so one read of the range they
    // occupy brings them in, and one decode per file, on a worker of its own,
    // brings them back. The read that follows waits for its own file only.
    let together = {
        let mut state = resource_state(&ctx);
        state.take_font_files_to_read()
    };
    if !together.is_empty()
        && let Err(error) = storage.read_entries_together(&together)
    {
        log::debug!("System.addFont: the font files could not be read together: {error}");
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
    let service = match crate::resource::text_service(&ctx) {
        Ok(service) => service,
        Err(error) => return report_error(out_error, &error),
    };
    set_int(out, register_faces(service, &name, &bytes));
    0
}

/// Register every usable face in `bytes` under `source`.
///
/// Returns the number of faces (`0` when the data is not a usable font, the
/// the "font registration failed" result). The storage name resolves
/// the file's first face, so a script that asks for the file by name gets a
/// face to draw with.
///
/// Every face of the file is enumerated into the font map.
fn register_faces(service: &mut TextEngine, source: &str, bytes: &[u8]) -> i64 {
    let mut count: u32 = 0;
    let mut first = None;
    loop {
        match service.add_font(bytes, count) {
            Ok(id) => {
                first.get_or_insert(id);
                count += 1;
            }
            Err(error) => {
                if count == 0 {
                    log::debug!("System.addFont({source:?}): not a usable font: {error}");
                }
                break;
            }
        }
    }
    if count == 0 {
        return 0;
    }
    if let Some(id) = first {
        service.alias(source, id);
    }
    i64::from(count)
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
