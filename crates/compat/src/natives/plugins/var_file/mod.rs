//! `varfile.dll` — the `var://` script-variable storage media.
//!
//! The plugin's entire surface is one storage media: a `var://` scheme whose
//! "files" are the members of the script object graph — a member that holds
//! an `Octet` is a file, a member that holds an object is a directory. It
//! registers **no** `Storages` members — the file-manipulation members the
//! core `Storages` class exposes come from the `fstat` plugin and are owned
//! by [`super::fstat`].
//!
//! # The media
//!
//! The media is served by [`VarMedia`], a storage-media provider the L2
//! namespace dispatches a `var://` name to. The name after the scheme is
//! `./<member>/<member>/…/<file>`: the domain is `.` — the script global
//! object — and the path names the members below it. The members are read
//! through `PropGet`, so a property's getter runs and an array's numeric
//! member is the element it names.
//!
//! The three failures keep their own wording: `no such domain:%1` for a
//! domain that is not `.`, `invalid path:%1` for a path without a member
//! separator and `cannot open memfile:%1` for an entry that is not a file.
#![allow(unsafe_code)]

use std::thread::ThreadId;

use tjs::{MemberRead, RetainedValue, Tjs2Engine};

use crate::engine_api::{StorageMedia, StorageReadError};
use crate::{CompatCtx, CompatError};

/// The provider that serves the `var://` media.
struct VarMedia {
    /// The VM whose global object the media walks.
    engine: *const Tjs2Engine,
    /// The thread the provider was built on.
    ///
    /// The media walks a live script object graph, so it is read on the
    /// thread that owns the VM.
    thread: ThreadId,
}

impl VarMedia {
    /// Build the provider for the VM `engine`.
    fn new(engine: *const Tjs2Engine) -> Self {
        Self {
            engine,
            thread: std::thread::current().id(),
        }
    }

    /// The VM the media walks.
    ///
    /// # Safety
    ///
    /// The engine must be alive. The provider is built from the bound VM and
    /// is read while that VM runs.
    fn engine(&self) -> &Tjs2Engine {
        // SAFETY: the provider is built from the bound VM and is only read
        // while that VM is alive.
        unsafe { &*self.engine }
    }

    /// The media's own failure.
    fn error(&self, message: String) -> StorageReadError {
        StorageReadError::Media {
            media: "var".to_string(),
            message,
        }
    }

    /// The octet the path names, when it names one.
    ///
    /// `path` is the storage name with its scheme removed: `./<member>/…`.
    fn resolve(&self, path: &str) -> Result<Option<Vec<u8>>, StorageReadError> {
        if std::thread::current().id() != self.thread {
            return Err(self.error(
                "varfile: the media was read from a thread that does not own the VM".to_string(),
            ));
        }
        let Some((domain, rest)) = path.split_once('/') else {
            return Err(self.error(format!("invalid path:{path}")));
        };
        if domain != "." {
            return Err(self.error(format!("no such domain:{domain}")));
        }
        let segments: Vec<&str> = rest.split('/').collect();
        let Some((file, parents)) = segments.split_last() else {
            return Err(self.error(format!("invalid path:{path}")));
        };
        if file.is_empty() {
            return Ok(None);
        }
        let engine = self.engine();
        let global = match engine.eval_retained("global", "varfile") {
            Ok(RetainedValue::Object(object)) => object,
            Ok(RetainedValue::Value(value)) => {
                return Err(self.error(format!(
                    "varfile: the global object is not an object ({value:?})"
                )));
            }
            Err(error) => return Err(self.error(format!("varfile: {error}"))),
        };
        let mut current = global;
        for segment in parents {
            match engine.member_read(current.raw_id(), segment) {
                Ok(MemberRead::Object(object)) => current = object,
                // A member that is not an object is not a directory, so the
                // path names nothing.
                Ok(_) => return Ok(None),
                Err(error) => return Err(self.error(format!("varfile: {error}"))),
            }
        }
        match engine.member_read(current.raw_id(), file) {
            Ok(MemberRead::Octet(bytes)) => Ok(Some(bytes)),
            // A file is an octet; every other shape is not one.
            Ok(_) => Ok(None),
            Err(error) => Err(self.error(format!("varfile: {error}"))),
        }
    }
}

impl StorageMedia for VarMedia {
    fn domain(&self) -> &str {
        "var"
    }

    fn exists(&self, path: &str) -> Result<bool, StorageReadError> {
        Ok(self.resolve(path)?.is_some())
    }

    fn read(&self, path: &str) -> Result<Vec<u8>, StorageReadError> {
        match self.resolve(path)? {
            Some(bytes) => Ok(bytes),
            None => Err(self.error(format!("cannot open memfile:var://{path}"))),
        }
    }
}

/// Register the `var://` media with the attached storage.
///
/// The media is registered when the plugin is linked, because the media needs
/// the storage namespace it serves.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    // SAFETY: the caller resolves `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let Some(storage) = services.storage() else {
        log::warn!("varfile: no storage is attached; the var:// media is not served");
        return Ok(());
    };
    // SAFETY: the ctx is only created for a bound, live engine.
    let engine = unsafe { ctx.engine() } as *const Tjs2Engine;
    // SAFETY: the host keeps the attached storage alive for the binding.
    let storage = unsafe { &*storage };
    storage.register_media(Box::new(VarMedia::new(engine)));
    Ok(())
}
