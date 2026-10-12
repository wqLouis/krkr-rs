//! Face registry populated by `System.addFont`.
//!
//! The reference `addFont` reads a font file from the TVP storage and adds
//! every face in it to the process-wide `TVPFontNames` map (family name →
//! font stream); the renderer later finds a requested `Font.face` there.
//!
//! krkr-rs keeps no process global: the same information lives in this
//! per-engine dispatch state, created lazily inside
//! [`CompatServices::state`](crate::CompatServices::state). Each registered
//! file carries the L2 [`TextEngine`] its faces were loaded into, so a family
//! lookup goes through the same shaping database the visual `Font` native and
//! the `TextRenderBase` plugin use ([`TextEngine::font_id`]); a consumer only
//! has to load the returned bytes at the returned collection index into its
//! own engine, because the [`FontId`] itself is engine-local.

use std::sync::Arc;

use engine::text::{FontId, TextEngine};

use crate::CompatServices;

/// A face registered by `System.addFont`: `(bytes, collection index, faces in
/// the source file)`.
///
/// A consumer loads `bytes` at `index` into its own [`TextEngine`]. The third
/// element is the `System.addFont` result for the source file, so a caller
/// that only needs the count does not re-parse.
pub(crate) type RegisteredFace = (Arc<[u8]>, u32, usize);

/// One registered font file.
struct RegisteredSource {
    /// The storage name the file was read from.
    source: String,
    /// The file's bytes, shared with every face entry.
    bytes: Arc<[u8]>,
    /// The engine the faces were loaded into, kept so a family-name lookup
    /// uses the exact L2 face database that parsed the file.
    engine: TextEngine,
    /// Face ids in collection order (`FontId::index` is the face's position).
    ids: Vec<FontId>,
}

/// The per-engine `System.addFont` registry.
#[derive(Default)]
pub(crate) struct FontRegistry {
    /// Registered files in registration order; the most recent wins.
    sources: Vec<RegisteredSource>,
}

impl FontRegistry {
    /// Resolve `name` — a family name or a registered storage name — to the
    /// face a consumer should load.
    ///
    /// The most recent registration wins, matching the reference's
    /// hash-table `Add`.
    pub(crate) fn lookup(&self, name: &str) -> Option<RegisteredFace> {
        for source in self.sources.iter().rev() {
            let id = source.engine.font_id(name).or_else(|| {
                source
                    .source
                    .eq_ignore_ascii_case(name)
                    .then(|| source.ids.first().copied())
                    .flatten()
            });
            if let Some(id) = id {
                return Some((source.bytes.clone(), id.index() as u32, source.ids.len()));
            }
        }
        None
    }
}

/// Register every usable face in `bytes` under `source`.
///
/// Returns the number of faces (`0` when the data is not a usable font, the
/// reference's "font registration failed" result). Parsing and validation go
/// through the L2 [`TextEngine::add_font`], so the plugin does not grow its
/// own font parser.
pub(crate) fn register(services: &CompatServices, source: &str, bytes: &[u8]) -> i64 {
    let shared: Arc<[u8]> = Arc::from(bytes);
    let mut engine = TextEngine::new();
    let mut ids = Vec::new();
    loop {
        let index = ids.len() as u32;
        match engine.add_font(&shared, index) {
            Ok(id) => ids.push(id),
            Err(error) => {
                if ids.is_empty() {
                    log::debug!("System.addFont({source:?}): not a usable font: {error}");
                }
                break;
            }
        }
    }
    if ids.is_empty() {
        return 0;
    }
    let count = ids.len() as i64;
    // Re-adding a source replaces its previous registration; the reference's
    // hash-table `Add` has the same observable effect.
    let mut registry = services.state::<FontRegistry>();
    registry
        .sources
        .retain(|existing| !existing.source.eq_ignore_ascii_case(source));
    registry.sources.push(RegisteredSource {
        source: source.to_string(),
        bytes: shared,
        engine,
        ids,
    });
    count
}
