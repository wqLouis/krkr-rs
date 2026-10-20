//! Wiring for the L1 `media` image codec into the L3 `compat` host.
//!
//! `compat` never links `media`; it exposes the
//! [`CompatHost::set_image_codec`](compat::CompatHost::set_image_codec) seam
//! and expects the composition root to fill it. `app` is that composition
//! root, and it also knows the L2 [`Storage`] the host reads game data from,
//! so this module is the single place that knows both.
//!
//! Decode and header probing delegate straight to the stateless
//! [`media::decode_image`]/[`media::probe_image`]. The L2 [`Storage`] API is
//! read-only, so the save closure encodes through [`media::encode_image`]
//! and writes the result through the storage's [`FileSystem`] at the
//! storage's game directory; a later [`Storage::read`] then sees the file.
//!
//! The installed closures are self-contained and own only a cloned
//! [`FileSystem`] handle, a [`PathBuf`], the shared [`StartupStats`] counter
//! set and a clone of the stateless backend, so the host can install the
//! codec before or after the engine binding and keeps no extra lifetime.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use api::fs::{FsPath, OpenMode};
use api::media::{DecodedImage, ImageInfo};
use compat::CompatHost;
use engine::Storage;
use engine::imagedecode::{ImageDecodeError, ImageDecoder};
use engine::storage::local_name_in;
use fs::FileSystem;
use media::{Media, decode_image, encode_image, probe_image};

use crate::startup::StartupStats;

/// Installs the `media`-backed image codec on `host`.
///
/// * `Bitmap.load`/`loadAsync` decode through [`media::decode_image`];
/// * `Bitmap.loadHeader` probes through [`media::probe_image`];
/// * `Bitmap.save` encodes through [`media::encode_image`] and writes the
///   bytes under `storage`'s game directory, so `Storage::read` (and thus a
///   later `load`) can read the saved image back.
///
/// Saved names may contain sub-directories (`"shots/out.png"`); missing
/// parents are created. An unknown save type is a typed encoder error
/// surfaced to the script, never a silently skipped write.
///
/// `media` is the same stateless L1 backend the composition root passes to
/// [`install_audio_decoder`](crate::install_audio_decoder); the closures own
/// a clone of it.
pub fn install_image_codec(host: &CompatHost, media: &Media, storage: &Storage) {
    let stats = StartupStats::new();
    install_image_codec_metered(host, media, storage, &stats);
}

/// Installs the image codec with the shared [`StartupStats`] counter set.
pub(crate) fn install_image_codec_metered(
    host: &CompatHost,
    media: &Media,
    storage: &Storage,
    stats: &Rc<StartupStats>,
) {
    let fs = storage.file_system().clone();
    let base = storage.game_dir().to_path_buf();
    install_image_codec_on_metered(host, media, fs, base, stats);
}

/// Installs the `media` image codec with an explicit write target.
///
/// [`install_image_codec`] derives `(fs, base)` from a [`Storage`]; this
/// lower-level entry point is for hosts that keep the output elsewhere
/// (for example a dedicated screenshot directory).
pub fn install_image_codec_on(host: &CompatHost, media: &Media, fs: FileSystem, base: PathBuf) {
    let stats = StartupStats::new();
    install_image_codec_on_metered(host, media, fs, base, &stats);
}

/// Installs the image codec with an explicit write target and the shared
/// [`StartupStats`] counter set.
fn install_image_codec_on_metered(
    host: &CompatHost,
    media: &Media,
    fs: FileSystem,
    base: PathBuf,
    stats: &Rc<StartupStats>,
) {
    let decode_media = media.clone();
    let probe_media = media.clone();
    let save_media = media.clone();
    let decode_stats = Rc::clone(stats);

    host.set_image_codec(
        move |bytes, hint| {
            let started = Instant::now();
            let result = decode_image(&decode_media, bytes, hint);
            decode_stats.record_image_decode(started.elapsed());
            result.map_err(|error| error.to_string())
        },
        move |bytes, hint| {
            probe_image(&probe_media, bytes, hint).map_err(|error| error.to_string())
        },
        move |name, type_name, width, height, rgba| {
            let bytes = encode_image(&save_media, width, height, rgba, type_name)
                .map_err(|error| error.to_string())?;
            write_storage_file(&fs, &base, name, &bytes)
        },
    );
}

/// The image codec the L2 decode pool drives.
///
/// The pool calls it from its worker threads, so the decoder is stateless and
/// thread-safe: it holds the bottom media backend and nothing else. The
/// bytes it decodes are the ones the compatibility layer read from the
/// storage, so the storage namespace never crosses a thread boundary.
struct MediaImageDecoder {
    /// The stateless bottom backend.
    media: Media,
}

impl ImageDecoder for MediaImageDecoder {
    fn probe(&self, bytes: &[u8], hint: Option<&str>) -> Result<ImageInfo, ImageDecodeError> {
        probe_image(&self.media, bytes, hint).map_err(|error| ImageDecodeError::Failed {
            message: error.to_string(),
        })
    }

    fn decode(&self, bytes: &[u8], hint: Option<&str>) -> Result<DecodedImage, ImageDecodeError> {
        decode_image(&self.media, bytes, hint).map_err(|error| ImageDecodeError::Failed {
            message: error.to_string(),
        })
    }
}

/// Installs the image decoder the L2 decode pool drives, and starts the
/// pool.
///
/// The decoder is the bottom media backend the pool calls; the pool owns the
/// worker threads, the queue and the byte accounting. Call once, before any
/// script runs: a load that runs while the pool is absent decodes
/// synchronously.
///
/// # Errors
///
/// Returns [`engine::imagedecode::DecodePoolError`] when the pool cannot be
/// started.
pub fn install_image_decoder(
    engine: &mut engine::Engine,
    media: &Media,
) -> Result<(), engine::imagedecode::DecodePoolError> {
    let decoder: Arc<dyn ImageDecoder> = Arc::new(MediaImageDecoder {
        media: media.clone(),
    });
    engine.install_decoder(decoder, engine::imagedecode::DecodePoolConfig::default())
}

/// Writes `bytes` to `name` under `base` in `fs`, creating missing parents.
///
/// `name` may be the fully lowercased absolute form `System.dataPath`
/// produces. Resolving it through the storage's case-correcting path helper
/// keeps the write on the real mixed-case game path instead of creating a
/// second lowercase tree (or failing on a read-only parent).
fn write_storage_file(
    fs: &FileSystem,
    base: &Path,
    name: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let path = match local_name_in(fs, base, name) {
        Ok(Some(path)) => path,
        Ok(None) => {
            let requested = Path::new(name);
            if requested.is_absolute() {
                requested.to_path_buf()
            } else {
                base.join(requested)
            }
        }
        Err(error) => return Err(error.to_string()),
    };
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(fs, &FsPath::host(parent)).map_err(|error| error.to_string())?;
    }
    let file =
        fs::open(fs, &FsPath::host(&path), OpenMode::Write).map_err(|error| error.to_string())?;
    fs::write_at(fs, &file, 0, bytes).map_err(|error| error.to_string())?;
    fs::flush(fs, &file).map_err(|error| error.to_string())?;
    Ok(())
}
