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
//!   filter runs once per archive entry, exactly like the reference's
//!   `TVPXP3ArchiveContentFilterWrapper` (`xp3filter.cpp:465-491`,
//!   `XP3Archive.cpp:563-572`), and it runs whether or not an extraction
//!   filter is installed.
//!
//! # Script load timing
//!
//! The reference reads `<app>/xp3filter.tjs` in a post-registration callback
//! and executes it in a **dedicated sub-engine** whose global scope holds only
//! `Debug`/`System`/`Storages` (`xp3filter.cpp:552-559`, `:394-421`), so
//! nothing the filter script declares is visible to the game script. The port
//! reads the source through the L2 storage at registration when the storage
//! is already attached (a read failure is returned to
//! [`CompatHost`](crate::CompatHost) as a registration error); when the
//! composition root attaches the storage after registration, the read and the
//! seam install are completed at
//! [`CompatHost::begin_native_timing`](crate::CompatHost), which runs before
//! the startup sequence. That completion reports the not-yet-attached storage
//! as its own state (`ScriptLookup::StorageNotAttached` /
//! `SeamInstall::StorageNotAttached`), never as an absent script. The script
//! executes on the first archive read, once
//! the engine → context binding the setters need is live. The shared engine
//! cannot expose a second global, so the
//! source is wrapped in a named local function: its declarations stay local to
//! the filter script — the same isolation the sub-engine provides for
//! top-level `function`/`var`. A function *expression* cannot host the
//! wrapper: TJS2 does not bind a `function` declaration inside an anonymous
//! function body (the name resolves neither locally nor on `global`).
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
//! as a TJS `Array` of byte values and attaches the reference `ptr`/`count`
//! members and `xor` helper as own members of that object, calls the closure,
//! then copies the array back into the segment. The closure receives the
//! reference's argument list:
//! `(file_hash, offset, buffer, buffer_size, file_name, ctx)`. The `Array`'s
//! native indexed access already applies the reference's indexed operators
//! (`^=`, `+=`, ...) to individual bytes; the one surface the port cannot
//! reproduce is the `CBinaryAccessor` cursor being added to the index inside
//! a native `[]` access (`PropGetByNum`/`OperationByNum`, `xp3filter.cpp:
//! 162-197`), which would require a custom dispatch method the C ABI does
//! not expose.
//!
//! The content filter is invoked once per archive entry read before the
//! extraction filter; its `[ret, ctx]` return value contributes `ctx` (the
//! `ret` flag is ignored because the L1 read is always eager, which is what
//! `ret == 1` would have forced).
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use engine::StorageReadError;
use tjs::{
    DetachedValue, Engine, NativeMethodDef, NativeStaticMembers, RetainedValue, Tjs2Engine,
    TjsValue, VAL_OBJECT, Value,
};

use crate::binding;
use crate::native_util::{args, report_error, set_void};
use crate::{CompatCtx, CompatError, CompatServices};

/// The configuration script the reference reads from the application path.
const FILTER_SCRIPT: &str = "xp3filter.tjs";

/// The script `xor(offset, length, value)` helper attached to every
/// marshalled buffer, matching the reference `CBinaryAccessor::FuncXor`
/// (`reference/cpp/plugins/xp3filter.cpp:266-297`). Attaching it to the
/// per-call buffer object keeps the method off the core `Array` class. The
/// reference adds the buffer's `ptr` cursor to `offset` (`:258-260`), so the
/// helper reads `this.ptr`.
const BUFFER_XOR_HELPER: &str = "(function(offset, length, value) { \
     var base = this.ptr; \
     if (base === void) base = 0; \
     for (var i = 0; i < length; i++) this[base + offset + i] = this[base + offset + i] ^ value; \
     })";

/// Per-install XP3 filter dispatch state, kept in
/// [`CompatServices::state`](crate::CompatServices::state).
///
/// The retained closures are `DetachedValue`s so the plugin never holds a
/// borrow of the engine; the `script_loaded` flag makes the deferred
/// `xp3filter.tjs` execution a one-shot.
#[derive(Default)]
struct Xp3FilterState {
    /// The retained `setXP3ArchiveExtractionFilter` closure.
    extraction: Option<DetachedValue>,
    /// The retained `setXP3ArchiveContentFilter` closure.
    content: Option<DetachedValue>,
    /// The retained buffer `xor` helper attached to each marshalled segment
    /// (`reference/cpp/plugins/xp3filter.cpp:266-297`).
    buffer_xor: Option<DetachedValue>,
    /// Whether the L2 extraction-filter seam is installed on the attached
    /// storage.
    seam_installed: bool,
    /// Whether the `xp3filter.tjs` file read has run (once the L2 storage is
    /// attached).
    script_read: bool,
    /// The `xp3filter.tjs` source, awaiting execution on the first archive
    /// read (the reference reads in the post-registration callback and
    /// executes in the decoder on first use,
    /// `reference/cpp/plugins/xp3filter.cpp:552-559`, `:418`).
    pending_script: Option<String>,
    /// Whether `xp3filter.tjs` has been executed (or a filter was installed
    /// programmatically), so the deferred execution never clobbers it.
    script_loaded: bool,
    /// The content filter's context for the entry currently being decoded.
    /// The reference calls the content filter once per entry
    /// (`reference/cpp/core/archive/xp3/XP3Archive.cpp:563-565`) and passes
    /// the stored `ctx` to every extraction-filter chunk.
    entry: Option<EntryContext>,
}

/// The per-entry result of the content filter, kept for every decoded
/// segment of the entry.
struct EntryContext {
    /// Entry name the context was built for.
    file_name: String,
    /// Logical archive name the context was built for.
    archive_name: String,
    /// Entry's total uncompressed size.
    file_size: u64,
    /// The scalar `ctx` value when the content filter returned a non-object
    /// (or no filter is installed).
    value: TjsValue,
    /// A retained object `ctx`, kept alive while the entry is decoded.
    owner: Option<DetachedValue>,
    /// Whether the last decoded segment ended the entry.
    complete: bool,
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

    // SAFETY: registration runs on the bound, live engine.
    let engine = unsafe { ctx.engine() };
    // SAFETY: the caller holds the live services.
    let services = unsafe { ctx.services() };

    // The reference's `CBinaryAccessor` carries the `xor` helper; the port
    // retains a script helper once and attaches it to each marshalled buffer
    // as an own member (`reference/cpp/plugins/xp3filter.cpp:266-297`).
    match engine.eval_retained(BUFFER_XOR_HELPER, "xp3_filter") {
        Ok(RetainedValue::Object(helper)) => {
            services.state::<Xp3FilterState>().buffer_xor = Some(helper);
        }
        Ok(RetainedValue::Value(other)) => {
            return Err(CompatError::Register(format!(
                "xp3_filter: the buffer xor helper is not a function: {other:?}"
            )));
        }
        Err(error) => {
            return Err(CompatError::Register(format!(
                "xp3_filter: could not create the buffer xor helper: {error}"
            )));
        }
    }

    // The reference reads `xp3filter.tjs` from the application path in its
    // post-registration callback (`reference/cpp/plugins/xp3filter.cpp:550-564`).
    // The L2 storage is that path here; when it is attached this installs the
    // seam and reads the script now.
    activate(engine, services)?;

    // A harness that registers with the binding already live can execute the
    // pending script immediately; otherwise the first archive read executes it.
    if binding::resolve(engine.raw()).is_some() {
        execute_pending_script(engine, services)
            .map_err(|error| CompatError::Register(format!("xp3_filter: {error}")))?;
    }
    Ok(())
}

/// The outcome of looking for `xp3filter.tjs`.
///
/// The not-yet-attached state is kept distinct from a genuine absence: a
/// caller must not report "no script" when no read was possible.
enum ScriptLookup {
    /// The L2 storage is not attached yet, so no read was attempted.
    StorageNotAttached,
    /// The storage was searched and holds no `xp3filter.tjs`.
    Absent,
    /// The storage returned the script source.
    Found(String),
}

/// The outcome of installing the L2 extraction-filter seam.
enum SeamInstall {
    /// The seam is installed (or was already installed).
    Installed,
    /// The L2 storage is not attached yet, so there is nothing to install on.
    StorageNotAttached,
}

/// Install the extraction-filter seam and read `xp3filter.tjs` once the L2
/// storage is attached.
///
/// Idempotent, and reports "the storage is not attached yet" as its own state
/// rather than folding it into "no script". The reference does this in its
/// post-registration callback before the game runs
/// (`reference/cpp/plugins/xp3filter.cpp:550-564`); a composition root that
/// attaches the storage after [`register`] completes it at
/// [`CompatHost::begin_native_timing`](crate::CompatHost).
pub fn activate(engine: &Tjs2Engine, services: &CompatServices) -> Result<(), CompatError> {
    if !services.state::<Xp3FilterState>().script_read {
        match read_filter_script(services) {
            Ok(ScriptLookup::StorageNotAttached) => {
                log::debug!(
                    "xp3_filter: deferred: L2 storage not attached yet; {FILTER_SCRIPT} not looked for"
                );
                return Ok(());
            }
            Ok(ScriptLookup::Found(text)) => {
                log::info!(
                    "xp3_filter: {FILTER_SCRIPT} loaded ({} bytes); archive content is filtered",
                    text.len()
                );
                services.state::<Xp3FilterState>().pending_script = Some(text);
                services.state::<Xp3FilterState>().script_read = true;
            }
            Ok(ScriptLookup::Absent) => {
                log::info!(
                    "xp3_filter: {FILTER_SCRIPT} not found; archive content is not filtered"
                );
                services.state::<Xp3FilterState>().script_read = true;
            }
            Err(error) => return Err(CompatError::Register(format!("xp3_filter: {error}"))),
        }
    }

    match install_storage_filter(engine, services) {
        SeamInstall::Installed => {}
        SeamInstall::StorageNotAttached => {
            log::debug!(
                "xp3_filter: deferred: L2 storage not attached; extraction-filter seam not installed"
            );
        }
    }
    Ok(())
}

/// Install the L2 extraction-filter seam that dispatches to the retained TJS
/// closure.
///
/// The closure captures the stable addresses of the live engine and
/// [`CompatServices`] rather than the binding, so it can be installed during
/// registration before the binding exists. A missing storage is reported, not
/// swallowed.
fn install_storage_filter(engine: &Tjs2Engine, services: &CompatServices) -> SeamInstall {
    let Some(storage) = services.storage() else {
        return SeamInstall::StorageNotAttached;
    };
    if services.state::<Xp3FilterState>().seam_installed {
        return SeamInstall::Installed;
    }
    let engine_ptr = engine as *const Tjs2Engine as usize;
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
    services.state::<Xp3FilterState>().seam_installed = true;
    log::debug!("xp3_filter: XP3 extraction-filter seam installed");
    SeamInstall::Installed
}

/// Read and decode `<app>/xp3filter.tjs` through the storage namespace.
///
/// A missing script is not an error (the reference only loads it when
/// present); any other read failure is returned so registration can report it
/// (`reference/cpp/plugins/xp3filter.cpp:553-559`). The not-yet-attached
/// storage is a third, distinct state.
fn read_filter_script(services: &CompatServices) -> Result<ScriptLookup, String> {
    let Some(storage) = services.storage() else {
        return Ok(ScriptLookup::StorageNotAttached);
    };
    // SAFETY: the host keeps the attached storage alive for the binding.
    let storage = unsafe { &*storage };
    let bytes = match storage.read(FILTER_SCRIPT) {
        Ok(bytes) => bytes,
        Err(StorageReadError::NotFound(_)) => return Ok(ScriptLookup::Absent),
        Err(error) => return Err(format!("could not read {FILTER_SCRIPT}: {error}")),
    };
    crate::text::decode_text(&bytes, crate::text::TextEncoding::Detect).map(ScriptLookup::Found)
}

/// Execute the `xp3filter.tjs` source read once the storage is attached.
///
/// The source is wrapped in a named function declaration, called, and the
/// wrapper deleted so top-level declarations stay local to the filter script,
/// matching the reference's dedicated sub-engine (`xp3filter.cpp:394-421`:
/// `ExecScript` runs on `ScriptEngine->GetGlobalNoAddRef()`). A function
/// *expression* wrapper cannot be used: the VM does not bind `function`
/// declarations inside an anonymous function body, so the script's helpers
/// would be unreachable.
fn execute_filter_script(engine: &Tjs2Engine, text: &str) -> Result<(), String> {
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

/// Execute the script read (at registration or deferred activation), exactly
/// once. A `None` means the application shipped no `xp3filter.tjs` (the
/// reference only loads it when present).
fn execute_pending_script(engine: &Tjs2Engine, services: &CompatServices) -> Result<(), String> {
    // Take the source out before matching: a `match` scrutinee temporary
    // lives for the whole match, so binding it first drops the state borrow
    // before the script runs and may re-enter a setter.
    let pending = services.state::<Xp3FilterState>().pending_script.take();
    match pending {
        Some(text) => execute_filter_script(engine, &text),
        None => Ok(()),
    }
}

/// Deferred, one-shot `xp3filter.tjs` execution triggered by the first archive
/// read (the engine → context binding is live by then).
fn ensure_script_loaded(engine: &Tjs2Engine, services: &CompatServices) {
    {
        let state = services.state::<Xp3FilterState>();
        if state.script_loaded {
            return;
        }
    }
    // Mark first so a recursive archive read cannot loop back into the load.
    services.state::<Xp3FilterState>().script_loaded = true;
    if let Err(error) = execute_pending_script(engine, services) {
        // The reference executes the script in `AddXP3Decoder` on the first
        // read; the L2 filter callback has no error channel, so an execution
        // failure is reported here.
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

    // Take the closures and the cached entry out for the duration of the call
    // so a re-entrant setter cannot panic the state `RefCell`; put them back
    // afterwards unless the setter replaced them.
    let (extraction, content, mut entry) = {
        let mut state = services.state::<Xp3FilterState>();
        (
            state.extraction.take(),
            state.content.take(),
            state.entry.take(),
        )
    };

    // The content filter runs once, when the reference opens the entry's
    // stream (`reference/cpp/core/archive/xp3/XP3Archive.cpp:563-565`), so a
    // fresh context is built only at the first segment of an entry.
    let start_new = entry.as_ref().is_none_or(|entry| {
        offset == 0
            && (entry.complete
                || entry.file_name != file_name
                || entry.archive_name != archive_name
                || entry.file_size != file_size)
    });
    if start_new {
        entry = Some(build_entry_context(
            engine,
            content.as_ref(),
            file_name,
            archive_name,
            file_size,
        ));
    }
    if let Some(entry) = entry.as_mut() {
        entry.complete = offset.saturating_add(buffer.len() as u64) >= file_size;
    }
    if let Some(extraction) = extraction.as_ref()
        && let Some(entry) = entry.as_ref()
        && let Err(error) = apply_extraction_filter(
            engine,
            services,
            extraction,
            FilterCall {
                file_hash,
                offset,
                file_name,
            },
            buffer,
            entry,
        )
    {
        log::warn!("xp3_filter: filter for `{file_name}` failed: {error}");
    }
    // Put the temporarily-taken closures and the entry back, without
    // clobbering a replacement the re-entrant call installed.
    let mut state = services.state::<Xp3FilterState>();
    if state.extraction.is_none() {
        state.extraction = extraction;
    }
    if state.content.is_none() {
        state.content = content;
    }
    if state.entry.is_none() {
        state.entry = entry;
    }
}

/// Call the content filter once for a freshly opened entry and extract its
/// `ctx` result.
///
/// The reference reads `[ret, ctx]` only when the filter returns an object;
/// `ret` is ignored because the L1 read already fetches the whole entry
/// (`reference/cpp/plugins/xp3filter.cpp:480-490`).
fn build_entry_context(
    engine: &Tjs2Engine,
    content: Option<&DetachedValue>,
    file_name: &str,
    archive_name: &str,
    file_size: u64,
) -> EntryContext {
    let mut context = EntryContext {
        file_name: file_name.to_string(),
        archive_name: archive_name.to_string(),
        file_size,
        value: TjsValue::Void,
        owner: None,
        complete: false,
    };
    let Some(content) = content else {
        return context;
    };
    let args = [
        TjsValue::String(file_name.to_string()),
        TjsValue::String(archive_name.to_string()),
        TjsValue::Integer(file_size as i64),
    ];
    let Ok(result) = engine.call_detached(content, &args) else {
        log::warn!("xp3_filter: content filter for `{file_name}` raised");
        return context;
    };
    if !matches!(result, TjsValue::Object) {
        return context;
    }
    let Ok(array) = engine.retain_value_detached(&TjsValue::Object) else {
        log::warn!("xp3_filter: content filter for `{file_name}` returned an unreadable object");
        return context;
    };
    match engine.member(array.raw_id(), "1") {
        Ok(TjsValue::Object) => {
            if let Ok(object) = engine.retain_value_detached(&TjsValue::Object) {
                context.owner = Some(object);
            } else {
                log::warn!("xp3_filter: content filter ctx object could not be retained");
            }
        }
        Ok(other) => context.value = other,
        Err(error) => log::warn!("xp3_filter: content filter ctx read failed: {error}"),
    }
    context
}

/// The archive position and name an extraction filter call receives
/// (`reference/cpp/plugins/xp3filter.cpp:266-297`).
struct FilterCall<'a> {
    file_hash: u32,
    offset: u64,
    file_name: &'a str,
}

/// Marshal `buffer` to a TJS `Array`, attach the reference buffer surface,
/// call `filter`, and copy the array back.
fn apply_extraction_filter(
    engine: &Tjs2Engine,
    services: &CompatServices,
    filter: &DetachedValue,
    call: FilterCall<'_>,
    buffer: &mut [u8],
    entry: &EntryContext,
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
    // The reference `CBinaryAccessor` publishes `ptr` (a writable cursor) and
    // `count` (`reference/cpp/plugins/xp3filter.cpp:178-214`). The `Array`'s
    // own `count` already reports the number of marshalled bytes; `ptr`
    // becomes an own member so the filter script can move the cursor.
    engine.set_member(array.raw_id(), "ptr", &TjsValue::Integer(0))?;
    // Attach the reference `CBinaryAccessor::xor` helper as an own member of
    // the marshalled buffer (`reference/cpp/plugins/xp3filter.cpp:266-297`),
    // so the filter script can call `buffer.xor(offset, length, value)`
    // without a member on the `Array` class.
    let helper_id = services
        .state::<Xp3FilterState>()
        .buffer_xor
        .as_ref()
        .map(DetachedValue::raw_id);
    let Some(helper_id) = helper_id else {
        return Err("the buffer xor helper is not initialized".into());
    };
    let helper = engine.duplicate_retained(helper_id)?;
    engine.set_member(
        array.raw_id(),
        "xor",
        &TjsValue::Retained(helper.raw_id() as u64),
    )?;
    // A retained `ctx` object is consumed by the call, so duplicate it into a
    // fresh retention for every segment; a scalar `ctx` passes by value.
    let ctx_dup = match entry.owner.as_ref() {
        Some(owner) => Some(engine.duplicate_retained(owner.raw_id())?),
        None => None,
    };
    let ctx = match ctx_dup.as_ref() {
        Some(dup) => TjsValue::Retained(dup.raw_id() as u64),
        None => entry.value.clone(),
    };
    // Pass a duplicate retention: the call consumes the argument's retained
    // id, while `array` stays live for the read-back.
    let pass = engine.duplicate_retained(array.raw_id())?;
    let args = [
        TjsValue::Integer(i64::from(call.file_hash)),
        TjsValue::Integer(call.offset as i64),
        TjsValue::Retained(pass.raw_id() as u64),
        TjsValue::Integer(buffer.len() as i64),
        TjsValue::String(call.file_name.to_string()),
        ctx,
    ];
    let call = engine.call_detached(filter, &args);
    drop(pass);
    drop(ctx_dup);
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
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

#[cfg(test)]
mod tests {
    use super::*;

    // A storage that is not attached yet must be its own state, never folded
    // into "the script is absent": otherwise a caller cannot tell a genuine
    // miss from a read that was not yet possible.
    #[test]
    fn no_storage_is_not_the_same_as_an_absent_script() {
        let services = CompatServices::new();
        assert!(matches!(
            read_filter_script(&services),
            Ok(ScriptLookup::StorageNotAttached)
        ));
    }
}
