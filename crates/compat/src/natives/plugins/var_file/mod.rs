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
//! # The published containers
//!
//! A container the script loaded can publish its resources to the namespace
//! (see [`publish`]). The media then answers the read from the container
//! itself: the bytes come from the container, and the script object graph is
//! not touched at all.
//!
//! The packed images a title draws are read this way. A tile read goes
//! through the container's `root` member, and reaching the tile through the
//! script object graph rebuilds the container's whole root dictionary and
//! copies every resource of the container into a fresh octet, once per tile.
//! A published container answers the tile's bytes alone, so a tile read costs
//! the tile and not the container.
//!
//! A published container is named by the path below the global member that
//! reaches it (`<container>/root` for a container whose resources the script
//! reads through its `root` member), and the media looks that path up before
//! it walks the script object graph. A path that names no published
//! container is read from the script object graph.
//!
//! The two failures keep their own wording: `no such domain:%1` for a
//! domain that is not `.` and `invalid path:%1` for a path without a member
//! separator. An entry that is not a file is a missing storage: the member
//! is not there, so the caller decides what a missing member means.
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::rc::Rc;
use std::thread::ThreadId;

use tjs::{MemberRead, Tjs2Engine};

use crate::collections::Map;
use crate::engine_api::{StorageMedia, StorageReadError};
use crate::{CompatCtx, CompatError, CompatServices};

// ---------------------------------------------------------------------------
// The published containers
// ---------------------------------------------------------------------------

/// A container published to the `var://` namespace.
///
/// The container answers for the members the script sees under the path it
/// was published under: a member that names a file yields its bytes, and
/// every other member yields nothing.
pub(crate) trait VarContainer {
    /// The bytes of the member `name`.
    ///
    /// `Ok(None)` — the member is not a file (it is absent, or it holds a
    /// value or a nested container). `Err(message)` — the container holds
    /// the member but cannot answer for it; the message travels to the
    /// caller unchanged.
    fn file(&self, name: &str) -> Result<Option<Vec<u8>>, String>;
}

/// The containers published to the `var://` namespace.
///
/// A container is published under the path below the global member that
/// reaches it. The media looks the path up before it walks the script
/// object graph, so a published container is read without the VM.
#[derive(Default)]
pub(crate) struct VarRegistry {
    /// The published containers, by their path.
    containers: RefCell<Map<String, Rc<dyn VarContainer>>>,
}

impl VarRegistry {
    /// Publish a container under `path`.
    ///
    /// A later publication under the same path replaces the earlier one.
    pub(crate) fn publish(&self, path: &str, container: Rc<dyn VarContainer>) {
        self.containers
            .borrow_mut()
            .insert(path.to_string(), container);
    }

    /// The container the path names, and the member the path names in it.
    ///
    /// The path is the storage name with its scheme removed:
    /// `./<global member>/<container path>/<file>`. The container is the
    /// path below the global member, without the file; `None` when the
    /// media holds no container under that path.
    fn lookup<'a>(&self, path: &'a str) -> Option<(Rc<dyn VarContainer>, &'a str)> {
        let mut segments = path.split('/');
        // The first segment names a member of the script global object; the
        // published containers are named below it.
        let _global = segments.next()?;
        let rest: Vec<&str> = segments.collect();
        let (file, container) = rest.split_last()?;
        let container = container.join("/");
        if container.is_empty() {
            return None;
        }
        let containers = self.containers.borrow();
        let container = containers.get(container.as_str())?;
        Some((Rc::clone(container), file))
    }
}

/// The L3 state that owns the published containers.
///
/// The registry is shared: the media serves it and the plugins that load a
/// container publish into it.
#[derive(Default)]
pub(crate) struct VarRegistryState {
    /// The registry the media and the publishers share.
    registry: Rc<VarRegistry>,
}

/// The registry the `var://` media and the publishers share.
pub(crate) fn registry(services: &CompatServices) -> Rc<VarRegistry> {
    services.state::<VarRegistryState>().registry.clone()
}

/// Publish a container to the `var://` namespace under `path`.
///
/// The path is the path below the global member that reaches the container
/// — the media reads the container's members under that path.
pub(crate) fn publish(services: &CompatServices, path: &str, container: Rc<dyn VarContainer>) {
    registry(services).publish(path, container);
}

// ---------------------------------------------------------------------------
// The provider
// ---------------------------------------------------------------------------

/// The provider that serves the `var://` media.
struct VarMedia {
    /// The containers published to the namespace.
    registry: Rc<VarRegistry>,
    /// The VM whose global object the media walks.
    engine: *const Tjs2Engine,
    /// The thread the provider was built on.
    ///
    /// The walk reads a live script object graph, so it runs on the thread
    /// that owns the VM. A published container is read without the VM and
    /// is not bound to that thread.
    thread: ThreadId,
}

impl VarMedia {
    /// Build the provider for the VM `engine`.
    fn new(engine: *const Tjs2Engine, registry: Rc<VarRegistry>) -> Self {
        Self {
            registry,
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
        let Some((domain, rest)) = path.split_once('/') else {
            return Err(self.error(format!("invalid path:{path}")));
        };
        if domain != "." {
            return Err(self.error(format!("no such domain:{domain}")));
        }
        // A published container answers the read on its own: its bytes come
        // from the container, and the script object graph is not touched.
        if let Some((container, member)) = self.registry.lookup(rest) {
            return match container.file(member) {
                Ok(bytes) => Ok(bytes),
                Err(message) => Err(self.error(message)),
            };
        }
        self.walk(rest)
    }

    /// The octet the path names in the script object graph.
    ///
    /// `path` is the storage name with its scheme removed and its domain
    /// split off: `<member>/…`. The walk starts at the VM's global object,
    /// which the engine hands over directly, and reads every member through
    /// `PropGet`.
    fn walk(&self, path: &str) -> Result<Option<Vec<u8>>, StorageReadError> {
        if std::thread::current().id() != self.thread {
            return Err(self.error(
                "varfile: the media was read from a thread that does not own the VM".to_string(),
            ));
        }
        let segments: Vec<&str> = path.split('/').collect();
        let Some((file, parents)) = segments.split_last() else {
            return Err(self.error(format!("invalid path:{path}")));
        };
        if file.is_empty() {
            return Ok(None);
        }
        let engine = self.engine();
        let global = engine
            .global_object()
            .map_err(|error| self.error(format!("varfile: {error}")))?;
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
            // An entry that is not a file is a missing storage, not a
            // failure: the caller decides what a missing member means.
            None => Err(StorageReadError::NotFound(path.to_string())),
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
    storage.register_media(Box::new(VarMedia::new(engine, registry(services))));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{VarContainer, VarMedia, VarRegistry};
    use std::rc::Rc;
    use tjs::Tjs2Engine;

    /// A container that answers one member with fixed bytes.
    struct Fixed {
        /// The member the container answers for.
        member: &'static str,
        /// The bytes that member answers with.
        bytes: Vec<u8>,
    }

    impl VarContainer for Fixed {
        fn file(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
            Ok((name == self.member).then(|| self.bytes.clone()))
        }
    }

    /// The id of a thread that is not the current one.
    fn other_thread_id() -> std::thread::ThreadId {
        let handle = std::thread::spawn(|| {});
        let id = handle.thread().id();
        handle.join().expect("the thread ends");
        id
    }

    /// A published container is read without the script object graph.
    ///
    /// The media is told that the VM lives on another thread: a path that
    /// names a published container is answered all the same, and a path that
    /// reaches the script object graph is refused. The read of a published
    /// container therefore never touches the VM.
    #[test]
    fn a_published_container_is_read_without_the_vm() {
        let engine = Tjs2Engine::new().expect("the engine");
        let registry = Rc::new(VarRegistry::default());
        registry.publish(
            "pack.pimg/root",
            Rc::new(Fixed {
                member: "3797.tlg",
                bytes: b"tile".to_vec(),
            }),
        );
        let mut media = VarMedia::new(&engine as *const Tjs2Engine, registry);
        media.thread = other_thread_id();

        assert_eq!(
            media
                .resolve("./packImageMap/pack.pimg/root/3797.tlg")
                .expect("the read succeeds"),
            Some(b"tile".to_vec()),
            "a published container answers the read without the VM"
        );
        let error = media
            .resolve("./packImageMap/pack.pimg/root")
            .expect_err("a path that reaches the script object graph is refused");
        assert!(
            error.to_string().contains("does not own the VM"),
            "unexpected error: {error}"
        );
    }
}
