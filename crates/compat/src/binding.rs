//! The one process-wide engine → context binding.
//!
//! The C++ ABI hands native callbacks only the raw `tjs2_engine*`, and
//! `tjs` exposes **no user-data slot**, so this table keeps exactly **one**
//! process-wide binding and documents it.
//!
//! It is L3 (never L1), it holds *dispatch* state (the Rust [`Tjs2Engine`],
//! the L2 [`CompatServices`] and the [`PluginRegistry`]) and **never game
//! state**. The host installs it once, before any native callback can run,
//! and clears it before dropping the engine; the threaded services it points
//! at stay alive for the whole VM lifetime.
//!
//! The table stores plain `usize` addresses so it needs no `unsafe impl
//! `Send`/`Sync`: the addresses are only dereferenced on the single VM thread.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};
use std::ptr;
use std::sync::{LazyLock, Mutex, MutexGuard};

use tjs::{Engine, Tjs2Engine};

use crate::engine_api::StorageReadError;
use crate::storage::strip_file_media;
use crate::{CompatCtx, CompatError, CompatServices, PluginRegistry};

struct Binding {
    /// The C++ engine pointer native callbacks receive (`Tjs2Engine::raw`).
    engine_raw: usize,
    /// The address of the Rust host engine (for safe method calls).
    engine: usize,
    /// The address of the host-owned L2 services.
    services: usize,
    /// The address of the host-owned plugin registry.
    registry: usize,
}

static BINDING: LazyLock<Mutex<Option<Binding>>> = LazyLock::new(|| Mutex::new(None));

fn lock() -> MutexGuard<'static, Option<Binding>> {
    BINDING.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// A resolved binding, copied out of the table so no lock is held while a
/// native callback runs.
#[derive(Clone, Copy)]
pub(crate) struct Resolved {
    engine: usize,
    services: usize,
    registry: usize,
}

impl Resolved {
    /// The Rust host engine.
    pub(crate) fn engine(&self) -> &'static Tjs2Engine {
        // SAFETY: install only stores the address of a live host engine that
        // outlives the binding.
        unsafe { &*(self.engine as *const Tjs2Engine) }
    }

    /// The host-owned plugin registry.
    pub(crate) fn registry(&self) -> &'static PluginRegistry {
        // SAFETY: install only stores the address of a live registry that
        // outlives the binding.
        unsafe { &*(self.registry as *const PluginRegistry) }
    }

    /// A context handle for registration and native dispatch.
    pub(crate) fn ctx(&self) -> CompatCtx {
        // SAFETY: `Resolved` is only produced by `resolve` for the installed
        // binding, whose engine and services outlive the binding.
        unsafe { CompatCtx::from_raw(self.engine, self.services) }
    }
}

/// Install the process-wide binding.
///
/// Fails if a binding is already installed: there is exactly one VM.
pub(crate) fn install(
    engine: &Tjs2Engine,
    services: *mut CompatServices,
    registry: &PluginRegistry,
) -> Result<(), CompatError> {
    {
        let mut slot = lock();
        if slot.is_some() {
            return Err(CompatError::Register(
                "compat binding already installed".into(),
            ));
        }
        *slot = Some(Binding {
            engine_raw: engine.raw() as usize,
            engine: engine as *const Tjs2Engine as usize,
            services: services as usize,
            registry: registry as *const PluginRegistry as usize,
        });
    }
    // The VM's stream factories resolve storage names (`arc.xp3>member`)
    // through this seam. SAFETY: `services` is the boxed host state recorded
    // by the binding and outlives it; `uninstall` clears the seam first.
    unsafe { tjs::set_storage_read_callback(Some(storage_read), services as *mut c_void) };
    Ok(())
}

/// Remove the binding (called before the engine is dropped).
pub(crate) fn uninstall() {
    // SAFETY: clearing the seam passes no user pointer.
    unsafe { tjs::set_storage_read_callback(None, ptr::null_mut()) };
    *lock() = None;
}

/// Resolve the binding for the C++ engine pointer a callback received.
pub(crate) fn resolve(engine: *mut Engine) -> Option<Resolved> {
    let slot = lock();
    slot.as_ref().and_then(|binding| {
        if binding.engine_raw == engine as usize {
            Some(Resolved {
                engine: binding.engine,
                services: binding.services,
                registry: binding.registry,
            })
        } else {
            None
        }
    })
}

// ---------------------------------------------------------------------------
// Storage seam for the TJS text/binary stream factories
// ---------------------------------------------------------------------------

/// Stores a malloc'd UTF-8 message in `out_error` and returns the storage
/// callback's error status.
fn storage_failure(out_error: *mut *mut c_char, message: &str) -> c_int {
    if !out_error.is_null() {
        let bytes = message.as_bytes();
        // SAFETY: tjs2_malloc is malloc-compatible; the stream layer frees the
        // NUL-terminated copy with tjs2_free_string.
        unsafe {
            let buf = tjs::tjs2_malloc(bytes.len() + 1) as *mut u8;
            if !buf.is_null() {
                ptr::copy_nonoverlapping(bytes.as_ptr(), buf, bytes.len());
                *buf.add(bytes.len()) = 0;
                *out_error = buf as *mut c_char;
            }
        }
    }
    tjs::STORAGE_READ_ERROR
}

/// The storage read the text/binary stream factories call.
///
/// Resolves the raw name through the attached L2 storage (disk and
/// `archive.xp3>member`). A name the storage does not have answers
/// [`tjs::STORAGE_READ_NOT_FOUND`], so the factory falls back to the local
/// filesystem; any other storage error is reported with its message.
extern "C" fn storage_read(
    user: *mut c_void,
    name: *const c_char,
    name_len: usize,
    out_data: *mut *mut u8,
    out_len: *mut usize,
    out_error: *mut *mut c_char,
) -> c_int {
    if out_data.is_null() || out_len.is_null() {
        return storage_failure(out_error, "storage read: null result slot");
    }
    // SAFETY: the ABI passes both result slots writable for the call.
    unsafe {
        *out_data = ptr::null_mut();
        *out_len = 0;
    }
    if user.is_null() {
        return storage_failure(out_error, "storage read: no compat services");
    }
    if name.is_null() && name_len != 0 {
        return storage_failure(out_error, "storage read: null name");
    }
    // SAFETY: name/name_len are a valid byte range for the duration of the
    // call.
    let raw: &[u8] = unsafe {
        if name_len == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(name as *const u8, name_len)
        }
    };
    let Ok(name) = std::str::from_utf8(raw) else {
        return storage_failure(out_error, "storage read: name is not UTF-8");
    };
    // SAFETY: `user` is the boxed `CompatServices` recorded at install, and
    // the seam is cleared before the host can replace or drop it.
    let services = unsafe { &*(user as *const CompatServices) };
    let Some(storage) = services.storage() else {
        // No storage attached: let the factory use the local filesystem.
        return tjs::STORAGE_READ_NOT_FOUND;
    };
    // SAFETY: `attach_storage` requires the owner to outlive the binding.
    let storage = unsafe { &*storage };
    match storage.read(strip_file_media(name)) {
        Ok(bytes) => {
            let len = bytes.len();
            // SAFETY: tjs2_malloc is malloc-compatible; the stream layer frees
            // the buffer with tjs2_free_storage_bytes.
            let data = unsafe { tjs::tjs2_malloc(len.max(1)) as *mut u8 };
            if data.is_null() {
                return storage_failure(out_error, "storage read: out of memory");
            }
            // SAFETY: the allocation holds at least `len` bytes.
            unsafe {
                if len > 0 {
                    ptr::copy_nonoverlapping(bytes.as_ptr(), data, len);
                }
                *out_data = data;
                *out_len = len;
            }
            tjs::STORAGE_READ_OK
        }
        Err(StorageReadError::NotFound(_)) => tjs::STORAGE_READ_NOT_FOUND,
        Err(error) => storage_failure(out_error, &error.to_string()),
    }
}
