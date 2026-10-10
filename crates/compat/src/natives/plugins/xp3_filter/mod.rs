//! `xp3filter.dll` — the managed XP3 extraction/content filter.
//!
//! Reference: `reference/cpp/plugins/xp3filter.cpp`. The plugin reads
//! `xp3filter.tjs`, registers two static members on `Storages`
//! (`setXP3ArchiveExtractionFilter` / `setXP3ArchiveContentFilter`), and
//! installs Rust wrappers that the XP3 reader invokes per entry. The game's
//! script supplies the decryption algorithm; this module is only the
//! mechanism.
//!
//! # Layering
//!
//! * L1 (`fs::xp3`) owns the stateless per-segment hook
//!   ([`fs::xp3::Xp3ExtractionFilter`]); the filter is passed in and never
//!   stored there.
//! * L2 (`engine::Storage`) owns the installed filter and hands it to L1 on
//!   every archive read ([`engine::Storage::xp3_extraction_filter`]).
//! * L3 (this module) registers the `Storages` members, retains the TJS
//!   closures in [`CompatServices::state`], and installs an L2 filter that
//!   marshals each decoded segment to the retained TJS closure. The content
//!   filter runs first, exactly like the reference's
//!   `TVPXP3ArchiveContentFilterWrapper` (`xp3filter.cpp:465-491`), and it
//!   runs whether or not an extraction filter is installed.
//!
//! # Script load timing
//!
//! The reference reads `<app>/xp3filter.tjs` from a post-registration
//! callback and executes it in a **dedicated sub-engine** whose global scope
//! holds only `Debug`/`System`/`Storages` (`xp3filter.cpp:394-421`), so
//! nothing the filter script declares is visible to the game script. The
//! port's [`CompatHost`](crate::CompatHost) installs the engine → context
//! binding *after* native registration and the shared engine cannot expose a
//! second global, so this module (a) loads the script eagerly when the
//! binding is already live (registration harnesses that bind first),
//! otherwise deferring it to the first archive read, and (b) wraps the
//! source in a named local function so its declarations stay local to the
//! filter script — the same isolation the sub-engine provides for top-level
//! `function`/`var`. A function *expression* cannot host the wrapper: TJS2
//! does not bind a `function` declaration inside an anonymous function body
//! (the name resolves neither locally nor on `global`).
//! Residual difference: the filter script still resolves free names against
//! the game's global scope (a second engine would have to be created per
//! read, which the cross-engine call seam does not allow), and an explicit
//! `global.x = ...` assignment is therefore visible to the game script,
//! where the reference keeps it in the sub-engine.
//!
//! # Argument marshalling
//!
//! The reference passes a `CBinaryAccessor` whose indexed accessors and
//! `xor` method rewrite the segment in place. The VM's C ABI cannot expose a
//! borrowed byte slice as a native object, so the port marshals the segment
//! as a TJS `Array` of byte values (indexable, and given an `xor` method
//! matching the reference's), calls the closure, then copies the array back
//! into the segment. The closure receives the reference's argument list:
//! `(file_hash, offset, buffer, buffer_size, file_name, ctx)`.
//!
//! The content filter is invoked once per archive entry read before the
//! extraction filter; its `[ret, ctx]` return value contributes `ctx` (the
//! `ret` flag is ignored because the L1 read is always eager, which is what
//! `ret == 1` would have forced).
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use engine::StorageReadError;
use tjs::{
    DetachedValue, Engine, NativeInstanceMembers, NativeMethodDef, NativeObjThisMethodDef,
    NativeStaticMembers, RetainedValue, Tjs2Engine, TjsValue, VAL_OBJECT, Value,
};

use crate::binding;
use crate::native_util::{args, report_error, set_void};
use crate::natives::system::value_as_i64;
use crate::{CompatCtx, CompatError, CompatServices};

/// The configuration script the reference reads from the application path.
const FILTER_SCRIPT: &str = "xp3filter.tjs";

/// Per-install XP3 filter dispatch state, kept in
/// [`CompatServices::state`](crate::CompatServices::state).
///
/// The retained closures are `DetachedValue`s so the plugin never holds a
/// borrow of the engine; the `script_loaded` flag makes the deferred
/// `xp3filter.tjs` load a one-shot.
#[derive(Default)]
struct Xp3FilterState {
    /// The retained `setXP3ArchiveExtractionFilter` closure.
    extraction: Option<DetachedValue>,
    /// The retained `setXP3ArchiveContentFilter` closure.
    content: Option<DetachedValue>,
    /// Whether `xp3filter.tjs` has been read (or a filter was installed
    /// programmatically), so the deferred load never clobbers it.
    script_loaded: bool,
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the plugin's native surface on the VM.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_static_members(&NativeStaticMembers {
        class_name: "Storages",
        methods: vec![
            NativeMethodDef {
                name: "setXP3ArchiveExtractionFilter",
                f: set_extraction_filter,
            },
            NativeMethodDef {
                name: "setXP3ArchiveContentFilter",
                f: set_content_filter,
            },
        ],
        properties: vec![],
    })?;

    // The reference's `CBinaryAccessor` exposes an `xor` helper over the
    // buffer; attach the same helper to the marshalled `Array`.
    ctx.register_instance_members(&NativeInstanceMembers {
        class_name: "Array",
        methods: vec![NativeObjThisMethodDef {
            name: "xor",
            f: array_xor,
        }],
        properties: vec![],
    })?;

    install_storage_filter(ctx);

    // Eagerly load the script when the engine → context binding is already
    // live (unit-test and direct-registration harnesses). The host installs
    // the binding after registration, so the normal path defers the load to
    // the first archive read (see `ensure_script_loaded`).
    // SAFETY: registration runs on the bound, live engine.
    if binding::resolve(unsafe { ctx.engine() }.raw()).is_some() {
        // SAFETY: the caller holds the live services.
        let services = unsafe { ctx.services() };
        // SAFETY: the same bound, live engine.
        load_filter_script(unsafe { ctx.engine() }, services)
            .map_err(|error| CompatError::Register(format!("xp3_filter: {error}")))?;
        services.state::<Xp3FilterState>().script_loaded = true;
    }
    Ok(())
}

/// Install the L2 extraction-filter seam that dispatches to the retained TJS
/// closure.
///
/// The closure captures the stable addresses of the live engine and
/// [`CompatServices`] rather than the binding, so it can be installed during
/// registration before the binding exists.
fn install_storage_filter(ctx: &CompatCtx) {
    // SAFETY: registration runs on the bound, live engine.
    let engine = unsafe { ctx.engine() };
    let engine_ptr = engine as *const Tjs2Engine as usize;
    // SAFETY: the caller holds the live services.
    let services = unsafe { ctx.services() };
    let Some(storage) = services.storage() else {
        return;
    };
    let services_ptr = services as *const CompatServices as usize;
    let filter = Box::new(
        move |file_hash: u32,
              offset: u64,
              buffer: &mut [u8],
              file_name: &str,
              archive_name: &str,
              file_size: u64| {
            dispatch_extraction_filter(
                engine_ptr,
                services_ptr,
                FilterRequest {
                    file_hash,
                    offset,
                    buffer,
                    file_name,
                    archive_name,
                    file_size,
                },
            );
        },
    );
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    unsafe { &mut *storage }.xp3_extraction_filter = Some(filter);
}

/// Read `<app>/xp3filter.tjs` through the storage namespace and execute it.
///
/// The source is wrapped in a named function declaration, called, and the
/// wrapper deleted so top-level declarations stay local to the filter script,
/// matching the reference's dedicated sub-engine (`xp3filter.cpp:394-421`:
/// `ExecScript` runs on `ScriptEngine->GetGlobalNoAddRef()`). A function
/// *expression* wrapper cannot be used: the VM does not bind `function`
/// declarations inside an anonymous function body, so the script's helpers
/// would be unreachable.
///
/// A missing script is not an error (the reference only loads it when
/// present); any other failure is returned so registration can report it.
fn load_filter_script(engine: &Tjs2Engine, services: &CompatServices) -> Result<(), String> {
    let Some(storage) = services.storage() else {
        return Ok(());
    };
    // SAFETY: the host keeps the attached storage alive for the binding.
    let storage = unsafe { &*storage };
    let bytes = match storage.read(FILTER_SCRIPT) {
        Ok(bytes) => bytes,
        Err(StorageReadError::NotFound(_)) => return Ok(()),
        Err(error) => return Err(format!("could not read {FILTER_SCRIPT}: {error}")),
    };
    let text = crate::text::decode_text(&bytes, None)?;
    // A named declaration, unlike an anonymous function expression, binds the
    // script's top-level `function`/`var` names in its body. Delete the
    // wrapper afterwards so it does not become a game-visible global.
    let wrapped = format!(
        "function __krkr_rs_xp3_filter_script__() {{\n{text}\n}}\n\
         __krkr_rs_xp3_filter_script__();\n\
         delete __krkr_rs_xp3_filter_script__;"
    );
    engine
        .exec_script(&wrapped, FILTER_SCRIPT)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Deferred, one-shot `xp3filter.tjs` load used when registration ran before
/// the binding was installed.
fn ensure_script_loaded(engine: &Tjs2Engine, services: &CompatServices) {
    {
        let state = services.state::<Xp3FilterState>();
        if state.script_loaded {
            return;
        }
    }
    // Mark first so a recursive archive read cannot loop back into the load.
    services.state::<Xp3FilterState>().script_loaded = true;
    if let Err(error) = load_filter_script(engine, services) {
        log::warn!("xp3_filter: {error}");
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// One decoded XP3 segment handed to the installed filter, plus the metadata
/// the reference's `tTVPXP3ExtractionFilterInfo` carries.
struct FilterRequest<'a> {
    /// Adler32 name hash from the entry's `adlr` chunk.
    file_hash: u32,
    /// Uncompressed offset of `buffer[0]` within the entry.
    offset: u64,
    /// Decoded segment bytes, rewritten in place.
    buffer: &'a mut [u8],
    /// Entry name as stored in the archive.
    file_name: &'a str,
    /// Logical archive name the reader mounted.
    archive_name: &'a str,
    /// Entry's total uncompressed size.
    file_size: u64,
}

/// The L2 filter body: run the content filter (if any), marshal the segment,
/// call the retained extraction closure, then copy the result back.
///
/// The content filter runs whenever it is installed — even with no
/// extraction filter — because the reference invokes the two wrappers
/// independently (`xp3filter.cpp:465-491` for content, `:493-530` for
/// extraction).
fn dispatch_extraction_filter(engine_ptr: usize, services_ptr: usize, request: FilterRequest<'_>) {
    let FilterRequest {
        file_hash,
        offset,
        buffer,
        file_name,
        archive_name,
        file_size,
    } = request;
    // SAFETY: the captured addresses belong to the live, bound engine and
    // services (which the host keeps alive for the VM lifetime) and the VM is
    // single-threaded, so no other reference is live here.
    let engine = unsafe { &*(engine_ptr as *const Tjs2Engine) };
    let services = unsafe { &*(services_ptr as *const CompatServices) };

    ensure_script_loaded(engine, services);

    // Take the closures out for the duration of the call so a re-entrant
    // setter cannot panic the state `RefCell`; put them back afterwards
    // unless the setter replaced them.
    let (extraction, content) = {
        let mut state = services.state::<Xp3FilterState>();
        (state.extraction.take(), state.content.take())
    };

    let mut ctx_object = None;
    let ctx = match content.as_ref() {
        Some(content) => content_context(
            engine,
            content,
            file_name,
            archive_name,
            file_size,
            &mut ctx_object,
        ),
        None => TjsValue::Void,
    };
    if let Some(extraction) = extraction.as_ref()
        && let Err(error) = apply_extraction_filter(
            engine, extraction, file_hash, offset, buffer, file_name, ctx,
        )
    {
        log::warn!("xp3_filter: filter for `{file_name}` failed: {error}");
    }
    // `ctx_object` is consumed by the call; dropping it here releases it only
    // if the call never happened.
    drop(ctx_object);
    restore_filters(services, extraction, content);
}

/// Put the temporarily-taken closures back, without clobbering a replacement
/// the re-entrant call installed.
fn restore_filters(
    services: &CompatServices,
    extraction: Option<DetachedValue>,
    content: Option<DetachedValue>,
) {
    let mut state = services.state::<Xp3FilterState>();
    if state.extraction.is_none() {
        state.extraction = extraction;
    }
    if state.content.is_none() {
        state.content = content;
    }
}

/// Call the content filter and extract its `ctx` result.
///
/// The reference reads `[ret, ctx]` only when the filter returns an object;
/// `ret` is ignored because the L1 read already fetches the whole entry.
fn content_context(
    engine: &Tjs2Engine,
    content: &DetachedValue,
    file_name: &str,
    archive_name: &str,
    file_size: u64,
    object_out: &mut Option<DetachedValue>,
) -> TjsValue {
    let args = [
        TjsValue::String(file_name.to_string()),
        TjsValue::String(archive_name.to_string()),
        TjsValue::Integer(file_size as i64),
    ];
    let Ok(result) = engine.call_detached(content, &args) else {
        return TjsValue::Void;
    };
    if !matches!(result, TjsValue::Object) {
        return TjsValue::Void;
    }
    let Ok(array) = engine.retain_value_detached(&TjsValue::Object) else {
        return TjsValue::Void;
    };
    let _ = engine.member(array.raw_id(), "0");
    match engine.member(array.raw_id(), "1") {
        Ok(TjsValue::Object) => match engine.retain_value_detached(&TjsValue::Object) {
            Ok(object) => {
                let ctx = TjsValue::Retained(object.raw_id() as u64);
                *object_out = Some(object);
                ctx
            }
            Err(_) => TjsValue::Void,
        },
        Ok(other) => other,
        Err(_) => TjsValue::Void,
    }
}

/// Marshal `buffer` to a TJS `Array`, call `filter`, and copy the array back.
fn apply_extraction_filter(
    engine: &Tjs2Engine,
    filter: &DetachedValue,
    file_hash: u32,
    offset: u64,
    buffer: &mut [u8],
    file_name: &str,
    ctx: TjsValue,
) -> Result<(), String> {
    let array = match engine.eval_retained("[]", "xp3_filter") {
        Ok(RetainedValue::Object(array)) => array,
        Ok(RetainedValue::Value(_)) => {
            return Err("could not create the filter buffer".into());
        }
        Err(error) => return Err(error.to_string()),
    };
    for (index, byte) in buffer.iter().enumerate() {
        engine.set_member(
            array.raw_id(),
            &index.to_string(),
            &TjsValue::Integer(i64::from(*byte)),
        )?;
    }
    // Pass a duplicate retention: the call consumes the argument's retained
    // id, while `array` stays live for the read-back.
    let pass = engine.duplicate_retained(array.raw_id())?;
    let args = [
        TjsValue::Integer(i64::from(file_hash)),
        TjsValue::Integer(offset as i64),
        TjsValue::Retained(pass.raw_id() as u64),
        TjsValue::Integer(buffer.len() as i64),
        TjsValue::String(file_name.to_string()),
        ctx,
    ];
    let call = engine.call_detached(filter, &args);
    drop(pass);
    call?;
    for (index, byte) in buffer.iter_mut().enumerate() {
        match engine.member(array.raw_id(), &index.to_string())? {
            TjsValue::Integer(value) => *byte = value as u8,
            TjsValue::Real(value) => *byte = value as u8,
            _ => {}
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// `Storages` static members
// ---------------------------------------------------------------------------

extern "C" fn set_extraction_filter(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    set_filter(
        engine,
        argc,
        argv,
        out,
        out_error,
        "setXP3ArchiveExtractionFilter",
        true,
    )
}

extern "C" fn set_content_filter(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    set_filter(
        engine,
        argc,
        argv,
        out,
        out_error,
        "setXP3ArchiveContentFilter",
        false,
    )
}

/// Shared body of the two setters: retain the argument (or clear on
/// `null`) and store it in the per-family dispatch state.
fn set_filter(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    method: &str,
    extraction: bool,
) -> c_int {
    let Some(resolved) = binding::resolve(engine as *mut Engine) else {
        return report_error(
            out_error,
            &format!("Storages.{method}: compat context is not bound"),
        );
    };
    if argc < 1 {
        return report_error(out_error, &format!("Storages.{method} requires 1 argument"));
    }
    let ctx = resolved.ctx();
    // SAFETY: the caller resolved the live binding; `ctx` outlives `services`.
    let services = unsafe { ctx.services() };
    let value = unsafe { args(argv, argc) }.first();
    // Reference `XP3FilterRegister::FuncCall` (`xp3filter.cpp:353-361`):
    // `param[0]->AsObjectClosure()` keeps objects (including `null`, which
    // clears the installed closure) and raises the variant-convert error for
    // every other type (`reference/cpp/core/tjs2/tjsVariant.h:719-726`).
    let retained = match value {
        Some(value) if value.ty == VAL_OBJECT => match resolved.engine().retain_object_arg(value) {
            Ok(object) => Some(object),
            Err(error) => {
                return report_error(out_error, &format!("Storages.{method}: {error}"));
            }
        },
        // `null` clears the installed filter, matching the reference's
        // `TVPSetXP3ArchiveExtractionFilter(nullptr)` path (`xp3filter.cpp:538-545`).
        Some(value) if value.ty == tjs::VAL_NULL => None,
        Some(_) => {
            return report_error(
                out_error,
                &format!("Storages.{method}: the filter cannot be converted to an object"),
            );
        }
        None => None,
    };
    {
        let mut state = services.state::<Xp3FilterState>();
        if extraction {
            state.extraction = retained;
        } else {
            state.content = retained;
        }
        // A programmatically installed filter must not be clobbered by the
        // deferred `xp3filter.tjs` load.
        state.script_loaded = true;
    }
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// `Array.xor(offset, length, value)` — the reference `CBinaryAccessor::FuncXor`
// ---------------------------------------------------------------------------

extern "C" fn array_xor(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = binding::resolve(engine as *mut Engine) else {
        return report_error(out_error, "Array.xor: compat context is not bound");
    };
    let argv = unsafe { args(argv, argc) };
    if argv.len() < 3 {
        return report_error(out_error, "Array.xor requires 3 arguments");
    }
    let offset = value_as_i64(&argv[0]).max(0) as usize;
    let length = value_as_i64(&argv[1]).max(0) as usize;
    let key = value_as_i64(&argv[2]) as u8;
    let object = match resolved.engine().retain_object_detached(objthis) {
        Ok(object) => object,
        Err(error) => return report_error(out_error, &format!("Array.xor: {error}")),
    };
    let end = offset.saturating_add(length);
    for index in offset..end {
        let name = index.to_string();
        let current = match resolved.engine().member(object.raw_id(), &name) {
            Ok(TjsValue::Integer(value)) => value as u8,
            Ok(TjsValue::Real(value)) => value as u8,
            _ => 0,
        };
        if let Err(error) = resolved.engine().set_member(
            object.raw_id(),
            &name,
            &TjsValue::Integer(i64::from(current ^ key)),
        ) {
            return report_error(out_error, &format!("Array.xor: {error}"));
        }
    }
    set_void(out);
    0
}
