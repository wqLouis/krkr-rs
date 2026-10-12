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
//! [`FileSystem`] handle plus a [`PathBuf`], so the host can install the
//! codec before or after the engine binding and keeps no extra lifetime.

use std::path::{Path, PathBuf};

use api::fs::{FsPath, OpenMode};
use compat::CompatHost;
use engine::Storage;
use fs::FileSystem;
use media::{Media, decode_image, encode_image, probe_image};

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
    let fs = storage.file_system().clone();
    let base = storage.game_dir().to_path_buf();
    install_image_codec_on(host, media, fs, base);
}

/// Installs the `media` image codec with an explicit write target.
///
/// [`install_image_codec`] derives `(fs, base)` from a [`Storage`]; this
/// lower-level entry point is for hosts that keep the output elsewhere
/// (for example a dedicated screenshot directory).
pub fn install_image_codec_on(host: &CompatHost, media: &Media, fs: FileSystem, base: PathBuf) {
    let decode_media = media.clone();
    let probe_media = media.clone();
    let save_media = media.clone();

    host.set_image_codec(
        move |bytes, hint| {
            decode_image(&decode_media, bytes, hint).map_err(|error| error.to_string())
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

/// Writes `bytes` to `name` under `base` in `fs`, creating missing parents.
fn write_storage_file(
    fs: &FileSystem,
    base: &Path,
    name: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let path = base.join(name);
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
