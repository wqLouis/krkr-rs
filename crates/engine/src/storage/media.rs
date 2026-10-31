//! Storage media: the providers that serve one storage scheme.
//!
//! A storage name may carry a scheme (`<scheme>://<path>`). A name that
//! carries no scheme belongs to the default media — the mounted archives and
//! the disk below the mount root — and is resolved by the namespace itself.
//! A name that carries a scheme belongs to the media that registered that
//! scheme, and the namespace hands it to that provider.
//!
//! A provider is the generic form of a namespaced storage: it answers
//! whether a path exists under its domain and reads the bytes of one entry.
//! The provider owns whatever its domain is made of — a parsed container, a
//! script object graph — and the namespace knows nothing about it.
//!
//! A provider runs on the thread that asked for the bytes, never on the read
//! pool: a media name is never scheduled as a pool job, because the work it
//! describes is not a file-system read.

use crate::storage::StorageReadError;

/// A provider that serves the entries of one storage scheme.
///
/// The provider is registered under its own [`Self::domain`]; a later
/// registration for the same domain replaces the earlier provider.
pub trait StorageMedia {
    /// The scheme this provider serves, without the `://`.
    ///
    /// The namespace matches the scheme of a storage name against this value
    /// case-insensitively.
    fn domain(&self) -> &str;

    /// Whether `path` names an existing entry under this domain.
    ///
    /// `path` is the storage name with its scheme removed; the domain's own
    /// path grammar (a leading `./`, a container name, …) belongs to the
    /// provider.
    ///
    /// # Errors
    ///
    /// A provider that cannot decide — a malformed path, a domain the
    /// provider does not serve — reports it as
    /// [`StorageReadError::Media`]. An entry that simply does not exist is
    /// `Ok(false)`.
    fn exists(&self, path: &str) -> Result<bool, StorageReadError>;

    /// Reads the entry `path` names under this domain.
    ///
    /// `path` is the storage name with its scheme removed; the domain's own
    /// path grammar belongs to the provider.
    ///
    /// # Errors
    ///
    /// A provider that cannot read the entry reports it as
    /// [`StorageReadError::Media`], carrying the message its own domain
    /// raises. The message travels to the caller unchanged.
    fn read(&self, path: &str) -> Result<Vec<u8>, StorageReadError>;
}
