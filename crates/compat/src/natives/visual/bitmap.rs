//! The visual `Bitmap` native class.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};
use std::rc::Rc;

use api::media::{DecodedImage, ImageInfo};
use engine::{Scene, Storage, StorageReadError};
use tjs::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    NativeMethodDef, NativeStaticMembers, RetainedValue, Tjs2Engine, TjsValue, VAL_VOID, Value,
};

use super::common::{
    VisualState, argb_to_rgba, bound, resolve_object_id, rgba_to_argb, scene_mut, scene_ref,
    set_retained_out,
};
use crate::native_util::{args, report_error, set_bool, set_int, set_void, value_as_string};
use crate::natives::system::value_as_i64;
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

// ---------------------------------------------------------------------------
// Host-supplied image codec (the L1 `media` seam)
// ---------------------------------------------------------------------------

/// Host-supplied image decoder: the L1 `media::decode_image` call behind
/// `Bitmap.load`. `hint` is the resolved storage name.
type DecodeFn = dyn Fn(&[u8], Option<&str>) -> Result<DecodedImage, String>;
/// Host-supplied header prober: the L1 `media::probe_image` call behind
/// `Bitmap.loadHeader`.
type ProbeFn = dyn Fn(&[u8], Option<&str>) -> Result<ImageInfo, String>;
/// Host-supplied image saver: encodes the bitmap and writes it to the named
/// storage. The L2 [`Storage`] API has no write path, so the host owns both
/// the encode and the write.
type SaveFn = dyn Fn(&str, &str, u32, u32, &[u8]) -> Result<(), String>;

/// The per-engine image-codec seam.
///
/// L3 (`compat`) never links the L1 `media` codec; the L4 host installs the
/// decode/probe/save closures through [`crate::CompatHost::set_image_codec`],
/// exactly like the `WaveSoundBuffer` audio decoder seam. A member whose
/// closure is missing raises a typed error instead of fabricating a
/// result.
#[derive(Default)]
struct BitmapCodec {
    decode: Option<Rc<DecodeFn>>,
    probe: Option<Rc<ProbeFn>>,
    save: Option<Rc<SaveFn>>,
}

impl crate::CompatHost {
    /// Install the host image codec the `Bitmap` natives compose.
    ///
    /// `decode` backs `Bitmap.load`/`Bitmap.loadAsync`, `probe` backs
    /// `Bitmap.loadHeader` and `save` backs `Bitmap.save`. Only the L4 host
    /// links the L1 `media` crate; the integration tests install a synthetic
    /// codec here. A member whose closure is absent raises a typed error
    /// instead of fabricating a result.
    pub fn set_image_codec(
        &self,
        decode: impl Fn(&[u8], Option<&str>) -> Result<DecodedImage, String> + 'static,
        probe: impl Fn(&[u8], Option<&str>) -> Result<ImageInfo, String> + 'static,
        save: impl Fn(&str, &str, u32, u32, &[u8]) -> Result<(), String> + 'static,
    ) {
        let mut codec = self.services().state::<BitmapCodec>();
        codec.decode = Some(Rc::new(decode));
        codec.probe = Some(Rc::new(probe));
        codec.save = Some(Rc::new(save));
    }
}

/// Common image extensions probed when the requested storage name does not
/// resolve, mirroring the reference `TVPFindGraphicLoadHandler` order.
const IMAGE_EXTENSION_PROBE: [&str; 21] = [
    ".webp", ".bmp", ".dib", ".jpeg", ".jpg", ".jif", ".png", ".tlg", ".tlg5", ".tlg6", ".gif",
    ".tif", ".tiff", ".tga", ".dds", ".pnm", ".ico", ".qoi", ".hdr", ".exr", ".ff",
];

/// Read `name` through the attached L2 [`Storage`], probing the common image
/// extensions when the name itself does not resolve.
pub(crate) fn read_image_from_storage(
    storage: &Storage,
    name: &str,
) -> Result<(String, Vec<u8>), String> {
    let mut candidates: Vec<String> = Vec::with_capacity(IMAGE_EXTENSION_PROBE.len() + 1);
    candidates.push(name.to_string());
    candidates.extend(
        IMAGE_EXTENSION_PROBE
            .iter()
            .map(|extension| format!("{name}{extension}")),
    );
    for candidate in candidates {
        match storage.read(&candidate) {
            Ok(bytes) => return Ok((candidate, bytes)),
            Err(StorageReadError::NotFound(_)) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Err(format!("storage not found: {name}"))
}

/// Decode image bytes through the installed host decoder.
pub(crate) fn decode_image(
    services: &CompatServices,
    name: &str,
    bytes: &[u8],
) -> Result<DecodedImage, String> {
    let decoder = services.state::<BitmapCodec>().decode.clone();
    let Some(decoder) = decoder else {
        return Err("no image decoder is installed".to_string());
    };
    decoder(bytes, Some(name))
}

/// Probe image bytes through the installed host prober.
fn probe_image(services: &CompatServices, name: &str, bytes: &[u8]) -> Result<ImageInfo, String> {
    let prober = services.state::<BitmapCodec>().probe.clone();
    let Some(prober) = prober else {
        return Err("no image header prober is installed".to_string());
    };
    prober(bytes, Some(name))
}

/// Overwrite an existing scene bitmap's pixels in place, reusing its id (the
/// reference `Bitmap.load` replaces the current image).
pub(crate) fn replace_bitmap_pixels(
    scene: &mut Scene,
    id: u32,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
) {
    scene.resize_bitmap(id, width, height, [0, 0, 0, 0]);
    if let Some(bitmap) = scene.bitmap_mut(id) {
        bitmap.width = width;
        bitmap.height = height;
        bitmap.rgba = rgba;
        bitmap.script_owned = true;
        bitmap.mark_dirty();
    }
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
    use std::collections::HashMap;
    let mut counts: HashMap<u32, u32> = HashMap::new();
    let mut best = 0u32;
    let mut best_count = 0u32;
    for pixel in rgba.chunks_exact(4).take(width as usize) {
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
    for pixel in rgba.chunks_exact_mut(4) {
        let rgb = (u32::from(pixel[0]) << 16) | (u32::from(pixel[1]) << 8) | u32::from(pixel[2]);
        pixel[3] = if rgb == key { 0 } else { 255 };
    }
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

/// Perform a synchronous storage load into bitmap `id`, returning the image
/// size. Shared by `load` and `loadAsync`.
fn load_into_bitmap(
    ctx: &CompatCtx,
    id: u32,
    name: &str,
    colorkey: u32,
) -> Result<(u32, u32), String> {
    let services = unsafe { ctx.services() };
    let storage = services
        .storage()
        .ok_or_else(|| "no storage attached".to_string())?;
    // SAFETY: `attach_storage` guarantees the pointer stays valid.
    let (resolved, bytes) = read_image_from_storage(unsafe { &*storage }, name)?;
    let image = decode_image(services, &resolved, &bytes)?;
    if !image.is_consistent() {
        return Err("the decoded image is malformed".to_string());
    }
    let (width, height) = (image.width, image.height);
    let mut rgba = image.rgba;
    apply_color_key(&mut rgba, width, colorkey);
    let scene = scene_mut(ctx)?;
    replace_bitmap_pixels(scene, id, width, height, rgba);
    Ok((width, height))
}

/// See `register_window`.
fn register_bitmap(ctx: &CompatCtx, host: &Tjs2Engine, id: u32, objthis: *mut c_void) {
    if objthis.is_null() {
        return;
    }
    match host.retain_object_detached(objthis) {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        Ok(object) => {
            unsafe { ctx.services() }
                .state::<VisualState>()
                .objects
                .bitmaps
                .insert(id, object);
        }
        Err(error) => log::warn!("Bitmap: could not retain the script object: {error}"),
    }
}

// ---------------------------------------------------------------------------
// Bitmap
// ---------------------------------------------------------------------------

extern "C" fn bitmap_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(BitmapInst::default())) as *mut c_void
}

extern "C" fn bitmap_destroy(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    let inst = unsafe { &*(instance as *const BitmapInst) };
    if let (Some(id), Ok((_resolved, ctx))) = (inst.id, bound(engine)) {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        unsafe { ctx.services() }
            .state::<VisualState>()
            .objects
            .bitmaps
            .remove(&id);
    }
    // SAFETY: instance came from `bitmap_create`'s `Box::into_raw` and is
    // freed exactly once by the VM.
    drop(unsafe { Box::from_raw(instance as *mut BitmapInst) });
}

/// `Bitmap([width, height])` — create a bitmap and return its id.
extern "C" fn bitmap_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap: compat context is not bound");
    };
    let a = unsafe { args(argv, argc) };
    let width = a.first().map(value_as_i64).unwrap_or(1).max(1) as u32;
    let height = a.get(1).map(value_as_i64).unwrap_or(1).max(1) as u32;
    let inst = unsafe { &mut *(instance as *mut BitmapInst) };
    if inst.id.is_some() {
        return report_error(out_error, "Bitmap: this bitmap is already constructed");
    }
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let Some(pixels) = (width as usize)
        .checked_mul(height as usize)
        .and_then(|count| count.checked_mul(4))
    else {
        return report_error(out_error, "Bitmap: the requested size is too large");
    };
    let id = scene.add_bitmap(width, height, vec![0u8; pixels]);
    if let Some(bitmap) = scene.bitmap_mut(id) {
        bitmap.script_owned = true;
    }
    inst.id = Some(id);
    register_bitmap(&ctx, resolved.engine(), id, objthis);
    set_int(out, i64::from(id));
    0
}

/// `Bitmap.getPixel(x, y)` → ARGB.
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
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap.getPixel: compat context is not bound");
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_int(out, read_bitmap_pixel(scene, id, x, y));
    0
}

/// `Bitmap.setPixel(x, y, color)`.
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
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let color = argb_to_rgba(value_as_i64(&a[2]));
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap.setPixel: compat context is not bound");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    write_bitmap_pixel(scene, id, x, y, color, false);
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
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Bitmap.getMaskPixel: compat context is not bound",
        );
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
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
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let alpha = value_as_i64(&a[2]).clamp(0, 255) as u8;
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        return 0;
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
    if let Some(bitmap) = scene.bitmap_mut(id)
        && let Some(offset) = bitmap.pixel_offset(x as u32, y as u32)
    {
        bitmap.rgba[offset + 3] = alpha;
        bitmap.mark_dirty();
    }
    set_void(out);
    0
}

/// Read one bitmap pixel as ARGB.
fn read_bitmap_pixel(scene: &Scene, id: u32, x: i32, y: i32) -> i64 {
    let Some(bitmap) = scene.bitmap(id) else {
        return 0;
    };
    if x < 0 || y < 0 {
        return 0;
    }
    match bitmap.pixel_offset(x as u32, y as u32) {
        Some(offset) => rgba_to_argb([
            bitmap.rgba[offset],
            bitmap.rgba[offset + 1],
            bitmap.rgba[offset + 2],
            bitmap.rgba[offset + 3],
        ]),
        None => 0,
    }
}

/// Write one bitmap pixel. `hold_alpha` keeps the destination alpha.
fn write_bitmap_pixel(
    scene: &mut Scene,
    id: u32,
    x: i32,
    y: i32,
    color: [u8; 4],
    hold_alpha: bool,
) {
    if x < 0 || y < 0 {
        return;
    }
    let Some(bitmap) = scene.bitmap_mut(id) else {
        return;
    };
    if let Some(offset) = bitmap.pixel_offset(x as u32, y as u32) {
        if hold_alpha {
            bitmap.rgba[offset] = color[0];
            bitmap.rgba[offset + 1] = color[1];
            bitmap.rgba[offset + 2] = color[2];
        } else {
            bitmap.rgba[offset..offset + 4].copy_from_slice(&color);
        }
        bitmap.mark_dirty();
    }
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
    let a = unsafe { args(argv, argc) };
    let width = value_as_i64(&a[0]).max(0) as u32;
    let height = value_as_i64(&a[1]).max(0) as u32;
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Bitmap.setSize: this bitmap is not constructed");
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
        &unsafe { args(argv, argc) }[0],
        &["Bitmap"],
    ) {
        Ok(id) if id >= 0 => id as u32,
        _ => return report_error(out_error, "Bitmap.copyFrom: not a Bitmap object"),
    };
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Bitmap.copyFrom: this bitmap is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let Some((width, height, rgba)) = scene
        .bitmap(source_id)
        .map(|b| (b.width, b.height, b.rgba.clone()))
    else {
        return report_error(
            out_error,
            "Bitmap.copyFrom: the source bitmap does not exist",
        );
    };
    scene.resize_bitmap(id, width, height, [0, 0, 0, 0]);
    if let Some(bitmap) = scene.bitmap_mut(id) {
        bitmap.rgba = rgba;
        bitmap.width = width;
        bitmap.height = height;
        bitmap.mark_dirty();
    }
    set_void(out);
    0
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
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap.save: compat context is not bound");
    };
    let a = unsafe { args(argv, argc) };
    let name = value_as_string(&a[0]);
    if name.is_empty() {
        return report_error(out_error, "Bitmap.save: the storage name is empty");
    }
    let type_name = match a.get(1) {
        Some(value) if value.ty != VAL_VOID => {
            let text = value_as_string(value);
            if text.is_empty() {
                "bmp".to_string()
            } else {
                text
            }
        }
        _ => "bmp".to_string(),
    };
    let (id, loading) = {
        let inst = unsafe { &*(instance as *const BitmapInst) };
        (inst.id, inst.loading)
    };
    if loading {
        return report_error(out_error, "Bitmap.save: currently async load bitmap");
    }
    let Some(id) = id else {
        return report_error(out_error, "Bitmap.save: this bitmap is not constructed");
    };
    let services = unsafe { ctx.services() };
    let saver = services.state::<BitmapCodec>().save.clone();
    let Some(saver) = saver else {
        return report_error(out_error, "Bitmap.save: no image encoder is installed");
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &format!("Bitmap.save: {error}")),
    };
    let Some(bitmap) = scene.bitmap(id) else {
        return report_error(out_error, "Bitmap.save: this bitmap no longer exists");
    };
    let (width, height, rgba) = (bitmap.width, bitmap.height, bitmap.rgba.clone());
    match saver(&name, &type_name, width, height, &rgba) {
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
        let inst = unsafe { &*(instance as *const BitmapInst) };
        (inst.id, inst.loading)
    };
    if loading {
        return report_error(out_error, "Bitmap.load: currently async load bitmap");
    }
    let Some(id) = id else {
        return report_error(out_error, "Bitmap.load: this bitmap is not constructed");
    };
    let (width, height) = match load_into_bitmap(&ctx, id, &name, colorkey) {
        Ok(size) => size,
        Err(error) => return report_error(out_error, &format!("Bitmap.load: {error}")),
    };
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
    let a = unsafe { args(argv, argc) };
    let name = value_as_string(&a[0]);
    if name.is_empty() {
        return report_error(out_error, "Bitmap.loadAsync: the storage name is empty");
    }
    let id = {
        let inst = unsafe { &mut *(instance as *mut BitmapInst) };
        if inst.loading {
            return report_error(out_error, "Bitmap.loadAsync: currently async load bitmap");
        }
        let Some(id) = inst.id else {
            return report_error(
                out_error,
                "Bitmap.loadAsync: this bitmap is not constructed",
            );
        };
        inst.loading = true;
        id
    };
    let result = load_into_bitmap(&ctx, id, &name, COLOR_KEY_NONE);
    // SAFETY: the payload outlives the call and no other reference is live.
    unsafe { (*(instance as *mut BitmapInst)).loading = false };
    let host = resolved.engine();
    let (meta, error_flag, message) = match result {
        Ok((width, height)) => match metainfo_object(host, width, height, 32) {
            Ok(object) => (Some(object), 0, String::new()),
            Err(error) => (None, 1, error),
        },
        Err(error) => (None, 1, error),
    };
    if let Ok(owner) = host.retain_object_detached(objthis) {
        let meta_arg = match meta {
            Some(object) => {
                let raw = object.raw_id();
                std::mem::forget(object);
                TjsValue::Retained(raw as usize as u64)
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
    let name = value_as_string(&unsafe { args(argv, argc) }[0]);
    if name.is_empty() {
        return report_error(out_error, "Bitmap.loadHeader: the storage name is empty");
    }
    let services = unsafe { ctx.services() };
    let Some(storage) = services.storage() else {
        return report_error(out_error, "Bitmap.loadHeader: no storage attached");
    };
    let (resolved_name, bytes) = match read_image_from_storage(unsafe { &*storage }, &name) {
        Ok(found) => found,
        Err(error) => return report_error(out_error, &format!("Bitmap.loadHeader: {error}")),
    };
    let info = match probe_image(services, &resolved_name, &bytes) {
        Ok(info) => info,
        Err(error) => return report_error(out_error, &format!("Bitmap.loadHeader: {error}")),
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
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
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
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap: compat context is not bound");
    };
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
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap.buffer: compat context is not bound");
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_int(out, scene.bitmap(id).map_or(0, |b| b.rgba.as_ptr() as i64));
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
    let address = match scene.bitmap_mut(id) {
        Some(bitmap) => {
            bitmap.mark_dirty();
            bitmap.rgba.as_mut_ptr() as i64
        }
        None => 0,
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
        invalidate: None,
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
