//! The `psb://` storage media: the containers it serves.
//!
//! The media serves the resources of the containers that were registered
//! under it, and a resource is reachable under the `/`-joined path of the
//! members that contain it.
//!
//! Two registration modes exist:
//!
//! * [`PsbScope::RootResources`] — the resources that sit directly under the
//!   container's root. This is what a `PSBFile.load` registers.
//! * [`PsbScope::WholeTree`] — every resource in the value tree, under its
//!   member path. This is the mode of the separate registry of the container
//!   resources, which only the motion player uses.
//!
//! The provider resolves a member path against the parsed document when the
//! entry is read, so a container is registered once and its bytes are not
//! copied into a table.
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::rc::Rc;

use super::container::{PsbDocument, PsbValue, ResourceRef};
use super::normalize_psb_domain;
use crate::CompatServices;
use crate::collections::Map;
use crate::engine_api::{StorageMedia, StorageReadError};

/// Which resources of a container the media serves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PsbScope {
    /// Only the resources that sit directly under the container's root.
    RootResources,
    /// Every resource in the value tree, under its member path.
    WholeTree,
}

/// One container the media serves.
struct PsbContainer {
    /// The parsed document the resources are read from.
    document: Rc<PsbDocument>,
    /// Which of its resources the media serves.
    scope: PsbScope,
}

impl PsbContainer {
    /// The resource the member path names, in this container's scope.
    fn resource(&self, member: &str) -> Option<ResourceRef> {
        match self.scope {
            PsbScope::RootResources => {
                let PsbValue::Dictionary(entries) = &self.document.root else {
                    return None;
                };
                entries
                    .iter()
                    .find(|(key, _)| key == member)
                    .and_then(|(_, value)| match value {
                        PsbValue::Resource(reference) => Some(*reference),
                        _ => None,
                    })
            }
            PsbScope::WholeTree => {
                let mut value = &self.document.root;
                for segment in member.split('/') {
                    value = match value {
                        PsbValue::Dictionary(entries) => entries
                            .iter()
                            .find(|(key, _)| key == segment)
                            .map(|(_, value)| value)?,
                        PsbValue::List(items) => {
                            let index = match segment.parse::<usize>() {
                                Ok(index) => index,
                                Err(error) => {
                                    // A path segment that is not a list index
                                    // names no entry.
                                    log::debug!(
                                        "psb media: {segment:?} is not a list index: {error}"
                                    );
                                    return None;
                                }
                            };
                            items.get(index)?
                        }
                        _ => return None,
                    };
                }
                match value {
                    PsbValue::Resource(reference) => Some(*reference),
                    _ => None,
                }
            }
        }
    }
}

/// The `psb://` media: the containers whose resources it serves.
#[derive(Default)]
pub(crate) struct PsbMedia {
    /// The registered containers, by their normalized name.
    containers: RefCell<Map<String, PsbContainer>>,
}

impl PsbMedia {
    /// Register a container whose resources sit directly under its root.
    pub(crate) fn register_root_resources(&self, container: &str, document: Rc<PsbDocument>) {
        self.register(container, document, PsbScope::RootResources);
    }

    /// Register a container whose whole value tree is served.
    pub(crate) fn register_whole_tree(&self, container: &str, document: Rc<PsbDocument>) {
        self.register(container, document, PsbScope::WholeTree);
    }

    /// Register a container under `container`, replacing an earlier one.
    fn register(&self, container: &str, document: Rc<PsbDocument>, scope: PsbScope) {
        self.containers
            .borrow_mut()
            .insert(container.to_string(), PsbContainer { document, scope });
    }

    /// The bytes of the entry `path` names.
    ///
    /// `path` is the storage name with its scheme removed:
    /// `<container>/<member path>`.
    fn lookup(&self, path: &str) -> Option<Vec<u8>> {
        let (container, member) = path.split_once('/')?;
        let containers = self.containers.borrow();
        let entry = containers.get(&normalize_psb_domain(container))?;
        let reference = entry.resource(member)?;
        entry.document.resource_bytes(reference).map(<[u8]>::to_vec)
    }
}

/// The `psb://` provider: it serves the containers the media holds.
pub(crate) struct PsbProvider {
    /// The media the provider serves.
    media: Rc<PsbMedia>,
}

/// The provider that serves `media`.
pub(crate) fn provider(media: Rc<PsbMedia>) -> PsbProvider {
    PsbProvider { media }
}

impl StorageMedia for PsbProvider {
    fn domain(&self) -> &str {
        "psb"
    }

    fn exists(&self, path: &str) -> Result<bool, StorageReadError> {
        Ok(self.media.lookup(path).is_some())
    }

    fn read(&self, path: &str) -> Result<Vec<u8>, StorageReadError> {
        self.media
            .lookup(path)
            .ok_or_else(|| StorageReadError::Media {
                media: "psb".to_string(),
                // The storage manager raises this when a media hands back no
                // stream.
                message: format!("Cannot open storage psb://{path}"),
            })
    }
}

/// The L3 state that owns the `psb://` media.
///
/// The media is shared: the `psbfile` plugin and the motion player both
/// register containers in it, and both install the provider that serves it.
#[derive(Default)]
pub(crate) struct PsbMediaState {
    /// The media the plugins share.
    media: Rc<PsbMedia>,
}

/// The `psb://` media the plugins share.
pub(crate) fn media(services: &CompatServices) -> Rc<PsbMedia> {
    services.state::<PsbMediaState>().media.clone()
}

/// Register the `psb://` provider with the attached storage.
///
/// The media is registered when the plugin is linked, because the media needs
/// the storage namespace it serves. Registering it again replaces the
/// provider and keeps the containers the media already holds.
pub(crate) fn install_provider(services: &CompatServices) -> Result<(), crate::CompatError> {
    let Some(storage) = services.storage() else {
        log::warn!("psbfile: no storage is attached; the psb:// media is not served");
        return Ok(());
    };
    let media = media(services);
    // SAFETY: the host keeps the attached storage alive for the binding.
    let storage = unsafe { &*storage };
    storage.register_media(Box::new(provider(media)));
    Ok(())
}

/// Fixtures for the media's own tests.
#[cfg(test)]
pub(crate) mod fixture {
    use std::rc::Rc;

    use fs::FileSystem;

    use crate::natives::plugins::psb_file::container::{PsbDocument, PsbValue, ResourceRef};

    use super::{PsbMedia, provider};

    /// The resource reference of the chunk at `index`.
    pub(crate) fn chunk(index: u32) -> ResourceRef {
        ResourceRef {
            index,
            extra: false,
        }
    }

    /// A container with the given root value and the given chunk bytes.
    ///
    /// A chunk member of the root is a [`PsbValue::Resource`] whose reference
    /// is the chunk's index.
    pub(crate) fn container(root: PsbValue, chunks: Vec<Vec<u8>>) -> PsbDocument {
        let resources = chunks
            .into_iter()
            .enumerate()
            .map(|(index, bytes)| (chunk(index as u32), bytes))
            .collect();
        PsbDocument { root, resources }
    }

    /// A storage that serves the `psb://` media with one container.
    pub(crate) fn media_storage(
        container: &str,
        document: Rc<PsbDocument>,
    ) -> crate::engine_api::Storage {
        let dir = std::env::temp_dir();
        let storage = crate::engine_api::Storage::mount_archives(
            FileSystem::host(dir.clone()),
            dir.clone(),
            std::iter::empty::<&str>(),
        )
        .expect("mount storage");
        let media = Rc::new(PsbMedia::default());
        media.register_whole_tree(container, document);
        storage.register_media(Box::new(provider(media)));
        storage
    }
}
