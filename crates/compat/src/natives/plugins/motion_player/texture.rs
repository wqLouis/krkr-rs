//! Pixel decoding for the PSB icon textures the player draws.
//!
//! A group's icons live either in the group's `texture` atlas, which each
//! icon crops with its `left`/`top`/`width`/`height`, or in a per-icon `pixel`
//! chunk. Either chunk is raw 32-bit RGBA, `RL` run-length compressed, or
//! 8-bit index data resolved through the sibling `pal` chunk. Decoding yields
//! straight-alpha RGBA8 in the scene's pixel layout.
//!
//! The chunks are decoded on the engine's job pool. The chunks of one pose
//! are independent of each other, so they are decoded in parallel and the
//! draw waits for the pixels of the chunk it needs. The storage read stays on
//! the frame thread — the storage namespace is not shared with the workers —
//! and the pool decodes the bytes the read produced.

use std::sync::{Arc, Mutex, MutexGuard};

use crate::collections::Map;
use crate::engine_api::{
    DecodePool, JobOutcome, JobValue, JobWork, PendingJob, Storage, StorageReadError,
};
use crate::storage::strip_file_media;

use super::model::{IconInfo, MotionSnapshot};

/// One pixel chunk the player draws from.
///
/// The chunk is named by its `psb://` storage name, its palette is the
/// sibling `pal` chunk, and its size is the size the decode produces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChunkRequest {
    /// The chunk's storage name.
    pub(crate) path: String,
    /// The palette chunk's storage name.
    pub(crate) palette_path: String,
    /// The decoded width in pixels.
    pub(crate) width: u32,
    /// The decoded height in pixels.
    pub(crate) height: u32,
    /// Whether the chunk is `RL` run-length compressed.
    pub(crate) compressed: bool,
}

/// The chunks one icon draws from.
///
/// A group with a `texture` atlas serves its icons from the atlas; an icon
/// whose group has no atlas draws from its own `pixel` chunk. The own chunk
/// is also the fallback for an atlas that does not decode or does not hold
/// the icon's crop.
#[derive(Clone, Debug)]
pub(crate) struct IconChunks {
    /// The group's atlas, when the group has one.
    pub(crate) atlas: Option<ChunkRequest>,
    /// The icon's own chunk.
    pub(crate) own: ChunkRequest,
}

impl IconChunks {
    /// The chunks the icon `icon` of `snapshot` draws from.
    ///
    /// `width` and `height` are the icon's own pixel size; they size its
    /// chunk.
    pub(crate) fn new(snapshot: &MotionSnapshot, icon: &IconInfo, width: u32, height: u32) -> Self {
        let atlas = snapshot.textures.get(&icon.group).and_then(|texture| {
            let width = pixel_size(texture.width)?;
            let height = pixel_size(texture.height)?;
            Some(ChunkRequest {
                path: format!(
                    "psb://{}/source/{}/texture/pixel",
                    snapshot.container, icon.group
                ),
                palette_path: format!(
                    "psb://{}/source/{}/texture/pal",
                    snapshot.container, icon.group
                ),
                width,
                height,
                compressed: texture.compressed,
            })
        });
        Self {
            atlas,
            own: ChunkRequest {
                path: format!(
                    "psb://{}/source/{}/icon/{}/pixel",
                    snapshot.container, icon.group, icon.name
                ),
                palette_path: format!(
                    "psb://{}/source/{}/icon/{}/pal",
                    snapshot.container, icon.group, icon.name
                ),
                width,
                height,
                compressed: icon.compressed,
            },
        }
    }
}

/// The decoded pixel chunks of one motion container, keyed by their `psb://`
/// storage name. Every player that shares the snapshot reuses the entries, so
/// a chunk is read and decoded once for the lifetime of the snapshot.
#[derive(Clone, Default)]
pub(crate) struct PixelCache {
    /// The chunks that are decoded.
    entries: Arc<Mutex<Map<String, Arc<Pixels>>>>,
    /// The decodes that are running, keyed by their chunk's storage name.
    ///
    /// A decode that no draw consumed stays here, so the next draw waits for
    /// the same decode instead of reading and decoding the chunk again.
    states: Arc<Mutex<Map<String, ChunkState>>>,
}

impl PixelCache {
    /// Drop every decoded chunk and every decode that is running.
    pub(crate) fn clear(&self) {
        self.entries().clear();
        self.states().clear();
    }

    /// Submit the decodes of `requests` that are not decoded yet.
    ///
    /// The chunks are independent of each other, so they are decoded in
    /// parallel: the pose waits for the pixels of a chunk when it needs them.
    pub(crate) fn prefetch(
        &self,
        pool: &mut DecodePool,
        storage: &Storage,
        requests: &[ChunkRequest],
    ) {
        for request in requests {
            self.submit(Some(&mut *pool), storage, request);
        }
    }

    /// The decoded pixels of the chunk `request` names.
    ///
    /// The chunk is decoded on the pool when it is not decoded yet, and the
    /// caller waits for the decode. A chunk that cannot be read or decoded
    /// has no pixels: the failure is reported and the caller draws nothing.
    ///
    /// Without a pool there is no worker to decode on, so the chunk is
    /// decoded on the calling thread.
    pub(crate) fn get_or_decode(
        &self,
        pool: Option<&mut DecodePool>,
        storage: &Storage,
        request: &ChunkRequest,
    ) -> Option<Arc<Pixels>> {
        if let Some(pixels) = self.entries().get(&request.path) {
            return Some(pixels.clone());
        }
        let Some(pool) = pool else {
            return self.decode_here(storage, request);
        };
        // The decode that runs for this chunk, or the failure the read
        // produced before it was submitted.
        let running = self.states().remove(&request.path);
        let state = match running {
            Some(state) => state,
            None => {
                self.submit(Some(pool), storage, request);
                let submitted = self.states().remove(&request.path);
                match submitted {
                    Some(state) => state,
                    // The chunk was decoded while it was submitted.
                    None => return self.entries().get(&request.path).cloned(),
                }
            }
        };
        match state {
            ChunkState::Absent => None,
            ChunkState::Unreadable(message) => {
                log::warn!("{message}");
                None
            }
            ChunkState::Decoding {
                job,
                palette_warning,
            } => {
                if let Some(warning) = palette_warning {
                    log::warn!("{warning}");
                }
                match job.await_ready() {
                    JobOutcome::Ready(value) => {
                        let pixels = match value.downcast::<Pixels>() {
                            Ok(pixels) => pixels,
                            Err(error) => {
                                log::error!(
                                    "motion_player: the chunk decode produced another value: {error}"
                                );
                                return None;
                            }
                        };
                        self.entries().insert(request.path.clone(), pixels.clone());
                        Some(pixels)
                    }
                    JobOutcome::Failed(message) => {
                        log::warn!("{message}");
                        None
                    }
                    JobOutcome::Cancelled => {
                        log::warn!(
                            "motion_player: the decode of {} was cancelled",
                            request.path
                        );
                        None
                    }
                }
            }
        }
    }

    /// Submit the decode of `request` and record the state of the chunk.
    ///
    /// The chunk is read on this thread — the storage namespace is not
    /// shared with the workers — and the pool decodes the bytes the read
    /// produced. A chunk that is decoded already, or whose decode is already
    /// running, is left alone.
    fn submit(&self, pool: Option<&mut DecodePool>, storage: &Storage, request: &ChunkRequest) {
        let Some(pool) = pool else {
            self.decode_here(storage, request);
            return;
        };
        if self.entries().contains_key(&request.path) {
            return;
        }
        if self.states().contains_key(&request.path) {
            return;
        }
        let bytes = match read_chunk(storage, &request.path) {
            ChunkBytes::Read(bytes) => bytes,
            ChunkBytes::Absent => {
                self.states()
                    .insert(request.path.clone(), ChunkState::Absent);
                return;
            }
            ChunkBytes::Unreadable(message) => {
                self.states()
                    .insert(request.path.clone(), ChunkState::Unreadable(message));
                return;
            }
        };
        let (palette, palette_warning) = read_palette(storage, &request.palette_path);
        let palette: Option<Arc<[u8]>> = palette.map(Arc::from);
        let bytes: Arc<[u8]> = Arc::from(bytes);
        let path = request.path.clone();
        let width = request.width;
        let height = request.height;
        let compressed = request.compressed;
        let job_bytes = Arc::clone(&bytes);
        let job_palette = palette.clone();
        let work: Arc<JobWork> = Arc::new(move || {
            let pixels = decode_pixels(
                job_bytes.as_ref(),
                job_palette.as_deref(),
                width,
                height,
                compressed,
            )
            .map_err(|error| format!("motion_player: cannot decode {path}: {error}"))?;
            Ok(JobValue::new(pixels))
        });
        match pool.submit_job(work, chunk_charge(bytes.len(), width, height)) {
            Ok((_id, job)) => {
                self.states().insert(
                    request.path.clone(),
                    ChunkState::Decoding {
                        job,
                        palette_warning,
                    },
                );
            }
            Err(error) => {
                // The pool refused the job: the chunk is decoded on this
                // thread, and the refusal is reported rather than swallowed.
                // The error names the limit the pool refused the job against.
                log::warn!(
                    "motion_player: the job pool refused {:?} ({error}); the chunk is decoded on the \
                     drawing thread",
                    request.path
                );
                if let Some(warning) = palette_warning {
                    log::warn!("{warning}");
                }
                match decode_pixels(
                    bytes.as_ref(),
                    palette.as_deref(),
                    width,
                    height,
                    compressed,
                ) {
                    Ok(pixels) => {
                        let pixels = Arc::new(pixels);
                        self.entries().insert(request.path.clone(), pixels);
                    }
                    Err(error) => {
                        log::warn!("motion_player: cannot decode {}: {error}", request.path);
                    }
                }
            }
        }
    }

    /// Decode the chunk `request` names on this thread.
    ///
    /// A host that attached no pool has no worker to decode on, so the chunk
    /// is decoded where it is used.
    fn decode_here(&self, storage: &Storage, request: &ChunkRequest) -> Option<Arc<Pixels>> {
        let bytes = match read_chunk(storage, &request.path) {
            ChunkBytes::Read(bytes) => bytes,
            ChunkBytes::Absent => return None,
            ChunkBytes::Unreadable(message) => {
                log::warn!("{message}");
                return None;
            }
        };
        let (palette, palette_warning) = read_palette(storage, &request.palette_path);
        if let Some(warning) = palette_warning {
            log::warn!("{warning}");
        }
        match decode_pixels(
            &bytes,
            palette.as_deref(),
            request.width,
            request.height,
            request.compressed,
        ) {
            Ok(pixels) => {
                let pixels = Arc::new(pixels);
                self.entries().insert(request.path.clone(), pixels.clone());
                Some(pixels)
            }
            Err(error) => {
                log::warn!("motion_player: cannot decode {}: {error}", request.path);
                None
            }
        }
    }

    fn entries(&self) -> MutexGuard<'_, Map<String, Arc<Pixels>>> {
        match self.entries.lock() {
            Ok(entries) => entries,
            Err(poisoned) => {
                log::warn!("motion_player: the pixel cache lock was poisoned; recovering");
                poisoned.into_inner()
            }
        }
    }

    fn states(&self) -> MutexGuard<'_, Map<String, ChunkState>> {
        match self.states.lock() {
            Ok(states) => states,
            Err(poisoned) => {
                log::warn!("motion_player: the pixel cache lock was poisoned; recovering");
                poisoned.into_inner()
            }
        }
    }
}

impl std::fmt::Debug for PixelCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PixelCache")
            .field("entries", &self.entries().len())
            .field("decoding", &self.states().len())
            .finish()
    }
}

/// The state of one chunk's decode.
enum ChunkState {
    /// The chunk is absent from the storage.
    Absent,
    /// The chunk could not be read; the message is the warning to report
    /// when the chunk is used.
    Unreadable(String),
    /// The decode runs on the pool; the warning the palette's read produced
    /// is reported when the chunk is used.
    Decoding {
        /// The handle the decode's result lands in.
        job: PendingJob,
        /// The warning the palette's read produced.
        palette_warning: Option<String>,
    },
}

/// The bytes a chunk's read produced.
enum ChunkBytes {
    /// The chunk's bytes.
    Read(Vec<u8>),
    /// The chunk is absent from the storage.
    Absent,
    /// The chunk could not be read; the message is the warning to report
    /// when the chunk is used.
    Unreadable(String),
}

/// Read a chunk from the storage.
///
/// A chunk the storage does not hold is absent; a chunk whose read failed
/// carries the warning to report.
fn read_chunk(storage: &Storage, path: &str) -> ChunkBytes {
    match storage.read(strip_file_media(path)) {
        Ok(bytes) => ChunkBytes::Read(bytes),
        Err(StorageReadError::NotFound(_)) => ChunkBytes::Absent,
        Err(error) => ChunkBytes::Unreadable(format!("motion_player: cannot read {path}: {error}")),
    }
}

/// Read the palette a chunk's indices resolve through.
///
/// The palette is absent when the container carries none. A palette whose
/// read failed is reported when the chunk is used, and the chunk is then
/// decoded as raw pixels.
fn read_palette(storage: &Storage, path: &str) -> (Option<Vec<u8>>, Option<String>) {
    match read_chunk(storage, path) {
        ChunkBytes::Read(bytes) => (Some(bytes), None),
        ChunkBytes::Absent => (None, None),
        ChunkBytes::Unreadable(message) => (None, Some(message)),
    }
}

/// The bytes a chunk's decode holds against the pool's memory limit: the
/// chunk's bytes and the pixels the decode produces.
///
/// A size that does not fit the accounting is charged as the whole limit, so
/// the pool refuses the job instead of accepting a size it cannot hold.
fn chunk_charge(bytes: usize, width: u32, height: u32) -> u64 {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4));
    let bytes = match u64::try_from(bytes) {
        Ok(bytes) => bytes,
        Err(error) => {
            log::debug!("motion_player: the chunk's size does not fit the accounting: {error}");
            return u64::MAX;
        }
    };
    let Some(charge) = pixels.and_then(|pixels| bytes.checked_add(pixels)) else {
        return u64::MAX;
    };
    charge
}

/// An image dimension rounded to whole pixels, or `None` when it carries no
/// usable size.
pub(crate) fn pixel_size(value: f64) -> Option<u32> {
    let rounded = value.round();
    if !rounded.is_finite() || rounded < 1.0 || rounded > f64::from(u32::MAX) {
        return None;
    }
    Some(rounded as u32)
}

/// A whole-pixel offset into an atlas, or `None` when it is negative.
pub(crate) fn pixel_offset(value: f64) -> Option<u32> {
    let rounded = value.round();
    if !rounded.is_finite() || rounded < 0.0 || rounded > f64::from(u32::MAX) {
        return None;
    }
    Some(rounded as u32)
}

/// Decoded straight-alpha RGBA8 pixels of one pixel chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pixels {
    /// The image width in pixels.
    pub(crate) width: u32,
    /// The image height in pixels.
    pub(crate) height: u32,
    /// `width * height * 4` bytes.
    pub(crate) rgba: Vec<u8>,
}

/// Why a pixel chunk could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TextureError {
    /// The palette chunk holds no complete entry, so no index resolves.
    EmptyPalette,
    /// An 8-bit index addresses a palette entry that does not exist.
    PaletteIndex {
        /// The offending index.
        index: u8,
        /// The number of whole entries the palette holds.
        entries: usize,
    },
    /// A dimension or element count does not fit the address space.
    TooLarge,
}

impl std::fmt::Display for TextureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyPalette => write!(f, "the palette chunk holds no entries"),
            Self::PaletteIndex { index, entries } => {
                write!(f, "palette index {index} is outside {entries} entries")
            }
            Self::TooLarge => write!(f, "the image dimensions are too large"),
        }
    }
}

/// Decode one pixel chunk into RGBA8 pixels.
///
/// `palette` is the sibling `pal` chunk when the container carries one; it
/// takes precedence over `compressed`, whose 8-bit path is
/// run-length compressed too. A stream that ends early leaves the remaining
/// pixels transparent.
pub(crate) fn decode_pixels(
    bytes: &[u8],
    palette: Option<&[u8]>,
    width: u32,
    height: u32,
    compressed: bool,
) -> Result<Pixels, TextureError> {
    let count = (width as usize)
        .checked_mul(height as usize)
        .ok_or(TextureError::TooLarge)?;
    if count.checked_mul(4).is_none() {
        return Err(TextureError::TooLarge);
    }
    let rgba = match palette.filter(|palette| !palette.is_empty()) {
        Some(palette) => expand_palette(bytes, palette, count, compressed)?,
        None if compressed => swap_red_blue(rl_decode(bytes, count, 4)),
        None => swap_red_blue(raw(bytes, count)),
    };
    Ok(Pixels {
        width,
        height,
        rgba,
    })
}

/// Swap the red and blue channels of straight-alpha RGBA pixels.
///
/// The PSB raw/RL pixel chunks store their colour channels in B,G,R,A order;
/// the scene bitmap stores R,G,B,A, so the two channels are exchanged once
/// after decoding. The palette path already writes the scene order.
fn swap_red_blue(mut rgba: Vec<u8>) -> Vec<u8> {
    for pixel in rgba.as_chunks_mut::<4>().0.iter_mut() {
        pixel.swap(0, 2);
    }
    rgba
}

/// Copy `count` RGBA pixels from a raw chunk, zero-filling what is short.
fn raw(bytes: &[u8], count: usize) -> Vec<u8> {
    let mut out = vec![0u8; count * 4];
    let copy = bytes.len().min(out.len());
    out[..copy].copy_from_slice(&bytes[..copy]);
    out
}

/// Resolve 8-bit indices through a `pal` chunk, whose entries are RGBA quads.
fn expand_palette(
    bytes: &[u8],
    palette: &[u8],
    count: usize,
    compressed: bool,
) -> Result<Vec<u8>, TextureError> {
    let indices = if compressed {
        rl_decode(bytes, count, 1)
    } else {
        let mut indices = vec![0u8; count];
        let copy = bytes.len().min(count);
        indices[..copy].copy_from_slice(&bytes[..copy]);
        indices
    };
    let entries = palette.len() / 4;
    if entries == 0 {
        return Err(TextureError::EmptyPalette);
    }
    let mut out = Vec::with_capacity(count * 4);
    for index in indices {
        let entry = usize::from(index);
        if entry >= entries {
            return Err(TextureError::PaletteIndex { index, entries });
        }
        out.extend_from_slice(&palette[entry * 4..entry * 4 + 4]);
    }
    Ok(out)
}

/// `RL` run-length decode of `elements` values of `align` bytes each.
///
/// A marker with the high bit set repeats the next `align` bytes
/// `(marker & 0x7f) + 3` times; any other marker introduces `(marker + 1)`
/// literal values.
fn rl_decode(bytes: &[u8], elements: usize, align: usize) -> Vec<u8> {
    let mut out = vec![0u8; elements.saturating_mul(align)];
    let mut src = 0usize;
    let mut dst = 0usize;
    while src < bytes.len() && dst < out.len() {
        let marker = bytes[src];
        src += 1;
        if marker & 0x80 != 0 {
            let count = usize::from(marker & 0x7f) + 3;
            let Some(unit) = bytes.get(src..src + align) else {
                break;
            };
            for _ in 0..count {
                if dst + align > out.len() {
                    break;
                }
                out[dst..dst + align].copy_from_slice(unit);
                dst += align;
            }
            src += align;
        } else {
            let count = (usize::from(marker) + 1) * align;
            if src + count > bytes.len() {
                break;
            }
            let copy = count.min(out.len() - dst);
            out[dst..dst + copy].copy_from_slice(&bytes[src..src + copy]);
            src += count;
            dst += copy;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use std::sync::Arc;

    use api::media::{DecodedImage, ImageInfo};
    use engine::imagedecode::{DecodePool, ImageDecodeError, ImageDecoder};
    use engine::jobpool::JobPoolConfig;

    use super::{ChunkRequest, PixelCache, TextureError, decode_pixels, rl_decode};
    use crate::natives::plugins::psb_file::container::PsbValue;
    use crate::natives::plugins::psb_file::media::fixture::{chunk, container, media_storage};

    /// The storage name of the fixture's pixel chunk.
    const PIXEL: &str = "psb://cache.psb/source/group/icon/art/pixel";

    /// The storage name of the fixture's palette chunk.
    const PALETTE: &str = "psb://cache.psb/source/group/icon/art/pal";

    #[test]
    fn an_rl_stream_mixes_literals_and_runs() {
        // A literal marker (0x00) carries one value, a run marker (0x81)
        // repeats the next value `(0x81 & 0x7f) + 3` times, then one more
        // literal value completes six pixels.
        // `reference/cpp/plugins/motionplayer/PlayerInternal.h:63-102`.
        let mut stream = vec![0x00, 1, 2, 3, 4];
        stream.push(0x81);
        stream.extend_from_slice(&[9, 8, 7, 6]);
        stream.push(0x00);
        stream.extend_from_slice(&[5, 5, 5, 5]);
        let out = rl_decode(&stream, 6, 4);
        assert_eq!(
            out,
            vec![
                1, 2, 3, 4, 9, 8, 7, 6, 9, 8, 7, 6, 9, 8, 7, 6, 9, 8, 7, 6, 5, 5, 5, 5
            ]
        );
    }

    #[test]
    fn an_rl_stream_that_ends_early_leaves_the_rest_transparent() {
        // `reference/cpp/plugins/motionplayer/PlayerInternal.h:63-102`: the
        // reference allocates the full buffer zeroed and stops at the end of
        // the stream.
        let out = rl_decode(&[0x00, 1, 2, 3, 4], 4, 4);
        assert_eq!(out, vec![1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn a_raw_chunk_decodes_as_rgba() {
        let out = decode_pixels(&[1, 2, 3, 4, 5, 6, 7, 8], None, 2, 1, false)
            .expect("a raw chunk decodes");
        assert_eq!((out.width, out.height), (2, 1));
        assert_eq!(out.rgba, vec![3, 2, 1, 4, 7, 6, 5, 8]);
    }

    #[test]
    fn a_palette_chunk_resolves_each_index() {
        // Palette entries are RGBA quads; the indices are one byte per pixel
        // (`reference/cpp/plugins/motionplayer/PlayerInternal.h:132-165`).
        let palette = [10, 20, 30, 40, 50, 60, 70, 80];
        let out = decode_pixels(&[1, 0], Some(palette.as_slice()), 2, 1, false)
            .expect("a palettized chunk decodes");
        assert_eq!(out.rgba, vec![50, 60, 70, 80, 10, 20, 30, 40]);
    }

    #[test]
    fn a_palette_index_outside_the_palette_is_an_error() {
        let palette = [10, 20, 30, 40];
        assert_eq!(
            decode_pixels(&[0, 1], Some(palette.as_slice()), 2, 1, false),
            Err(TextureError::PaletteIndex {
                index: 1,
                entries: 1
            })
        );
    }

    #[test]
    fn a_palette_too_short_for_one_entry_is_an_error() {
        // An empty chunk counts as "no palette" (the raw path), but a chunk
        // that cannot hold one whole RGBA entry has nothing to index.
        let truncated = [10, 20, 30];
        assert_eq!(
            decode_pixels(&[0], Some(truncated.as_slice()), 1, 1, false),
            Err(TextureError::EmptyPalette)
        );
        let empty: &[u8] = &[];
        let raw = decode_pixels(&[1, 2, 3, 4], Some(empty), 1, 1, false)
            .expect("an empty palette falls back to the raw chunk");
        assert_eq!(raw.rgba, vec![3, 2, 1, 4]);
    }

    #[test]
    fn a_decoded_chunk_is_served_from_the_cache_until_it_is_cleared() {
        // The source cache keeps a decoded entry and only a clear drops it
        // (`reference/cpp/plugins/motionplayer/SourceCache.cpp:497-552`,
        // `:610-620`).
        let path = "psb://cache.psb/source/group/icon/art/pixel";
        let palette = "psb://cache.psb/source/group/icon/art/pal";
        let root = PsbValue::Dictionary(vec![(
            "source".to_string(),
            PsbValue::Dictionary(vec![(
                "group".to_string(),
                PsbValue::Dictionary(vec![(
                    "icon".to_string(),
                    PsbValue::Dictionary(vec![(
                        "art".to_string(),
                        PsbValue::Dictionary(vec![
                            ("pixel".to_string(), PsbValue::Resource(chunk(0))),
                            ("pal".to_string(), PsbValue::Resource(chunk(1))),
                        ]),
                    )]),
                )]),
            )]),
        )]);
        let document = Rc::new(container(root, vec![vec![1, 2, 3, 4], Vec::new()]));
        let storage = media_storage("cache.psb", Rc::clone(&document));
        let cache = PixelCache::default();
        let first = cache
            .get_or_decode(None, &storage, &request(path, palette, 1, 1))
            .expect("the registered chunk decodes");
        let read_bytes = storage.read_bytes();
        assert_eq!(read_bytes, 4);
        let second = cache
            .get_or_decode(None, &storage, &request(path, palette, 1, 1))
            .expect("the cached chunk decodes");
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(
            storage.read_bytes(),
            read_bytes,
            "a cache hit does not read the chunk again"
        );
        cache.clear();
        let third = cache
            .get_or_decode(None, &storage, &request(path, palette, 1, 1))
            .expect("the cleared chunk decodes again");
        assert!(!Arc::ptr_eq(&first, &third));
        assert_eq!(storage.read_bytes(), read_bytes * 2);
    }

    /// The request for the chunk at `path` with the palette at `palette`.
    fn request(path: &str, palette: &str, width: u32, height: u32) -> ChunkRequest {
        ChunkRequest {
            path: path.to_string(),
            palette_path: palette.to_string(),
            width,
            height,
            compressed: false,
        }
    }

    /// The pool's codec: the pixel chunks are decoded by their own job, so
    /// the codec is never asked for an image.
    struct UnusedCodec;

    impl ImageDecoder for UnusedCodec {
        fn probe(&self, _bytes: &[u8], _hint: Option<&str>) -> Result<ImageInfo, ImageDecodeError> {
            Err(ImageDecodeError::Failed {
                message: "the pool decodes no image".to_string(),
            })
        }

        fn decode(
            &self,
            _bytes: &[u8],
            _hint: Option<&str>,
        ) -> Result<DecodedImage, ImageDecodeError> {
            Err(ImageDecodeError::Failed {
                message: "the pool decodes no image".to_string(),
            })
        }
    }

    /// A pool with one worker.
    fn pool() -> DecodePool {
        DecodePool::new(
            Arc::new(UnusedCodec),
            JobPoolConfig {
                workers: 1,
                queue_limit: 8,
                memory_limit: 1024 * 1024,
            },
        )
        .expect("the pool starts")
    }

    /// A storage that serves one container with the given chunks.
    fn storage_with(chunks: Vec<Vec<u8>>) -> crate::engine_api::Storage {
        let root = PsbValue::Dictionary(vec![(
            "source".to_string(),
            PsbValue::Dictionary(vec![(
                "group".to_string(),
                PsbValue::Dictionary(vec![(
                    "icon".to_string(),
                    PsbValue::Dictionary(vec![(
                        "art".to_string(),
                        PsbValue::Dictionary(vec![
                            ("pixel".to_string(), PsbValue::Resource(chunk(0))),
                            ("pal".to_string(), PsbValue::Resource(chunk(1))),
                        ]),
                    )]),
                )]),
            )]),
        )]);
        let document = Rc::new(container(root, chunks));
        media_storage("cache.psb", document)
    }

    #[test]
    fn a_chunk_is_decoded_on_the_job_pool() {
        let storage = storage_with(vec![vec![1, 2, 3, 4], Vec::new()]);
        let mut pool = pool();
        let cache = PixelCache::default();
        let pixels = cache
            .get_or_decode(Some(&mut pool), &storage, &request(PIXEL, PALETTE, 1, 1))
            .expect("the chunk decodes");
        assert_eq!(pixels.rgba, vec![3, 2, 1, 4], "the chunk's pixels");
        assert_eq!(pool.stats().jobs, 1, "the pool ran the chunk's decode");
    }

    #[test]
    fn the_pool_decodes_the_same_pixels_as_the_drawing_thread() {
        let storage = storage_with(vec![vec![1, 2, 3, 4], Vec::new()]);
        let mut pool = pool();
        let pooled = PixelCache::default()
            .get_or_decode(Some(&mut pool), &storage, &request(PIXEL, PALETTE, 1, 1))
            .expect("the chunk decodes");
        let here = PixelCache::default()
            .get_or_decode(None, &storage, &request(PIXEL, PALETTE, 1, 1))
            .expect("the chunk decodes");
        assert_eq!(pooled.rgba, here.rgba, "the pixels are the same");
        assert_eq!(
            (pooled.width, pooled.height),
            (here.width, here.height),
            "the dimensions are the same"
        );
    }

    #[test]
    fn an_absent_chunk_has_no_pixels_and_runs_no_job() {
        let storage = storage_with(vec![vec![1, 2, 3, 4], Vec::new()]);
        let mut pool = pool();
        let cache = PixelCache::default();
        let absent = cache.get_or_decode(
            Some(&mut pool),
            &storage,
            &request(
                "psb://cache.psb/source/group/icon/absent/pixel",
                PALETTE,
                1,
                1,
            ),
        );
        assert!(absent.is_none(), "the absent chunk has no pixels");
        assert_eq!(pool.stats().jobs, 0, "the absent chunk ran no job");
    }

    #[test]
    fn a_chunk_that_cannot_be_decoded_has_no_pixels() {
        // The palette holds one entry and the chunk addresses the second one.
        let storage = storage_with(vec![vec![1], vec![10, 20, 30, 40]]);
        let mut pool = pool();
        let cache = PixelCache::default();
        let pixels = cache.get_or_decode(Some(&mut pool), &storage, &request(PIXEL, PALETTE, 1, 1));
        assert!(
            pixels.is_none(),
            "the chunk that cannot be decoded has no pixels"
        );
        assert_eq!(pool.stats().jobs, 1, "the pool ran the failing decode");
    }
}
