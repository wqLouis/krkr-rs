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
use std::time::{Duration, Instant};

use super::SlowReporter;
use super::container::{PsbDocument, PsbValue, ResourceRef};
use super::is_slow;
use super::normalize_psb_domain;
use crate::CompatServices;
use crate::collections::Map;
use crate::engine_api::{StorageMedia, StorageReadError};
use crate::natives::plugins::var_file::VarContainer;

/// Which resources of a container the media serves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PsbScope {
    /// Only the resources that sit directly under the container's root.
    RootResources,
    /// Every resource in the value tree, under its member path.
    WholeTree,
}

/// One container the media serves.
pub(crate) struct PsbContainer {
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
    containers: RefCell<Map<String, Rc<PsbContainer>>>,
    /// The reporter that keeps the slow-read report bounded.
    reporter: RefCell<SlowReporter>,
}

impl PsbMedia {
    /// Register a container whose resources sit directly under its root.
    ///
    /// The container is returned, so that the caller can publish the same
    /// one to another namespace.
    pub(crate) fn register_root_resources(
        &self,
        container: &str,
        document: Rc<PsbDocument>,
    ) -> Rc<PsbContainer> {
        let entry = Rc::new(PsbContainer {
            document,
            scope: PsbScope::RootResources,
        });
        self.containers
            .borrow_mut()
            .insert(container.to_string(), Rc::clone(&entry));
        entry
    }

    /// Register a container whose whole value tree is served.
    pub(crate) fn register_whole_tree(&self, container: &str, document: Rc<PsbDocument>) {
        self.register(container, document, PsbScope::WholeTree);
    }

    /// Register a container under `container`, replacing an earlier one.
    fn register(&self, container: &str, document: Rc<PsbDocument>, scope: PsbScope) {
        self.containers.borrow_mut().insert(
            container.to_string(),
            Rc::new(PsbContainer { document, scope }),
        );
    }

    /// The container and the resource the entry `path` names.
    ///
    /// `path` is the storage name with its scheme removed:
    /// `<container>/<member path>`. `None` when the media holds no container
    /// under that name, or the container holds no member under that path.
    fn lookup(&self, path: &str) -> Option<(Rc<PsbDocument>, ResourceRef)> {
        let (container, member) = path.split_once('/')?;
        let containers = self.containers.borrow();
        let entry = containers.get(&normalize_psb_domain(container))?;
        let reference = entry.resource(member)?;
        Some((Rc::clone(&entry.document), reference))
    }

    /// Reports a read that took longer than the threshold.
    ///
    /// The line names the phases: the lookup and the copy of the resource's
    /// bytes. The report is bounded: the reporter keeps the interval
    /// between two reports, so a read that is slow in every frame does not
    /// report in every frame.
    fn report_slow_read(&self, path: &str, elapsed: Duration, lookup_ms: f64, copy_ms: f64) {
        if !is_slow(elapsed) {
            return;
        }
        let mut reporter = self.reporter.borrow_mut();
        if !reporter.is_due() {
            return;
        }
        log::warn!(
            "psb: the read of psb://{path} took {:.1} ms (lookup={lookup_ms:.1} copy={copy_ms:.1})",
            elapsed.as_secs_f64() * 1000.0,
        );
    }
}

impl VarContainer for PsbContainer {
    fn file(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        let Some(reference) = self.resource(name) else {
            return Ok(None);
        };
        match self.document.resource_bytes(reference) {
            Some(bytes) => Ok(Some(bytes.to_vec())),
            None => Err(format!(
                "the container carries no bytes for the member {name}"
            )),
        }
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
        let started = Instant::now();
        let Some((document, reference)) = self.media.lookup(path) else {
            // A member the container does not hold is a missing storage, not
            // a failure: the caller decides what a missing member means.
            return Err(StorageReadError::NotFound(path.to_string()));
        };
        let lookup_ms = started.elapsed().as_secs_f64() * 1000.0;
        let copy_started = Instant::now();
        let bytes = document
            .resource_bytes(reference)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| StorageReadError::Media {
                media: "psb".to_string(),
                message: format!("the container carries no bytes for psb://{path}"),
            })?;
        self.media.report_slow_read(
            path,
            started.elapsed(),
            lookup_ms,
            copy_started.elapsed().as_secs_f64() * 1000.0,
        );
        Ok(bytes)
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
/// The provider is installed when the plugin is linked and when the motion
/// player registers a container, because the media needs the storage
/// namespace it serves. Registering it again replaces the provider and keeps
/// the containers the media already holds.
pub(crate) fn install_provider(services: &CompatServices) -> Result<(), crate::CompatError> {
    let Some(storage) = services.storage() else {
        log::warn!("psb: no storage is attached; the psb:// media is not served");
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
