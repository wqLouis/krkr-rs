//! The visual `Bitmap` native class.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::engine_api::{
    BitmapState, DecodePass, DecodedImageKey, DecodedImageMode, PendingImage, Scene, Storage,
    StorageEntry, StorageReadError,
};
use api::geometry::PixelRegion;
use api::media::{DecodedImage, ImageInfo};
use tjs::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    NativeMethodDef, NativeStaticMembers, RetainedValue, Tjs2Engine, TjsValue, VAL_VOID, Value,
};

use super::common::{
    actual_color_bytes, bound, decode_pool_mut, resolve_object_id, rgba_to_pixel_word, scene_mut,
    scene_ref, set_retained_out,
};
use crate::collections::Map;
use crate::native_util::{args, report_error, set_bool, set_int, set_void, value_as_string};
use crate::natives::storages::extract_storage_ext;
use crate::natives::system::value_as_i64;
use crate::resource::ResourceResolver;
use crate::storage::strip_file_media;
use crate::{CompatCtx, CompatError, CompatServices};

/// `Bitmap` payload: the scene bitmap id plus the async-load flag.
#[derive(Default)]
struct BitmapInst {
    id: Option<u32>,
    loading: bool,
}

/// The scene id a `Bitmap` payload carries — the reader behind the VM's
/// native-instance lookup (`native_instance_payload("Bitmap")`), replacing
/// the removed `id`/`nativeId` script members for engine call sites.
pub(super) fn bitmap_payload_id(payload: *mut c_void) -> Option<u32> {
    // SAFETY: `native_instance_payload("Bitmap")` only hands out Bitmap payloads.
    unsafe { &*(payload as *const BitmapInst) }.id
}

/// The image extensions a request that carries none is probed with.
///
/// The one list every L3 image path probes with: the `Bitmap` path, the
/// `Layer` path and the layerEx image path. The graphic loader registers one
/// handler per extension and suggests the first one whose file exists, so the
/// list is the handler table.
const IMAGE_EXTENSION_PROBE: [&str; 21] = [
    ".webp", ".bmp", ".dib", ".jpeg", ".jpg", ".jif", ".png", ".tlg", ".tlg5", ".tlg6", ".gif",
    ".tif", ".tiff", ".tga", ".dds", ".pnm", ".ico", ".qoi", ".hdr", ".exr", ".ff",
];

/// The storage names an image request is looked up under, in order.
///
/// A request that carries an extension is looked up under that name and under
/// no other: the extension names the file, so no extension is suggested for
/// it. A request that carries none is looked up as given first and then with
/// each registered extension appended, and the first candidate that resolves
/// wins.
///
/// The one rule every L3 image path resolves through: the `Bitmap` path, the
/// `Layer` path and the layerEx image path.
pub(crate) fn image_candidates(requested: &str) -> Vec<String> {
    let lookup = strip_file_media(requested);
    if extract_storage_ext(lookup).is_some() {
        return vec![lookup.to_string()];
    }
    let mut candidates = Vec::with_capacity(IMAGE_EXTENSION_PROBE.len() + 1);
    candidates.push(lookup.to_string());
    candidates.extend(
        IMAGE_EXTENSION_PROBE
            .iter()
            .map(|extension| format!("{lookup}{extension}")),
    );
    candidates
}

/// The name a resolved image is decoded under.
///
/// A name that resolves as given keeps the spelling the script gave it; a
/// name that resolves through an extension carries the extension, so the
/// decoder picks its format from it.
pub(crate) fn resolved_image_name(requested: &str, candidate: &str) -> String {
    if candidate == strip_file_media(requested) {
        requested.to_string()
    } else {
        candidate.to_string()
    }
}

/// Why an image could not be loaded.
///
/// A storage failure keeps the storage's own message, so it must not be
/// reworded by the caller: the words the storage raised are the words the
/// script sees.
#[derive(Debug, Clone)]
pub(crate) enum ImageLoadError {
    /// The storage could not deliver the bytes.
    Storage(StorageReadError),
    /// The image could not be decoded or installed.
    Message(String),
}

impl ImageLoadError {
    /// The message the script sees.
    pub(crate) fn message(&self) -> String {
        match self {
            Self::Storage(error) => error.to_string(),
            Self::Message(message) => message.clone(),
        }
    }

    /// The message the script sees, with `prefix` in front of a failure that
    /// is not the storage's own.
    pub(crate) fn message_with(&self, prefix: &str) -> String {
        match self {
            Self::Storage(error) => error.to_string(),
            Self::Message(message) => format!("{prefix}{message}"),
        }
    }
}

impl std::fmt::Display for ImageLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl From<StorageReadError> for ImageLoadError {
    fn from(error: StorageReadError) -> Self {
        Self::Storage(error)
    }
}

impl From<String> for ImageLoadError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

/// Read `name` through the attached L2 [`Storage`], resolving it to the
/// storage names an image request is looked up under.
///
/// A request that carries an extension is read under that name alone; a
/// request that carries none is read as given first and then under each
/// registered extension.
pub(crate) fn read_image_from_storage(
    storage: &Storage,
    name: &str,
) -> Result<(String, Vec<u8>), ImageLoadError> {
    let resolved = resolve_image(storage, name)?;
    let resolved_name = resolved.name().to_string();
    let bytes = resolved.bytes(storage)?;
    Ok((resolved_name, bytes))
}

/// A resolved image: the name it resolved under and the entry that holds it.
///
/// The entry of the default media is a **scheduled read**: the bytes are
/// produced by the storage's read pool, off the thread that resolved the
/// name, and the caller waits for them when it needs them. The entry of a
/// storage media is read by its provider, on the thread that asks for the
/// bytes: a provider owns state that belongs to that thread — the `var://`
/// media walks the script object graph, and that walk may not run on a
/// worker.
pub(crate) enum ResolvedImage {
    /// A file of the default media; its read is scheduled.
    File {
        /// The name the image resolved under.
        name: String,
        /// The entry the image resolved to.
        entry: StorageEntry,
    },
    /// An entry a storage media serves; the provider answered with the bytes.
    Media {
        /// The name the image resolved under.
        name: String,
        /// The bytes the provider answered with.
        bytes: Vec<u8>,
    },
}

impl ResolvedImage {
    /// The name the image resolved under.
    ///
    /// A name that resolves as given keeps the spelling the script gave it;
    /// a name that resolves through an extension carries the extension, so
    /// the decoder picks its format from it.
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::File { name, .. } | Self::Media { name, .. } => name,
        }
    }

    /// The bytes of the entry, waiting for the scheduled read.
    ///
    /// # Errors
    ///
    /// The storage's own error: a read that failed, or one that was
    /// cancelled before it produced a result.
    pub(crate) fn bytes(self, storage: &Storage) -> Result<Vec<u8>, ImageLoadError> {
        match self {
            Self::File { entry, .. } => storage.read_entry(&entry).map_err(ImageLoadError::Storage),
            Self::Media { bytes, .. } => Ok(bytes),
        }
    }
}

/// Resolves an image name into the entry that holds it.
///
/// The candidates are the storage names an image request is looked up under:
/// a request that carries an extension is resolved under that name alone, and
/// a request that carries none is resolved as given first and then under each
/// registered extension.
///
/// A candidate is **resolved**, never read: the default media's entry is
/// looked up and its read is scheduled, and a media's entry is read by its
/// provider. A candidate that does not resolve costs a lookup and no read at
/// all.
pub(crate) fn resolve_image(
    storage: &Storage,
    name: &str,
) -> Result<ResolvedImage, ImageLoadError> {
    for candidate in image_candidates(name) {
        match storage.resolve_entry(&candidate)? {
            Some(StorageEntry::File {
                name: entry_name,
                pending,
            }) => {
                return Ok(ResolvedImage::File {
                    name: resolved_image_name(name, &candidate),
                    entry: StorageEntry::File {
                        name: entry_name,
                        pending,
                    },
                });
            }
            Some(StorageEntry::Media { provider, path }) => {
                // A media's entry is read by its provider, here: the
                // provider owns state that belongs to this thread, and the
                // `var://` media's script-graph walk may not run on a
                // worker.
                let media = StorageEntry::Media { provider, path };
                match storage.read_entry(&media) {
                    Ok(bytes) => {
                        return Ok(ResolvedImage::Media {
                            name: resolved_image_name(name, &candidate),
                            bytes,
                        });
                    }
                    // The media holds no such entry: the next candidate is
                    // tried.
                    Err(StorageReadError::NotFound(_)) => {}
                    Err(error) => return Err(ImageLoadError::Storage(error)),
                }
            }
            // The storage holds no such entry: the next candidate is tried.
            None => {}
        }
    }
    Err(ImageLoadError::Storage(StorageReadError::NotFound(
        name.to_string(),
    )))
}

/// The province image's name for a graphic: `_p` before the extension.
///
/// `foo.png` → `foo_p.png`, `foo` → `foo_p`, `dir/foo.png` → `dir/foo_p.png`.
/// The media prefix is part of the path and is kept.
fn province_image_name(name: &str) -> String {
    // The extension starts after the last path separator: the media prefix
    // (`file://.`) carries a dot of its own.
    let path = name.rfind('/').map_or(0, |at| at + 1);
    match name[path..].rfind('.') {
        Some(at) => {
            let at = path + at;
            format!("{}_p{}", &name[..at], &name[at..])
        }
        None => format!("{name}_p"),
    }
}

/// The province image's storage name for a graphic, when the storage holds
/// it.
///
/// The name carries `_p` before the extension and is resolved as an image
/// name is: it carries an extension of its own, so it is looked up under that
/// name alone. A storage without that image has no province image.
pub(crate) fn resolve_province_image(
    storage: &Storage,
    name: &str,
) -> Result<Option<String>, ImageLoadError> {
    let province_name = province_image_name(name);
    match resolve_image(storage, &province_name) {
        Ok(resolved) => Ok(Some(resolved.name().to_string())),
        // The storage holds no province image for this graphic.
        Err(ImageLoadError::Storage(StorageReadError::NotFound(_))) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Decode image bytes through the installed host decoder.
pub(crate) fn decode_image(
    services: &CompatServices,
    name: &str,
    bytes: &[u8],
) -> Result<DecodedImage, String> {
    services
        .state::<ResourceResolver>()
        .decode(bytes, Some(name))
}

/// The host decoder that reads an image's 8-bit index plane.
///
/// The plane is the image's own 8-bit sample — the palette index of a
/// palettized image, the gray level of a grayscale one — and it travels as a
/// gray image: every pixel's red, green and blue channels carry the plane
/// value.
pub(crate) type IndexPlaneDecodeFn = dyn Fn(&[u8], Option<&str>) -> Result<DecodedImage, String>;

/// The installed index-plane decoder.
#[derive(Default)]
pub(crate) struct IndexPlaneDecoder {
    /// The decoder, absent until the host installs one.
    decoder: Option<Rc<IndexPlaneDecodeFn>>,
}

/// Install the index-plane decoder the province load composes.
pub(crate) fn set_index_plane_decoder(
    services: &CompatServices,
    decoder: impl Fn(&[u8], Option<&str>) -> Result<DecodedImage, String> + 'static,
) {
    services.state::<IndexPlaneDecoder>().decoder = Some(Rc::new(decoder));
}

/// The installed index-plane decoder, if the host installed one.
///
/// A host that installed no decoder gets a typed error instead of a colour
/// image: the plane is not the image's colour.
fn installed_index_plane_decoder(
    services: &CompatServices,
) -> Result<Rc<IndexPlaneDecodeFn>, String> {
    let decoder = {
        let state = services.state::<IndexPlaneDecoder>();
        state.decoder.clone()
    };
    decoder.ok_or_else(|| "no index-plane decoder is installed".to_string())
}

/// Probe image bytes through the installed host prober.
fn probe_image(services: &CompatServices, name: &str, bytes: &[u8]) -> Result<ImageInfo, String> {
    services
        .state::<ResourceResolver>()
        .probe(bytes, Some(name))
}

/// `TVP_clNone`: no color key.
pub(crate) const COLOR_KEY_NONE: u32 = 0x1fff_ffff;
/// `TVP_clAdapt`: the most frequent color on the first scanline.
const COLOR_KEY_ADAPT: u32 = 0x01ff_ffff;

/// Apply the reference color-key transparency to a decoded RGBA8 image
/// (`TVPMakeAlphaFromKey`): matching pixels become transparent, all others
/// opaque.
pub(crate) fn apply_color_key(rgba: &mut [u8], width: u32, keyidx: u32) {
    if keyidx == COLOR_KEY_NONE {
        return;
    }
    if keyidx == COLOR_KEY_ADAPT {
        make_alpha_from_key(rgba, adaptive_color_key(rgba, width));
        return;
    }
    if keyidx & 0xff00_0000 == 0 {
        make_alpha_from_key(rgba, keyidx & 0x00ff_ffff);
    }
}

/// The most frequent RGB value on the first scanline.
fn adaptive_color_key(rgba: &[u8], width: u32) -> u32 {
    let mut counts: Map<u32, u32> = Map::default();
    let mut best = 0u32;
    let mut best_count = 0u32;
    for pixel in rgba.as_chunks::<4>().0.iter().take(width as usize) {
        let rgb = (u32::from(pixel[0]) << 16) | (u32::from(pixel[1]) << 8) | u32::from(pixel[2]);
        let count = counts.entry(rgb).or_insert(0);
        *count += 1;
        if *count > best_count {
            best_count = *count;
            best = rgb;
        }
    }
    best
}

/// Make `key`-colored pixels transparent and everything else opaque.
fn make_alpha_from_key(rgba: &mut [u8], key: u32) {
    for pixel in rgba.as_chunks_mut::<4>().0.iter_mut() {
        let rgb = (u32::from(pixel[0]) << 16) | (u32::from(pixel[1]) << 8) | u32::from(pixel[2]);
        pixel[3] = if rgb == key { 0 } else { 255 };
    }
}

/// The pass a decode worker runs for a colour-keyed load, when the load has
/// one: the key transformation over the decoded pixels.
///
/// The pass owns no frame-thread state, so it runs on the pool's worker with
/// the decode it belongs to; the load installs the keyed pixels the worker
/// produced.
fn color_key_pass(colorkey: u32) -> Option<Arc<DecodePass>> {
    if colorkey == COLOR_KEY_NONE {
        return None;
    }
    Some(Arc::new(move |image: DecodedImage| {
        if !image.is_consistent() {
            return Err("the decoded image is malformed".to_string());
        }
        let (width, height) = (image.width, image.height);
        let mut rgba = image.rgba;
        apply_color_key(&mut rgba, width, colorkey);
        Ok(DecodedImage::new(width, height, rgba))
    }))
}

/// The metainfo dictionary returned by `Bitmap.load`/`Bitmap.loadAsync` (the
/// game-visible `width`/`height`/`bpp` subset of the reference meta tags).
fn metainfo_object(
    host: &Tjs2Engine,
    width: u32,
    height: u32,
    bpp: u32,
) -> Result<DetachedValue, String> {
    let expression = format!("%[\"width\"=>{width}, \"height\"=>{height}, \"bpp\"=>{bpp}]");
    match host.eval_retained(&expression, "Bitmap.metainfo") {
        Ok(RetainedValue::Object(object)) => Ok(object),
        Ok(RetainedValue::Value(_)) => Err("the metainfo expression was not an object".to_string()),
        Err(error) => Err(error.to_string()),
    }
}

/// The save-option dictionary expression for `type`, or `None` when no
/// handler accepts it (the reference `TVPGetSaveOption` then yields `void`).
fn save_option_expression(type_name: &str) -> Option<&'static str> {
    let lower = type_name.to_ascii_lowercase();
    if lower.starts_with("bmp") || lower == ".bmp" || lower == ".dib" {
        Some(r#"%["bpp"=>%["type"=>"select","items"=>["32","24","8"],"desc"=>"bpp","default"=>0]]"#)
    } else if lower.starts_with("png") || lower == ".png" {
        Some("%[]")
    } else if lower.starts_with("jpg") || lower == ".jpg" || lower == ".jpeg" || lower == ".jif" {
        Some(
            r#"%["quality"=>%["type"=>"range","min"=>1,"max"=>100,"desc"=>"100 is high quality, 1 is low quality","default"=>90],"subsampling"=>%["type"=>"select","items"=>["4:2:0","4:2:2","4:4:4"],"desc"=>"subsampling","default"=>0],"dct"=>%["type"=>"select","items"=>["islow","ifast","float"],"desc"=>"DCT method","default"=>0],"progressive"=>%["type"=>"boolean","desc"=>"progressive","default"=>true]]"#,
        )
    } else if lower.starts_with("tlg") || lower == ".tlg" || lower == ".tlg5" || lower == ".tlg6" {
        Some(
            r#"%["mode"=>%["type"=>"select text","items"=>["opaque","alpha","add","sub","mul","dodge","darken","lighten","screen","addalpha","psnormal","psadd","pssub","psmul","psscreen","psoverlay","pshlight","psslight","psdodge","psdodge5","psburn","pslighten","psdarken","psdiff","psdiff5","psexcl"],"desc"=>"blending mode","default"=>"alpha"],"offs_x"=>%["type"=>"integer","desc"=>"offset x","default"=>0],"offs_y"=>%["type"=>"integer","desc"=>"offset y","default"=>0],"offs_unit"=>%["type"=>"select text","items"=>["pixel"],"desc"=>"offset unit","default"=>"pixel"]]"#,
        )
    } else {
        None
    }
}

/// Read `name` through the attached storage and decode it. This is the
/// uncached decode body the scene's decoded-image cache wraps.
///
/// The `mode` picks the decode: a full-colour load reads the image's pixels
/// and applies the color key, a province load reads the image's 8-bit index
/// plane — the palette index of a palettized image, not the colour the
/// palette maps it to. The plane travels as a gray image, so its red channel
/// is the plane value.
pub(crate) fn decode_storage_image_uncached(
    ctx: &CompatCtx,
    name: &str,
    colorkey: u32,
    mode: DecodedImageMode,
) -> Result<DecodedImage, ImageLoadError> {
    // SAFETY: `ctx` is only created for a bound, live engine, so its service pointer is valid for `decode_storage_image_uncached`.
    let services = unsafe { ctx.services() };
    let storage = services
        .storage()
        .ok_or_else(|| "no storage attached".to_string())?;
    let started = Instant::now();
    // SAFETY: `attach_storage` guarantees the pointer stays valid.
    let (resolved, bytes) = read_image_from_storage(unsafe { &*storage }, name)?;
    let read_ms = started.elapsed().as_secs_f64() * 1000.0;
    let decode_started = Instant::now();
    let image = match mode {
        DecodedImageMode::Full => {
            let image = decode_image(services, &resolved, &bytes)?;
            if !image.is_consistent() {
                return Err(ImageLoadError::Message(
                    "the decoded image is malformed".to_string(),
                ));
            }
            let (width, height) = (image.width, image.height);
            let mut rgba = image.rgba;
            apply_color_key(&mut rgba, width, colorkey);
            DecodedImage::new(width, height, rgba)
        }
        DecodedImageMode::Province => {
            // The province plane is the image's own 8-bit sample; the color
            // key does not apply to it.
            let decoder = installed_index_plane_decoder(services)?;
            let image = decoder(&bytes, Some(resolved.as_str()))?;
            if !image.is_consistent() {
                return Err(ImageLoadError::Message(
                    "the decoded image is malformed".to_string(),
                ));
            }
            image
        }
    };
    report_slow_sync_load(
        services,
        name,
        started.elapsed(),
        read_ms,
        decode_started.elapsed().as_secs_f64() * 1000.0,
    );
    Ok(image)
}

/// Load `name` through the scene's decoded-image cache (the reference
/// `TVPLoadGraphic` cache path,
/// `reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1878-1960`). The cache is
/// keyed by the requested name, color key, mode and desired size; the decode
/// body runs only on a miss, or while the cache is disabled.
pub(crate) fn load_cached_storage_image(
    ctx: &CompatCtx,
    name: &str,
    colorkey: u32,
    mode: DecodedImageMode,
) -> Result<DecodedImage, ImageLoadError> {
    let key = DecodedImageKey::new(name, colorkey, mode, 0, 0);
    let scene = scene_mut(ctx)?;
    scene.cached_decoded_image(key, || {
        decode_storage_image_uncached(ctx, name, colorkey, mode)
    })
}

/// Decode the storage image `name` into RGBA8, applying the color key, through
/// the cache.
fn decode_storage_image(
    ctx: &CompatCtx,
    name: &str,
    colorkey: u32,
) -> Result<DecodedImage, ImageLoadError> {
    load_cached_storage_image(ctx, name, colorkey, DecodedImageMode::Full)
}

/// Load `name` into bitmap `id`, allocating the scene bitmap when the payload
/// is still the reference's empty initial bitmap. Returns `(id, width,
/// height)`. Shared by the constructor, `load` and `loadAsync`.
///
/// `defer` asks for the decode to run on the pool: the call returns with the
/// image's size known and its pixels still decoding, and every read of those
/// pixels waits for the decode. `loadAsync` does not defer: it reports the
/// load's result to the script, so the decode must have run.
fn load_into_bitmap(
    ctx: &CompatCtx,
    id: Option<u32>,
    name: &str,
    colorkey: u32,
    defer: bool,
) -> Result<(u32, u32, u32), ImageLoadError> {
    // The decoded-image cache comes first: a hit is installed synchronously,
    // exactly as a cached graphic is installed.
    let key = DecodedImageKey::new(name, colorkey, DecodedImageMode::Full, 0, 0);
    let cached = {
        let scene = scene_mut(ctx)?;
        scene.lookup_decoded_image(&key)
    };
    if let Some(image) = cached {
        return install_decoded_image(ctx, id, image);
    }
    if defer
        && let Some((width, height, pending)) =
            start_pending_decode(ctx, name, colorkey, DecodedImageMode::Full)?
    {
        let id = install_pending_image(ctx, id, width, height, pending)?;
        let scene = scene_mut(ctx)?;
        // The image is the script `Bitmap`'s, so it is released with that
        // object; a layer's own image stays layer-owned.
        scene.set_bitmap_script_owned(id);
        return Ok((id, width, height));
    }
    // The synchronous path: the decode runs here and the cache keeps it.
    let image = decode_storage_image(ctx, name, colorkey)?;
    install_decoded_image(ctx, id, image)
}

/// Installs a decoded image as the image of `id`, or of a new bitmap.
///
/// The failure is the scene's own: no scene attached, or the bitmap that is
/// gone. It is a message, not a storage failure, so the caller keeps its own
/// prefix.
fn install_decoded_image(
    ctx: &CompatCtx,
    id: Option<u32>,
    image: DecodedImage,
) -> Result<(u32, u32, u32), ImageLoadError> {
    let (width, height) = (image.width, image.height);
    let rgba = image.rgba;
    let scene = scene_mut(ctx)?;
    let id = match id {
        Some(id) => {
            scene.replace_bitmap_image(id, width, height, rgba);
            id
        }
        None => scene.add_bitmap(width, height, rgba),
    };
    // The image is the script `Bitmap`'s, so it is released with that
    // object; a layer's own image stays layer-owned.
    scene.set_bitmap_script_owned(id);
    Ok((id, width, height))
}

/// A load that took longer than this many milliseconds is reported in the
/// log.
///
/// The threshold is a few tens of milliseconds: a faster load is a normal
/// one, and only the slow ones are reported.
const SLOW_LOAD_MS: f64 = 50.0;

/// The minimum interval between two reports of a slow load.
///
/// A load that runs in every frame would report in every frame; the
/// interval keeps the log readable while the problem stays visible.
const SLOW_REPORT_INTERVAL: Duration = Duration::from_secs(5);

/// The reporter that keeps the slow-load report bounded.
///
/// The reporter holds the time of its last report, so a load that is slow
/// in every frame is reported once per interval instead of once per frame.
#[derive(Default)]
struct SlowLoadReporter {
    /// The time of the last report.
    last: Option<Instant>,
}

impl SlowLoadReporter {
    /// Whether a report is due: the last one is at least the interval old.
    ///
    /// The check is what keeps the report bounded: a load that is slow in
    /// every frame is reported once per interval instead of once per frame.
    fn is_due(&mut self) -> bool {
        let now = Instant::now();
        let due = self
            .last
            .is_none_or(|last| now.duration_since(last) >= SLOW_REPORT_INTERVAL);
        if due {
            self.last = Some(now);
        }
        due
    }
}

/// Whether a load that took `elapsed` is slow enough to report.
///
/// The threshold is what keeps the report quiet: a faster load is a normal
/// one.
fn is_slow(elapsed: Duration) -> bool {
    elapsed.as_secs_f64() * 1000.0 >= SLOW_LOAD_MS
}

/// The phases of one image load.
///
/// The load is timed as a whole and in its phases: the name resolution (the
/// lookup and the scheduling of the read), the storage read (the bytes the
/// scheduled read produces) and the header probe (the decoder's look at the
/// header, which yields the image's size).
#[derive(Clone, Copy, Default)]
struct LoadPhases {
    /// The time the name resolution took.
    resolve_ms: f64,
    /// The time the storage read took.
    read_ms: f64,
    /// The time the header probe took.
    probe_ms: f64,
}

/// Reports a load that took longer than the threshold.
///
/// The line names the phases, so a reader can see which one cost what. The
/// report is bounded: the reporter keeps the interval between two reports,
/// so a load that is slow in every frame does not report in every frame.
fn report_slow_load(services: &CompatServices, name: &str, elapsed: Duration, phases: LoadPhases) {
    if !is_slow(elapsed) {
        return;
    }
    let mut reporter = services.state::<SlowLoadReporter>();
    if !reporter.is_due() {
        return;
    }
    log::warn!(
        "image: the load of {name:?} took {:.1} ms (resolve={:.1} storage_read={:.1} header_probe={:.1})",
        elapsed.as_secs_f64() * 1000.0,
        phases.resolve_ms,
        phases.read_ms,
        phases.probe_ms,
    );
}

/// Reports a synchronous load that took longer than the threshold.
///
/// The line names the phases: the storage read (the name resolution and the
/// bytes it produced) and the decode, which runs on the calling thread on
/// this path. The report is bounded like the deferred one.
fn report_slow_sync_load(
    services: &CompatServices,
    name: &str,
    elapsed: Duration,
    read_ms: f64,
    decode_ms: f64,
) {
    if !is_slow(elapsed) {
        return;
    }
    let mut reporter = services.state::<SlowLoadReporter>();
    if !reporter.is_due() {
        return;
    }
    log::warn!(
        "image: the synchronous load of {name:?} took {:.1} ms (storage_read={read_ms:.1} decode={decode_ms:.1})",
        elapsed.as_secs_f64() * 1000.0,
    );
}

/// A pending image load: the decode that is already running for the image,
/// or the entry the load reads.
///
/// The load is resolved and submitted in two steps, so a caller that loads a
/// burst of images can resolve them all before it submits any of them: the
/// resolution schedules the reads, and the storage's read pool runs them
/// concurrently.
pub(crate) enum PendingLoad {
    /// A decode that is already running for this image: the load shares it.
    Running(u32, u32, PendingImage),
    /// The entry the load reads and decodes.
    Entry {
        /// The name the script asked for; the cache key is built from it.
        name: String,
        /// The entry the load reads.
        entry: ResolvedImage,
        /// The time the name resolution took: the lookup and the scheduling
        /// of the read.
        resolve: Duration,
    },
}

/// Resolves an image load into the decode that is already running for it, or
/// the entry it reads.
///
/// The resolution is a **lookup**: the name is resolved against the storage
/// namespace and the entry's read is scheduled on the storage's read pool,
/// so the bytes are produced off the thread that resolved the name. The
/// storage namespace is not shared with the workers.
///
/// Returns `None` when the deferred path does not apply: no pool is
/// attached, or the load is a province load. The caller then decodes
/// synchronously.
///
/// # Errors
///
/// Returns the resolution failure. It is raised at the call, the way a load
/// failure is raised there: a name that does not resolve never reaches the
/// pool.
pub(crate) fn resolve_pending_load(
    ctx: &CompatCtx,
    name: &str,
    colorkey: u32,
    mode: DecodedImageMode,
) -> Result<Option<PendingLoad>, ImageLoadError> {
    // The province plane is a separate target whose reads cannot wait for a
    // decode, so only a full-colour image is deferred.
    if mode != DecodedImageMode::Full {
        return Ok(None);
    }
    // Without a pool the load decodes synchronously.
    if decode_pool_mut(ctx).is_none() {
        return Ok(None);
    }
    // The key depends on the load's inputs alone, so it is known before the
    // bytes are read.
    let key = DecodedImageKey::new(name, colorkey, mode, 0, 0);
    // A decode that is already running for the same image is shared: the
    // load reads no bytes, probes no header and submits no second job. Every
    // holder of the shared decode waits for the same pixels.
    let running = {
        let scene = scene_ref(ctx)?;
        scene.pending_decode_for(&key)
    };
    if let Some((width, height, pending)) = running {
        return Ok(Some(PendingLoad::Running(width, height, pending)));
    }
    // The resolution and the extension resolution stay on this thread: the
    // storage namespace is not shared with the workers.
    // SAFETY: `ctx` is only created for a bound, live engine, so its service
    // pointer is valid for `resolve_pending_load`.
    let services = unsafe { ctx.services() };
    let storage = services
        .storage()
        .ok_or_else(|| "no storage attached".to_string())?;
    let started = Instant::now();
    // SAFETY: `attach_storage` guarantees the pointer stays valid.
    let entry = resolve_image(unsafe { &*storage }, name)?;
    Ok(Some(PendingLoad::Entry {
        name: name.to_string(),
        entry,
        resolve: started.elapsed(),
    }))
}

/// Submits the decode of a resolved load, probing its header first.
///
/// The probe runs on the calling thread: the load's size is known before the
/// call returns, and the probe's failure is raised at the call, the way a
/// load failure is raised there.
///
/// The load is timed as a whole and in its phases, and a load that took
/// longer than the threshold is reported with the phase times.
///
/// Returns the probed size and the future the result lands in, or `None` when
/// the deferred path does not apply: the pool refused the job. The caller
/// then decodes synchronously. A colour key does not keep the load
/// synchronous: the key is a pass over the decoded pixels, and the pool runs
/// it on the worker.
///
/// # Errors
///
/// Returns the read or the probe failure. Both are raised at the call, the
/// way a load failure is raised there: a name that does not resolve and an
/// image the codec does not recognise never reach the pool.
pub(crate) fn submit_pending_load(
    ctx: &CompatCtx,
    load: PendingLoad,
    colorkey: u32,
    mode: DecodedImageMode,
) -> Result<Option<(u32, u32, PendingImage)>, ImageLoadError> {
    let started = Instant::now();
    let (name, entry, resolve) = match load {
        // A decode that is already running for this image: the load shares
        // it.
        PendingLoad::Running(width, height, pending) => {
            return Ok(Some((width, height, pending)));
        }
        PendingLoad::Entry {
            name,
            entry,
            resolve,
        } => (name, entry, resolve),
    };
    let mut phases = LoadPhases {
        resolve_ms: resolve.as_secs_f64() * 1000.0,
        ..LoadPhases::default()
    };
    let result = submit_resolved_load(ctx, &name, entry, colorkey, mode, &mut phases);
    // SAFETY: `ctx` is only created for a bound, live engine, so its service
    // pointer is valid for `submit_pending_load`.
    let services = unsafe { ctx.services() };
    report_slow_load(services, &name, resolve + started.elapsed(), phases);
    result
}

/// Submits the decode of a resolved load, probing its header first.
///
/// The probe runs on the calling thread: the load's size is known before the
/// call returns, and the probe's failure is raised at the call, the way a
/// load failure is raised there.
///
/// The phases the load went through are recorded in `phases`: the storage
/// read and the header probe.
///
/// Returns the probed size and the future the result lands in, or `None` when
/// the deferred path does not apply: the pool refused the job. The caller
/// then decodes synchronously. A colour key does not keep the load
/// synchronous: the key is a pass over the decoded pixels, and the pool runs
/// it on the worker.
///
/// # Errors
///
/// Returns the read or the probe failure. Both are raised at the call, the
/// way a load failure is raised there: a name that does not resolve and an
/// image the codec does not recognise never reach the pool.
fn submit_resolved_load(
    ctx: &CompatCtx,
    name: &str,
    entry: ResolvedImage,
    colorkey: u32,
    mode: DecodedImageMode,
    phases: &mut LoadPhases,
) -> Result<Option<(u32, u32, PendingImage)>, ImageLoadError> {
    // SAFETY: `ctx` is only created for a bound, live engine, so its service
    // pointer is valid for `submit_resolved_load`.
    let services = unsafe { ctx.services() };
    let Some(pool) = decode_pool_mut(ctx) else {
        return Ok(None);
    };
    let key = DecodedImageKey::new(name, colorkey, mode, 0, 0);
    // The name the image resolved under: the decoder picks its format from
    // it.
    let resolved_name = entry.name().to_string();
    // The bytes the scheduled read produced.
    let Some(storage_ptr) = services.storage() else {
        return Err(ImageLoadError::Message("no storage attached".to_string()));
    };
    // SAFETY: `attach_storage` guarantees the pointer stays valid.
    let storage = unsafe { &*storage_ptr };
    let read_started = Instant::now();
    let bytes = entry.bytes(storage)?;
    phases.read_ms = read_started.elapsed().as_secs_f64() * 1000.0;
    // The header probe gives the dimensions the image is sized from, and it
    // rejects an image the load would have rejected at the call.
    let probe_started = Instant::now();
    let info = probe_image(services, &resolved_name, &bytes)?;
    phases.probe_ms = probe_started.elapsed().as_secs_f64() * 1000.0;
    let Some(expected) = decoded_size_bytes(info.width, info.height) else {
        return Ok(None);
    };
    let bytes: Arc<[u8]> = Arc::from(bytes);
    let hint = Some(resolved_name);
    // A colour key is a pass over the decoded pixels: the worker applies it,
    // so the frame thread never sees the unkeyed image and never runs the
    // pass.
    let pass = color_key_pass(colorkey);
    match pool.submit_cached(key, bytes, hint, expected, pass) {
        Ok((_id, pending)) => Ok(Some((info.width, info.height, pending))),
        Err(error) => {
            // The pool refused the job: the load decodes synchronously, and
            // the refusal is reported rather than swallowed. The error names
            // the limit the pool refused the job against.
            log::warn!(
                "image: the decode pool refused {name:?} ({error}); the load decodes synchronously on the calling thread"
            );
            Ok(None)
        }
    }
}

/// Starts a pending decode for `name`, when the pool can take it.
///
/// Returns the probed size and the future the result lands in, or `None` when
/// the deferred path does not apply: no pool is attached, the load is a
/// province load, or the pool refused the job. The caller then decodes
/// synchronously. A colour key does not keep the load synchronous: the key is
/// a pass over the decoded pixels, and the pool runs it on the worker.
///
/// A decode that is already running for the same image is shared: the load
/// takes the future of that decode instead of reading the bytes, probing the
/// header and submitting a second job. Every holder of the shared future
/// waits for the same pixels.
///
/// # Errors
///
/// Returns the read or the probe failure. Both are raised at the call, the
/// way a load failure is raised there: a name that does not resolve and an
/// image the codec does not recognise never reach the pool.
pub(crate) fn start_pending_decode(
    ctx: &CompatCtx,
    name: &str,
    colorkey: u32,
    mode: DecodedImageMode,
) -> Result<Option<(u32, u32, PendingImage)>, ImageLoadError> {
    let Some(load) = resolve_pending_load(ctx, name, colorkey, mode)? else {
        return Ok(None);
    };
    submit_pending_load(ctx, load, colorkey, mode)
}

/// Installs a pending decode as the image of `id`, or of a new bitmap.
///
/// The bitmap carries the probed size and no pixels: the decode result lands
/// in it when the scene resolves it. Returns the bitmap id.
pub(crate) fn install_pending_image(
    ctx: &CompatCtx,
    id: Option<u32>,
    width: u32,
    height: u32,
    pending: PendingImage,
) -> Result<u32, String> {
    let scene = scene_mut(ctx)?;
    let id = match id {
        Some(id) => {
            if !scene.set_pending_image(id, width, height, pending) {
                return Err("the bitmap is gone".to_string());
            }
            id
        }
        None => scene
            .add_pending_bitmap(width, height, pending)
            .map_err(|error| error.to_string())?,
    };
    Ok(id)
}

/// The number of bytes an image of `width` x `height` RGBA8 pixels holds.
///
/// `None` when the count is not representable.
fn decoded_size_bytes(width: u32, height: u32) -> Option<u64> {
    let pixels = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)?;
    u64::try_from(pixels).ok()
}

// ---------------------------------------------------------------------------
// Bitmap
// ---------------------------------------------------------------------------

extern "C" fn bitmap_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(BitmapInst::default())) as *mut c_void
}

/// Release the scene image this `Bitmap` owns. A layer still referencing the
/// image keeps it alive (`Scene::release_script_bitmap`). Idempotent, so it
/// is safe to run from both the invalidate and the destroy hook.
extern "C" fn bitmap_release_image(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` is the live `BitmapInst` payload the VM allocated for this script object; no `&mut` to it is live here.
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        return;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return;
    };
    if let Ok(scene) = scene_mut(&ctx) {
        scene.release_script_bitmap(id);
    }
}

extern "C" fn bitmap_destroy(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    bitmap_release_image(engine, instance);
    // SAFETY: instance came from `bitmap_create`'s `Box::into_raw` and is
    // freed exactly once by the VM.
    drop(unsafe { Box::from_raw(instance as *mut BitmapInst) });
}

/// `Bitmap(name[, colorkey])` / `Bitmap(width, height[, bpp])` — the reference
/// `tTJSNI_Bitmap::Construct`: a string first argument loads a storage image,
/// two or more numeric arguments size a new image, no argument makes the
/// reference's 32x32 transparent white initial bitmap, and a lone non-string
/// argument leaves the bitmap null until `load` fills it.
extern "C" fn bitmap_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap: compat context is not bound");
    };
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `BitmapInst` payload the VM allocated for this script object; `bitmap_ctor` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut BitmapInst) };
    if inst.id.is_some() {
        return report_error(out_error, "Bitmap: this bitmap is already constructed");
    }
    let id = if a.first().is_some_and(|value| value.ty == tjs::VAL_STRING) {
        let name = value_as_string(&a[0]);
        if name.is_empty() {
            return report_error(out_error, "Bitmap: the storage name is empty");
        }
        let colorkey = a
            .get(1)
            .filter(|value| value.ty != VAL_VOID)
            .map_or(COLOR_KEY_NONE, |value| value_as_i64(value) as u32);
        match load_into_bitmap(&ctx, None, &name, colorkey, true) {
            Ok((id, _width, _height)) => id,
            Err(error) => return report_error(out_error, &error.message_with("Bitmap: ")),
        }
    } else if a.len() > 1 {
        let Some(width) = bitmap_dimension(&a[0]) else {
            return report_error(out_error, "Bitmap: the requested size is too large");
        };
        let Some(height) = bitmap_dimension(&a[1]) else {
            return report_error(out_error, "Bitmap: the requested size is too large");
        };
        // A zero dimension is a one-pixel image: the image is never empty.
        let width = width.max(1);
        let height = height.max(1);
        let Some(pixels) = (width as usize)
            .checked_mul(height as usize)
            .and_then(|count| count.checked_mul(4))
        else {
            return report_error(out_error, "Bitmap: the requested size is too large");
        };
        let mut rgba = Vec::new();
        if rgba.try_reserve_exact(pixels).is_err() {
            return report_error(out_error, "Bitmap: the requested size is too large");
        }
        rgba.resize(pixels, 0);
        let scene = match scene_mut(&ctx) {
            Ok(scene) => scene,
            Err(error) => return report_error(out_error, &error),
        };
        let id = scene.add_bitmap(width, height, rgba);
        if let Some(bitmap) = scene.bitmap_mut(id) {
            bitmap.script_owned = true;
        }
        id
    } else if argc == 0 {
        // The reference's initial bitmap: a 32x32 transparent white rectangle
        // (`TVPGetInitialBitmap`, LayerIntf.cpp:104-110).
        const SIZE: u32 = 32;
        let scene = match scene_mut(&ctx) {
            Ok(scene) => scene,
            Err(error) => return report_error(out_error, &error),
        };
        let id = scene.add_bitmap(
            SIZE,
            SIZE,
            [255u8, 255, 255, 0].repeat((SIZE * SIZE) as usize),
        );
        if let Some(bitmap) = scene.bitmap_mut(id) {
            bitmap.script_owned = true;
        }
        id
    } else {
        // A single non-string argument leaves `Bitmap` null: constructed, no image.
        set_int(out, 0);
        return 0;
    };
    inst.id = Some(id);
    set_int(out, i64::from(id));
    0
}

/// `Bitmap.getPixel(x, y)` → the pixel's packed colour word: the red in the
/// low byte, the green in the second, the blue in the third.
extern "C" fn bitmap_get_pixel(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Bitmap.getPixel requires 2 arguments");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    // SAFETY: `instance` is the live `BitmapInst` payload the VM allocated for this script object; no `&mut` to it is live in `bitmap_get_pixel`.
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Bitmap.getPixel: this bitmap has no image");
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap.getPixel: compat context is not bound");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    // The image may still be decoding: the pixel is the decoded one.
    scene.resolve_bitmap(id);
    set_int(out, read_bitmap_pixel(scene, id, x, y) & 0x00ff_ffff);
    0
}

/// `Bitmap.setPixel(x, y, color)` → the red, green and blue planes of the
/// named colour; the mask plane is held.
extern "C" fn bitmap_set_pixel(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 3 {
        return report_error(out_error, "Bitmap.setPixel requires 3 arguments");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let color = actual_color_bytes(value_as_i64(&a[2]));
    // SAFETY: `instance` is the live `BitmapInst` payload the VM allocated for this script object; no `&mut` to it is live in `bitmap_set_pixel`.
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Bitmap.setPixel: this bitmap has no image");
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap.setPixel: compat context is not bound");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    // The image may still be decoding: the pixel is the decoded one.
    scene.resolve_bitmap(id);
    write_bitmap_pixel(scene, id, x, y, color);
    set_void(out);
    0
}

/// `Bitmap.getMaskPixel(x, y)` → alpha.
extern "C" fn bitmap_get_mask_pixel(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Bitmap.getMaskPixel requires 2 arguments");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    // SAFETY: `instance` is the live `BitmapInst` payload the VM allocated for this script object; no `&mut` to it is live in `bitmap_get_mask_pixel`.
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Bitmap.getMaskPixel: this bitmap has no image");
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Bitmap.getMaskPixel: compat context is not bound",
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    // The image may still be decoding: the pixel is the decoded one.
    scene.resolve_bitmap(id);
    set_int(out, (read_bitmap_pixel(scene, id, x, y) >> 24) & 0xff);
    0
}

/// `Bitmap.setMaskPixel(x, y, alpha)`.
extern "C" fn bitmap_set_mask_pixel(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 3 {
        return report_error(out_error, "Bitmap.setMaskPixel requires 3 arguments");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let alpha = value_as_i64(&a[2]).clamp(0, 255) as u8;
    // SAFETY: `instance` is the live `BitmapInst` payload the VM allocated for this script object; no `&mut` to it is live in `bitmap_set_mask_pixel`.
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Bitmap.setMaskPixel: this bitmap has no image");
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Bitmap.setMaskPixel: compat context is not bound",
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    // The image may still be decoding: the pixel is the decoded one.
    scene.resolve_bitmap(id);
    if let Some(bitmap) = scene.bitmap_mut(id)
        && let Some(range) = pixel_range(bitmap, x, y)
    {
        bitmap.rgba[range.start + 3] = alpha;
        // The write touched one pixel, so the upload needs that pixel and
        // not the whole image.
        bitmap.mark_dirty_rect(PixelRegion::new(x as u32, y as u32, 1, 1));
    }
    set_void(out);
    0
}

/// The byte length of a `width` x `height` straight-alpha RGBA8 image.
///
/// `None` when the count is not representable.
fn image_bytes(width: u32, height: u32) -> Option<usize> {
    usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)
        .and_then(|pixels| pixels.checked_mul(4))
}

/// Whether a bitmap's buffer is the pixels its declared size names.
///
/// An empty buffer is never an addressable image: `Vec::new().as_ptr()` is a
/// dangling one-byte pointer, not the null the script members report for an
/// image that is not there.
fn holds_addressable_pixels(bitmap: &BitmapState) -> bool {
    image_bytes(bitmap.width, bitmap.height) == Some(bitmap.rgba.len()) && !bitmap.rgba.is_empty()
}

/// A `Bitmap` constructor dimension: a non-negative integer that fits in
/// `u32`.
///
/// A negative dimension names no image: it is refused here rather than
/// wrapping into a size that asks the allocator for gigabytes of pixels.
fn bitmap_dimension(value: &Value) -> Option<u32> {
    let requested = value_as_i64(value);
    if requested < 0 {
        return None;
    }
    u32::try_from(requested).ok()
}

/// The byte range of one RGBA pixel inside a bitmap's buffer.
///
/// `None` when the point is outside the bitmap's declared size, or when the
/// bitmap declares the pixel but does not hold its four bytes: a blank image
/// whose allocation failed keeps the probed size and an empty buffer, and a
/// read or a write of it is then the same no-op as a point outside the image.
fn pixel_range(bitmap: &BitmapState, x: i32, y: i32) -> Option<Range<usize>> {
    if x < 0 || y < 0 {
        return None;
    }
    let offset = bitmap.pixel_offset(x as u32, y as u32)?;
    let end = offset.checked_add(4)?;
    if end > bitmap.rgba.len() {
        log::error!(
            "Bitmap: the image {} declares {}x{} pixels but holds {} bytes",
            bitmap.id,
            bitmap.width,
            bitmap.height,
            bitmap.rgba.len()
        );
        return None;
    }
    Some(offset..end)
}

/// Read one bitmap pixel as the packed word.
fn read_bitmap_pixel(scene: &Scene, id: u32, x: i32, y: i32) -> i64 {
    let Some(bitmap) = scene.bitmap(id) else {
        return 0;
    };
    match pixel_range(bitmap, x, y) {
        Some(range) => rgba_to_pixel_word([
            bitmap.rgba[range.start],
            bitmap.rgba[range.start + 1],
            bitmap.rgba[range.start + 2],
            bitmap.rgba[range.start + 3],
        ]),
        None => 0,
    }
}

/// Write one bitmap pixel's red, green and blue bytes. The mask byte is
/// left as it is.
fn write_bitmap_pixel(scene: &mut Scene, id: u32, x: i32, y: i32, rgb: [u8; 3]) {
    if x < 0 || y < 0 {
        return;
    }
    let Some(bitmap) = scene.bitmap_mut(id) else {
        return;
    };
    let Some(range) = pixel_range(bitmap, x, y) else {
        return;
    };
    bitmap.rgba[range.start..range.start + 3].copy_from_slice(&rgb);
    // The write touched one pixel, so the upload needs that pixel and not
    // the whole image.
    bitmap.mark_dirty_rect(PixelRegion::new(x as u32, y as u32, 1, 1));
}

/// `Bitmap.setSize(width, height)`.
extern "C" fn bitmap_set_size(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Bitmap.setSize requires 2 arguments");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let width = value_as_i64(&a[0]).max(0) as u32;
    let height = value_as_i64(&a[1]).max(0) as u32;
    if width == 0 || height == 0 {
        return report_error(out_error, "Bitmap.setSize: cannot create empty layer image");
    }
    // SAFETY: `instance` is the live `BitmapInst` payload the VM allocated for this script object; no `&mut` to it is live in `bitmap_set_size`.
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Bitmap.setSize: this bitmap has no image");
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap.setSize: compat context is not bound");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    scene.resize_bitmap(id, width, height, [0, 0, 0, 0]);
    set_void(out);
    0
}

/// `Bitmap.copyFrom(bitmap)` — copy the source pixels into this bitmap.
extern "C" fn bitmap_copy_from(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Bitmap.copyFrom requires a Bitmap argument");
    }
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap.copyFrom: compat context is not bound");
    };
    let source_id = match resolve_object_id(
        resolved.engine(),
        // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
        &unsafe { args(argv, argc) }[0],
        &["Bitmap"],
    ) {
        Ok(id) if id >= 0 => id as u32,
        _ => return report_error(out_error, "Bitmap.copyFrom: not a Bitmap object"),
    };
    // SAFETY: `instance` is the live `BitmapInst` payload the VM allocated for this script object; no `&mut` to it is live in `bitmap_copy_from`.
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Bitmap.copyFrom: this bitmap has no image");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    // The source may still be decoding: the copy reads its pixels.
    scene.resolve_bitmap(source_id);
    let Some((width, height, rgba)) = scene
        .bitmap(source_id)
        .map(|b| (b.width, b.height, b.rgba.clone()))
    else {
        return report_error(
            out_error,
            "Bitmap.copyFrom: the source bitmap does not exist",
        );
    };
    // A source that declares its size and holds no pixels has nothing to
    // copy: copying its empty buffer would give this bitmap the same
    // inconsistent state.
    if image_bytes(width, height) != Some(rgba.len()) {
        return report_error(
            out_error,
            "Bitmap.copyFrom: the source image does not hold its pixels",
        );
    }
    scene.replace_bitmap_image(id, width, height, rgba);
    set_void(out);
    0
}

/// Fold the `meta.bpp` save option into the encoder's type hint. The
/// reference `TVPSaveAsBMP` reads the meta dictionary's `bpp` member
/// (`GraphicsLoaderIntf.cpp`); the L1 encoder only takes options through its
/// hint, so only the BMP bit depth is translated. Every other option is
/// accepted and ignored here.
fn apply_save_meta(host: &Tjs2Engine, value: &Value, type_name: &str) -> Result<String, String> {
    let lower = type_name.to_ascii_lowercase();
    let is_bmp = lower.starts_with("bmp")
        || lower.ends_with(".bmp")
        || lower.ends_with(".dib")
        || lower == "dib"
        || lower == "bmp";
    if !is_bmp {
        return Ok(type_name.to_string());
    }
    let object = host.retain_object_arg(value)?;
    // The reference checks `TJS_SUCCEEDED(PropGet(...))`, so a missing `bpp`
    // is simply "use the handler default".
    let bpp = match host.member(object.raw_id(), "bpp") {
        Ok(TjsValue::Integer(bpp)) => bpp,
        Ok(TjsValue::Real(bpp)) => bpp as i64,
        Ok(_) | Err(_) => return Ok(type_name.to_string()),
    };
    // `TVPSaveTextureAsBMP` switches on `meta.bpp` as an **index**: 0 selects
    // 4 bytes per pixel, 1 selects 3 and 2 selects 1, and an index outside
    // that range leaves the mode's own default in place
    // (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:740-770`).
    Ok(match bpp {
        0 => "bmp".to_string(),
        1 => "bmp24".to_string(),
        2 => "bmp8".to_string(),
        _ => type_name.to_string(),
    })
}

/// `Bitmap.save(name[, type[, meta]])` — encode this bitmap and write it
/// through the installed host saver. The L2 [`Storage`] API is read-only, so
/// the host owns the write; without an installed encoder the member raises a
/// typed error.
extern "C" fn bitmap_save(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Bitmap.save requires a storage name");
    }
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap.save: compat context is not bound");
    };
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let name = value_as_string(&a[0]);
    if name.is_empty() {
        return report_error(out_error, "Bitmap.save: the storage name is empty");
    }
    let type_name = match a.get(1) {
        // Reference `Bitmap.save`: a present, non-void argument is used
        // verbatim, including the empty string (`BitmapIntf.cpp:330-333`);
        // only an absent/void argument falls back to `"bmp"`.
        Some(value) if value.ty != VAL_VOID => value_as_string(value),
        _ => "bmp".to_string(),
    };
    // The meta dictionary drives the save options (`BitmapIntf.cpp:334-336`,
    // `TVPSaveImage`). The L1 encoder only accepts options through its type
    // hint, so the BMP `bpp` option is folded into it; the argument is read
    // and validated even when no option applies.
    let type_name = match a.get(2) {
        Some(value) if value.ty == tjs::VAL_OBJECT => {
            match apply_save_meta(resolved.engine(), value, &type_name) {
                Ok(type_name) => type_name,
                Err(error) => return report_error(out_error, &format!("Bitmap.save: {error}")),
            }
        }
        Some(value) if value.ty != VAL_VOID && value.ty != tjs::VAL_NULL => {
            return report_error(out_error, "Bitmap.save: the meta argument is not an object");
        }
        _ => type_name,
    };
    let (id, loading) = {
        // SAFETY: `instance` is the live `BitmapInst` payload the VM allocated for this script object; no `&mut` to it is live in `bitmap_save`.
        let inst = unsafe { &*(instance as *const BitmapInst) };
        (inst.id, inst.loading)
    };
    if loading {
        return report_error(out_error, "Bitmap.save: currently async load bitmap");
    }
    let Some(id) = id else {
        return report_error(out_error, "Bitmap.save: this bitmap has no image");
    };
    // SAFETY: `ctx` is only created for a bound, live engine, so its service pointer is valid for `bitmap_save`.
    let services = unsafe { ctx.services() };
    let resolver = services.state::<ResourceResolver>();
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &format!("Bitmap.save: {error}")),
    };
    // The image may still be decoding: the saved pixels are the decoded ones.
    scene.resolve_bitmap(id);
    let Some(bitmap) = scene.bitmap(id) else {
        return report_error(out_error, "Bitmap.save: this bitmap no longer exists");
    };
    let (width, height, rgba) = (bitmap.width, bitmap.height, bitmap.rgba.clone());
    // A bitmap that declares its size and holds no pixels has nothing to
    // encode: handing the encoder an empty buffer would write a truncated
    // image.
    if image_bytes(width, height) != Some(rgba.len()) {
        return report_error(
            out_error,
            "Bitmap.save: this image does not hold its pixels",
        );
    }
    match resolver.save_image(&name, &type_name, width, height, &rgba) {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &format!("Bitmap.save: {error}")),
    }
}

/// `Bitmap.load(name[, colorkey])` — synchronously load `name` from storage
/// into this bitmap and return the metainfo dictionary.
extern "C" fn bitmap_load(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Bitmap.load requires a storage name");
    }
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap.load: compat context is not bound");
    };
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let name = value_as_string(&a[0]);
    if name.is_empty() {
        return report_error(out_error, "Bitmap.load: the storage name is empty");
    }
    let colorkey = a
        .get(1)
        .filter(|value| value.ty != VAL_VOID)
        .map_or(COLOR_KEY_NONE, |value| value_as_i64(value) as u32);
    let (id, loading) = {
        // SAFETY: `instance` is the live `BitmapInst` payload the VM allocated for this script object; no `&mut` to it is live in `bitmap_load`.
        let inst = unsafe { &*(instance as *const BitmapInst) };
        (inst.id, inst.loading)
    };
    if loading {
        return report_error(out_error, "Bitmap.load: currently async load bitmap");
    }
    let had_image = id.is_some();
    let (id, width, height) = match load_into_bitmap(&ctx, id, &name, colorkey, true) {
        Ok(size) => size,
        Err(error) => return report_error(out_error, &error.message_with("Bitmap.load: ")),
    };
    if !had_image {
        // SAFETY: the payload is owned by the VM for the duration of the call.
        unsafe { (*(instance as *mut BitmapInst)).id = Some(id) };
    }
    let host = resolved.engine();
    match metainfo_object(host, width, height, 32) {
        Ok(object) => {
            set_retained_out(host, out, object.raw_id());
            std::mem::forget(object);
        }
        Err(error) => {
            log::warn!("Bitmap.load: {error}");
            set_void(out);
        }
    }
    0
}

/// `Bitmap.loadAsync(name)` — L2 exposes no async worker, so the load runs
/// synchronously and `onLoaded(meta, async, error, message)` is dispatched
/// immediately (the reference's synchronous cache-hit path).
extern "C" fn bitmap_load_async(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Bitmap.loadAsync requires a storage name");
    }
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap.loadAsync: compat context is not bound");
    };
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let name = value_as_string(&a[0]);
    if name.is_empty() {
        return report_error(out_error, "Bitmap.loadAsync: the storage name is empty");
    }
    let (id, had_image) = {
        // SAFETY: `instance` is the live `BitmapInst` payload the VM allocated for this script object; `bitmap_load_async` is its only live accessor.
        let inst = unsafe { &mut *(instance as *mut BitmapInst) };
        if inst.loading {
            return report_error(out_error, "Bitmap.loadAsync: currently async load bitmap");
        }
        let id = inst.id;
        inst.loading = true;
        (id, id.is_some())
    };
    // `loadAsync` reports the load's result to the script, so its decode
    // runs here rather than on the pool.
    let result = load_into_bitmap(&ctx, id, &name, COLOR_KEY_NONE, false);
    // SAFETY: the payload outlives the call and no other reference is live.
    unsafe { (*(instance as *mut BitmapInst)).loading = false };
    if let Ok((new_id, _width, _height)) = &result
        && !had_image
    {
        // SAFETY: the payload is owned by the VM for the duration of the call.
        unsafe { (*(instance as *mut BitmapInst)).id = Some(*new_id) };
    }
    let host = resolved.engine();
    let (meta, error_flag, message) = match result {
        Ok((_id, width, height)) => match metainfo_object(host, width, height, 32) {
            Ok(object) => (Some(object), 0, String::new()),
            Err(error) => (None, 1, error),
        },
        Err(error) => (None, 1, error.message()),
    };
    if let Ok(owner) = host.retain_object_detached(objthis) {
        let meta_arg = match meta {
            Some(object) => {
                let raw = object.raw_id();
                std::mem::forget(object);
                TjsValue::Retained(raw.as_ptr() as usize as u64)
            }
            None => TjsValue::Void,
        };
        let call_args = [
            meta_arg,
            TjsValue::Integer(0),
            TjsValue::Integer(error_flag),
            TjsValue::String(message),
        ];
        if let Err(error) = host.call_member(owner.raw_id(), "onLoaded", &call_args) {
            log::warn!("Bitmap.loadAsync: onLoaded dispatch failed: {error}");
        }
    }
    set_void(out);
    0
}

/// `Bitmap.loadHeader(name)` — probe the image header through the installed
/// host prober and return a `{ width, height, bpp }` dictionary. Registered
/// as a static class member (the reference `TVP_END_NATIVE_STATIC_METHOD_DECL`).
extern "C" fn bitmap_load_header(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Bitmap.loadHeader requires a storage name");
    }
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap.loadHeader: compat context is not bound");
    };
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let name = value_as_string(&unsafe { args(argv, argc) }[0]);
    if name.is_empty() {
        return report_error(out_error, "Bitmap.loadHeader: the storage name is empty");
    }
    // SAFETY: `ctx` is only created for a bound, live engine, so its service pointer is valid for `bitmap_load_header`.
    let services = unsafe { ctx.services() };
    let Some(storage) = services.storage() else {
        return report_error(out_error, "Bitmap.loadHeader: no storage attached");
    };
    // SAFETY: `storage` is the attached L2 storage the host keeps alive for the VM lifetime.
    let (resolved_name, bytes) = match read_image_from_storage(unsafe { &*storage }, &name) {
        Ok(found) => found,
        Err(error) => {
            return report_error(out_error, &error.message_with("Bitmap.loadHeader: "));
        }
    };
    let info = match probe_image(services, &resolved_name, &bytes) {
        Ok(info) => info,
        Err(error) => {
            // The reference header router leaves the result void when no
            // handler accepts the data; it does not raise
            // (`BitmapIntf.cpp:378-401`). Log the reason and yield void.
            log::warn!("Bitmap.loadHeader: no header handler accepted {name:?}: {error}");
            set_void(out);
            return 0;
        }
    };
    let bpp = if info.has_alpha { 32 } else { 24 };
    let host = resolved.engine();
    match metainfo_object(host, info.width, info.height, bpp) {
        Ok(object) => {
            set_retained_out(host, out, object.raw_id());
            std::mem::forget(object);
        }
        Err(error) => return report_error(out_error, &format!("Bitmap.loadHeader: {error}")),
    }
    0
}

/// `Bitmap.getSaveOption(type)` — the reference `TVPGetSaveOption` dictionary
/// for an accepted save type, or `void` when no handler accepts it. Registered
/// as a static class member.
extern "C" fn bitmap_get_save_option(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Bitmap.getSaveOption requires a type");
    }
    let Ok((resolved, _ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Bitmap.getSaveOption: compat context is not bound",
        );
    };
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let type_name = value_as_string(&unsafe { args(argv, argc) }[0]);
    let host = resolved.engine();
    match save_option_expression(&type_name) {
        Some(expression) => match host.eval_retained(expression, "Bitmap.getSaveOption") {
            Ok(RetainedValue::Object(object)) => {
                set_retained_out(host, out, object.raw_id());
                std::mem::forget(object);
            }
            Ok(RetainedValue::Value(_)) => set_void(out),
            Err(error) => {
                return report_error(
                    out_error,
                    &format!("Bitmap.getSaveOption: could not build the option object: {error}"),
                );
            }
        },
        None => set_void(out),
    }
    0
}

/// `Bitmap.independ()` — ensure the bitmap owns its pixels. Each `Bitmap`
/// object already has a private scene bitmap, so this is a no-op.
extern "C" fn bitmap_independ(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void(out);
    0
}

// --- Bitmap properties -----------------------------------------------------

/// `bitmap.width`.
extern "C" fn bitmap_width_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    bitmap_dimension_get(engine, instance, out, out_error, true)
}

/// `bitmap.height`.
extern "C" fn bitmap_height_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    bitmap_dimension_get(engine, instance, out, out_error, false)
}

/// `bitmap.width = n` — resize (preserving the overlapping region).
extern "C" fn bitmap_width_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    bitmap_dimension_set(engine, instance, value, out_error, true)
}

/// `bitmap.height = n` — resize (preserving the overlapping region).
extern "C" fn bitmap_height_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    bitmap_dimension_set(engine, instance, value, out_error, false)
}

fn bitmap_dimension_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    width: bool,
) -> c_int {
    // SAFETY: `instance` is the live `BitmapInst` payload the VM allocated for this script object; no `&mut` to it is live in `bitmap_dimension_get`.
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Bitmap: this bitmap has no image");
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap: compat context is not bound");
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let Some(bitmap) = scene.bitmap(id) else {
        return report_error(out_error, "Bitmap: bitmap no longer exists");
    };
    set_int(
        out,
        i64::from(if width { bitmap.width } else { bitmap.height }),
    );
    0
}

fn bitmap_dimension_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    width: bool,
) -> c_int {
    // SAFETY: `instance` is the live `BitmapInst` payload the VM allocated for this script object; no `&mut` to it is live in `bitmap_dimension_set`.
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Bitmap: this bitmap has no image");
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap: compat context is not bound");
    };
    // SAFETY: `value` is the valid `Value` the caller holds for this call.
    let requested = value_as_i64(unsafe { &*value }).max(0) as u32;
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let Some((current_width, current_height)) = scene.bitmap(id).map(|b| (b.width, b.height))
    else {
        return report_error(out_error, "Bitmap: bitmap no longer exists");
    };
    let (new_width, new_height) = if width {
        (requested, current_height)
    } else {
        (current_width, requested)
    };
    scene.resize_bitmap(id, new_width, new_height, [0, 0, 0, 0]);
    0
}

/// `bitmap.buffer` — the pixel buffer address (0 when absent).
extern "C" fn bitmap_buffer_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `BitmapInst` payload the VM allocated for this script object; no `&mut` to it is live in `bitmap_buffer_get`.
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap.buffer: compat context is not bound");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    // The image may still be decoding: the address is the decoded buffer's.
    scene.resolve_bitmap(id);
    let address = scene.bitmap(id).map_or(0, |bitmap| {
        if holds_addressable_pixels(bitmap) {
            bitmap.rgba.as_ptr() as i64
        } else {
            0
        }
    });
    set_int(out, address);
    0
}

/// `bitmap.bufferForWrite` — the pixel buffer address, marking the bitmap
/// dirty.
extern "C" fn bitmap_buffer_for_write_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `BitmapInst` payload the VM allocated for this script object; no `&mut` to it is live in `bitmap_buffer_for_write_get`.
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Bitmap.bufferForWrite: compat context is not bound",
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    // The image may still be decoding: the address is the decoded buffer's.
    scene.resolve_bitmap(id);
    let address = match scene.bitmap_mut(id) {
        Some(bitmap) if holds_addressable_pixels(bitmap) => {
            bitmap.mark_dirty();
            bitmap.rgba.as_mut_ptr() as i64
        }
        _ => 0,
    };
    set_int(out, address);
    0
}

/// `bitmap.bufferPitch` — the row pitch in bytes.
extern "C" fn bitmap_buffer_pitch_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `BitmapInst` payload the VM allocated for this script object; no `&mut` to it is live in `bitmap_buffer_pitch_get`.
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap.bufferPitch: compat context is not bound");
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_int(
        out,
        scene
            .bitmap(id)
            .map_or(0, |b| i64::from(b.width.saturating_mul(4))),
    );
    0
}

/// `bitmap.loading`.
extern "C" fn bitmap_loading_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `BitmapInst` payload the VM allocated for this script object; no `&mut` to it is live in `bitmap_loading_get`.
    let inst = unsafe { &*(instance as *const BitmapInst) };
    set_bool(out, inst.loading);
    0
}

/// `Bitmap.onLoaded()` — the (unused) async load completion hook.
extern "C" fn bitmap_on_loaded(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void(out);
    0
}

pub(super) fn register_bitmap_class(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "Bitmap",
        create: bitmap_create,
        destroy: bitmap_destroy,
        invalidate: Some(bitmap_release_image),
        methods: vec![
            NativeInstanceMethodDef {
                name: "Bitmap",
                f: bitmap_ctor,
            },
            NativeInstanceMethodDef {
                name: "getPixel",
                f: bitmap_get_pixel,
            },
            NativeInstanceMethodDef {
                name: "setPixel",
                f: bitmap_set_pixel,
            },
            NativeInstanceMethodDef {
                name: "getMaskPixel",
                f: bitmap_get_mask_pixel,
            },
            NativeInstanceMethodDef {
                name: "setMaskPixel",
                f: bitmap_set_mask_pixel,
            },
            NativeInstanceMethodDef {
                name: "independ",
                f: bitmap_independ,
            },
            NativeInstanceMethodDef {
                name: "setSize",
                f: bitmap_set_size,
            },
            NativeInstanceMethodDef {
                name: "copyFrom",
                f: bitmap_copy_from,
            },
            NativeInstanceMethodDef {
                name: "save",
                f: bitmap_save,
            },
            NativeInstanceMethodDef {
                name: "load",
                f: bitmap_load,
            },
            NativeInstanceMethodDef {
                name: "loadAsync",
                f: bitmap_load_async,
            },
            NativeInstanceMethodDef {
                name: "onLoaded",
                f: bitmap_on_loaded,
            },
        ],
        properties: vec![
            NativeInstancePropertyDef {
                name: "width",
                get: Some(bitmap_width_get),
                set: Some(bitmap_width_set),
            },
            NativeInstancePropertyDef {
                name: "height",
                get: Some(bitmap_height_get),
                set: Some(bitmap_height_set),
            },
            NativeInstancePropertyDef {
                name: "buffer",
                get: Some(bitmap_buffer_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "bufferForWrite",
                get: Some(bitmap_buffer_for_write_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "bufferPitch",
                get: Some(bitmap_buffer_pitch_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "loading",
                get: Some(bitmap_loading_get),
                set: None,
            },
        ],
    })?;
    ctx.register_static_members(&NativeStaticMembers {
        class_name: "Bitmap",
        methods: vec![
            NativeMethodDef {
                name: "loadHeader",
                f: bitmap_load_header,
            },
            NativeMethodDef {
                name: "getSaveOption",
                f: bitmap_get_save_option,
            },
        ],
        properties: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A load below the threshold is never reported, a load above it is
    /// reported once, and the next one is held back until the interval has
    /// passed: the report stays bounded.
    #[test]
    fn the_slow_load_report_is_bounded() {
        assert!(!is_slow(Duration::from_millis(10)), "a fast load");
        assert!(is_slow(Duration::from_millis(80)), "a slow load");

        let mut reporter = SlowLoadReporter::default();
        assert!(reporter.is_due(), "the first slow load is reported");
        assert!(
            !reporter.is_due(),
            "a second slow load inside the interval is held back"
        );
    }
}
