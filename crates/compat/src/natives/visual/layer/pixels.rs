//! `Layer` image / pixel operations (group 1).
//!
//! Every method composes the L2 `engine_api::layer_ops` / `engine_api::raster`
//! primitives directly (the same ones `natives/image_function.rs` uses) — it
//! never calls into `ImageFunction`. The destination is always the layer's
//! own `MainImage`; the source is resolved from a script `Layer`/`Bitmap`
//! argument.
//!
//! Reference: `tTJSNI_BaseLayer` in `reference/cpp/core/visual/LayerIntf.cpp`
//! (`ColorRect` 4338, `CopyRect` 4574, `OperateRect` 5224, `DoBoxBlur` 5691,
//! `AdjustGamma` 5715, `ApplyGaussianBlur` 5749, `DoGrayScale` 5898,
//! `LRFlip` 5910, `UDFlip` 5925, ...).
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use crate::engine_api::layer_ops::{self, GammaAdjust, RectI};
use crate::engine_api::{BitmapState, DecodedImageKey, DecodedImageMode, DestAlpha, Scene};
use api::blend::{BlendMode, DstBlend};
use api::media::DecodedImage;
use tjs::{NativeInstanceMethodDef, Tjs2Engine, Value};

use super::super::bitmap::{
    COLOR_KEY_NONE, ImageLoadError, install_pending_image, load_cached_storage_image,
    start_pending_decode,
};
use super::super::common::scene_mut;
use super::super::common::{
    DEFAULT_FONT_FACE, DEFAULT_FONT_HEIGHT, actual_color_bytes, argb_to_rgba, bound,
    rgba_to_pixel_word, scene_ref, set_null_out, swap_red_blue,
};
use super::LayerInst;
use super::util::{
    self, clip_dest_src, layer_clip, main_bitmap, normalize_rect, operation_mode_from_layer_type,
    opt_bool, opt_f64, opt_i64, resolve_source_bitmap, resolve_source_layer, scale_rect,
    value_as_f64, with_two_bitmaps,
};
use crate::CompatCtx;
use crate::native_util::{args, report_error, set_int, set_void};
use crate::natives::system::value_as_i64;
use crate::resource::{TextDrawParams, draw_text_into_bitmap, glyph_text, resource_state};

/// Operation modes (`tTVPBlendOperationMode`).
const OM_OPAQUE: i64 = 1;
const OM_ALPHA: i64 = 2;
const OM_AUTO: i64 = 128;
/// Draw faces (`tTVPDrawFace`, `reference/cpp/core/visual/LayerIntf.h:59-68`).
pub(super) const DF_ALPHA: i32 = 0;
pub(super) const DF_OPAQUE: i32 = 1;
pub(super) const DF_MASK: i32 = 2;
pub(super) const DF_PROVINCE: i32 = 3;
pub(super) const DF_ADD_ALPHA: i32 = 4;
/// `dfAuto` — the face is chosen automatically from the layer type
/// (`LayerIntf.h:67`).
const DF_AUTO: i32 = 128;

// ---------------------------------------------------------------------------
// Small geometry helpers
// ---------------------------------------------------------------------------

/// Whether the bitmap's buffer covers every pixel its declared size names.
///
/// A bitmap can carry a size with no pixels behind it: a decode that has not
/// landed keeps the probed size and an empty buffer, and a blank image that
/// could not be allocated leaves the same state. The pixel helpers below read
/// such a bitmap as empty and report it once for the operation.
fn pixels_present(bitmap: &BitmapState) -> bool {
    usize::try_from(bitmap.width)
        .ok()
        .zip(usize::try_from(bitmap.height).ok())
        .and_then(|(width, height)| width.checked_mul(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .is_some_and(|bytes| bytes <= bitmap.rgba.len())
}

/// Extract `[left, top, right, bottom)` of `src` into a new standalone
/// bitmap (the scratch the rect-limited box blur runs on).
///
/// A source that holds no pixels yields an empty region: the missing pixels
/// stay transparent and the operation reports the image once.
fn extract_region(src: &BitmapState, rect: RectI) -> BitmapState {
    let width = (rect.2 - rect.0) as u32;
    let height = (rect.3 - rect.1) as u32;
    let mut out = BitmapState::new(
        width,
        height,
        vec![0u8; width as usize * height as usize * 4],
    );
    if !pixels_present(src) {
        log::error!(
            "Layer: the blur source holds {} pixel bytes for a {}x{} image; the region stays empty",
            src.rgba.len(),
            src.width,
            src.height
        );
        return out;
    }
    let src_width = src.width as usize;
    let dst_width = width as usize;
    for y in rect.1..rect.3 {
        for x in rect.0..rect.2 {
            let si = (y as usize * src_width + x as usize) * 4;
            let di = ((y - rect.1) as usize * dst_width + (x - rect.0) as usize) * 4;
            let Some(pixel) = src.rgba.get(si..si + 4) else {
                continue;
            };
            let Some(target) = out.rgba.get_mut(di..di + 4) else {
                continue;
            };
            target.copy_from_slice(pixel);
        }
    }
    out
}

/// Restore the alpha plane of `dst` (an extracted region) from `src`.
///
/// A destination that holds no pixels keeps its plane as it is, and the
/// operation reports the image once.
fn restore_region_alpha(src: &BitmapState, dst: &mut BitmapState, rect: RectI) {
    if !pixels_present(dst) {
        log::error!(
            "Layer: the blur target holds {} pixel bytes for a {}x{} image; the alpha plane \
             is not restored",
            dst.rgba.len(),
            dst.width,
            dst.height
        );
        return;
    }
    let src_width = src.width as usize;
    let dst_width = dst.width as usize;
    for y in rect.1..rect.3 {
        for x in rect.0..rect.2 {
            let si = (y as usize * src_width + x as usize) * 4 + 3;
            let di = ((y - rect.1) as usize * dst_width + (x - rect.0) as usize) * 4 + 3;
            let (Some(value), Some(target)) = (src.rgba.get(si), dst.rgba.get_mut(di)) else {
                continue;
            };
            *target = *value;
        }
    }
}

/// Restore the RGB planes of `dst` (an extracted region) from `src`.
///
/// A destination that holds no pixels keeps its planes as they are, and the
/// operation reports the image once.
fn restore_region_rgb(src: &BitmapState, dst: &mut BitmapState, rect: RectI) {
    if !pixels_present(dst) {
        log::error!(
            "Layer: the blur target holds {} pixel bytes for a {}x{} image; the colour planes \
             are not restored",
            dst.rgba.len(),
            dst.width,
            dst.height
        );
        return;
    }
    let src_width = src.width as usize;
    let dst_width = dst.width as usize;
    for y in rect.1..rect.3 {
        for x in rect.0..rect.2 {
            let si = (y as usize * src_width + x as usize) * 4;
            let di = ((y - rect.1) as usize * dst_width + (x - rect.0) as usize) * 4;
            let (Some(pixel), Some(target)) =
                (src.rgba.get(si..si + 3), dst.rgba.get_mut(di..di + 3))
            else {
                continue;
            };
            target.copy_from_slice(pixel);
        }
    }
}

/// The alpha bytes of `bitmap` inside `clip`, in row-major order.
///
/// The L2 affine primitive has no `hold_alpha` argument, so the
/// `HoldAlpha` behaviour is emulated by restoring the alpha the destination
/// held before the pass.
///
/// # Errors
///
/// Returns an error naming the image when its buffer does not cover the
/// alpha plane of the clipped region.
fn snapshot_alpha_in_clip(bitmap: &BitmapState, clip: RectI) -> Result<Vec<u8>, String> {
    let Some(region) = layer_ops::intersect(bitmap, clip) else {
        return Ok(Vec::new());
    };
    let width = bitmap.width as usize;
    let region_width = (region.2 - region.0) as usize;
    let region_height = (region.3 - region.1) as usize;
    let mut alpha = Vec::with_capacity(region_width * region_height);
    for y in region.1..region.3 {
        for x in region.0..region.2 {
            let index = (y as usize * width + x as usize) * 4 + 3;
            let Some(value) = bitmap.rgba.get(index) else {
                return Err(format!(
                    "Layer: the affine target holds {} pixel bytes for a {}x{} image; the alpha \
                     plane inside the clip is not kept",
                    bitmap.rgba.len(),
                    bitmap.width,
                    bitmap.height
                ));
            };
            alpha.push(*value);
        }
    }
    Ok(alpha)
}

/// Restore the alpha plane inside `clip` from a snapshot taken by
/// [`snapshot_alpha_in_clip`].
///
/// # Errors
///
/// Returns an error naming the image when its buffer does not cover the
/// alpha plane of the clipped region.
fn restore_alpha_in_clip(dst: &mut BitmapState, clip: RectI, alpha: &[u8]) -> Result<(), String> {
    let Some(region) = layer_ops::intersect(dst, clip) else {
        return Ok(());
    };
    let width = dst.width as usize;
    let mut index = 0usize;
    for y in region.1..region.3 {
        for x in region.0..region.2 {
            let Some(value) = alpha.get(index) else {
                return Err(format!(
                    "Layer: the affine target holds {} pixel bytes for a {}x{} image; the alpha \
                     plane inside the clip is not restored",
                    dst.rgba.len(),
                    dst.width,
                    dst.height
                ));
            };
            let target = (y as usize * width + x as usize) * 4 + 3;
            let Some(slot) = dst.rgba.get_mut(target) else {
                return Err(format!(
                    "Layer: the affine target holds {} pixel bytes for a {}x{} image; the alpha \
                     plane inside the clip is not restored",
                    dst.rgba.len(),
                    dst.width,
                    dst.height
                ));
            };
            *slot = *value;
            index += 1;
        }
    }
    Ok(())
}

/// Clip a source rectangle to the source image's bounds, moving the
/// destination origin with it.
///
/// A rectangle that reaches past the source image contributes only the part
/// that is inside it; the destination moves with the part that is left, so
/// the pixels that are copied stay aligned with the source.
fn clip_source_to_image(
    dx: i32,
    dy: i32,
    src: RectI,
    width: u32,
    height: u32,
) -> Option<(i32, i32, RectI)> {
    let mut dx = dx;
    let mut dy = dy;
    let (mut left, mut top, mut right, mut bottom) = (src.0, src.1, src.2, src.3);
    if left < 0 {
        dx = dx.saturating_sub(left);
        left = 0;
    }
    if top < 0 {
        dy = dy.saturating_sub(top);
        top = 0;
    }
    let (Ok(image_right), Ok(image_bottom)) = (i32::try_from(width), i32::try_from(height)) else {
        return None;
    };
    right = right.min(image_right);
    bottom = bottom.min(image_bottom);
    if right <= left || bottom <= top {
        return None;
    }
    Some((dx, dy, (left, top, right, bottom)))
}

/// The destination bitmap + `HoldAlpha` flag of a layer, or `None` when its
/// MainImage has been deallocated (`Layer.hasImage = false`).
fn dest_info(scene: &mut Scene, id: u32, method: &str) -> Result<Option<(u32, bool)>, String> {
    // The layer's fields are copied out before the scene is borrowed again:
    // the resolve below needs the whole scene.
    let (bitmap, hold_alpha) = {
        let layer = scene
            .layer(id)
            .ok_or_else(|| format!("Layer.{method}: this layer no longer exists"))?;
        (layer.bitmap, layer.hold_alpha)
    };
    let Some(bitmap) = bitmap else {
        return Ok(None);
    };
    // The image may still be decoding: every operation on it waits, and an
    // image that resolved without its pixels is a typed error rather than an
    // empty buffer for the caller to index.
    main_bitmap(scene, id, method)?;
    Ok(Some((bitmap, hold_alpha)))
}

/// Mark a layer's image modified after a pixel write. The `imageModified`
/// flag is the signal scripts read to decide whether the layer must be
/// cleared before it is drawn again.
pub(super) fn mark_image_modified(scene: &mut Scene, id: u32) {
    if let Some(layer) = scene.layer_mut(id) {
        layer.image_modified = true;
    }
}

/// The layer's effective draw face — the port of the reference
/// `UpdateDrawFace` (`reference/cpp/core/visual/LayerIntf.cpp:1525-1620`):
/// when `face == dfAuto` the face is derived from the layer type, otherwise
/// the stored face is used verbatim.
pub(super) fn draw_face(scene: &Scene, id: u32) -> i32 {
    let Some(layer) = scene.layer(id) else {
        return DF_ALPHA;
    };
    if layer.face != DF_AUTO {
        return layer.face;
    }
    blend_face(layer.blend)
}

/// `UpdateDrawFace`'s type→face table for `Face == dfAuto`
/// (`LayerIntf.cpp:1529-1618`): every Photoshop `ltPs*` type resolves to
/// `dfAlpha`, `ltAddAlpha` resolves to `dfAddAlpha`, and the remaining types
/// (`ltOpaque` and the non-Photoshop additive/subtractive/multiplicative/
/// screen/dodge/darken/lighten blends) resolve to `dfOpaque`.
const fn blend_face(blend: BlendMode) -> i32 {
    match blend {
        BlendMode::Replace => DF_OPAQUE,
        BlendMode::SourceOver
        | BlendMode::PsNormal
        | BlendMode::PsAdditive
        | BlendMode::PsSubtractive
        | BlendMode::PsMultiplicative
        | BlendMode::PsScreen => DF_ALPHA,
        BlendMode::Additive => DF_OPAQUE,
        BlendMode::Subtractive => DF_OPAQUE,
        BlendMode::Multiply => DF_OPAQUE,
        BlendMode::Screen => DF_OPAQUE,
        BlendMode::AddAlpha => DF_ADD_ALPHA,
        BlendMode::DestinationDependent(
            DstBlend::ColorDodge | DstBlend::Darken | DstBlend::Lighten,
        ) => DF_OPAQUE,
        BlendMode::DestinationDependent(
            DstBlend::ColorDodge5
            | DstBlend::ColorBurn
            | DstBlend::Difference
            | DstBlend::Difference5
            | DstBlend::Exclusion
            | DstBlend::Overlay
            | DstBlend::HardLight
            | DstBlend::SoftLight
            | DstBlend::PsColorDodge
            | DstBlend::PsDarken
            | DstBlend::PsLighten,
        ) => DF_ALPHA,
    }
}

/// `GetBltMethodFromOperationModeAndDrawFace`
/// (`reference/cpp/core/visual/LayerIntf.cpp:4118-4271`): the face-sensitive
/// modes `omAlpha` / `omAddAlpha` / `omOpaque` only know the `dfAlpha`,
/// `dfAddAlpha` and `dfOpaque` faces — any other destination face makes the
/// reference raise `TVPNotDrawableFaceType`. The face-independent modes
/// (additive, subtractive, … , every `omPs*`) accept any face.
fn check_operate_face(mode: i64, face: i32, method: &str) -> Result<(), String> {
    if FACE_SENSITIVE_MODES.contains(&mode) && !matches!(face, DF_ALPHA | DF_OPAQUE | DF_ADD_ALPHA)
    {
        return Err(unsupported_face(method));
    }
    Ok(())
}

/// The operation modes whose blend method the draw face selects: the
/// `omAlpha`, `omAddAlpha` and `omOpaque` modes. Every other mode blends
/// the same way whatever the face is.
const FACE_SENSITIVE_MODES: [i64; 3] = [OM_ALPHA, OM_OPAQUE, super::blend::LT_ADD_ALPHA];

/// The destination's stored alpha handling a draw face names.
///
/// `dfAddAlpha` is the face of a layer that stores an additive (premultiplied)
/// alpha, `dfOpaque` the face that ignores its alpha, and every other face
/// stores a straight alpha.
const fn face_dst_alpha(face: i32) -> DestAlpha {
    match face {
        DF_ADD_ALPHA => DestAlpha::Additive,
        DF_OPAQUE => DestAlpha::Opaque,
        _ => DestAlpha::Straight,
    }
}

/// The blend one (operation mode, draw face) pair selects, and what the pass
/// takes from the layer.
///
/// The pair names a blend method. The engine carries the operation mode whose
/// kernel applies, the destination's stored alpha handling selects the
/// method's "on an additive alpha" spelling where it has one, and the two
/// flags say what the layer contributes: whether the source is read as an
/// opaque image and whether the pass holds the destination's alpha.
#[derive(Clone, Copy)]
struct FaceBlend {
    /// The engine's operation mode the method runs as.
    mode: i64,
    /// Whether the source is read as an opaque image.
    treat_as_opaque: bool,
    /// Whether the pass holds the destination's alpha.
    hold_alpha: bool,
    /// The destination's stored alpha handling the method runs against.
    dst_alpha: DestAlpha,
}

impl FaceBlend {
    /// A method that blends `mode` and takes nothing else from the layer.
    const fn new(mode: i64, dst_alpha: DestAlpha) -> FaceBlend {
        FaceBlend {
            mode,
            treat_as_opaque: false,
            hold_alpha: false,
            dst_alpha,
        }
    }

    /// The same, with the destination's alpha held.
    const fn holding(mode: i64, dst_alpha: DestAlpha) -> FaceBlend {
        FaceBlend {
            mode,
            treat_as_opaque: false,
            hold_alpha: true,
            dst_alpha,
        }
    }

    /// A method that reads the source as an opaque image and holds the
    /// destination's alpha with `hold`.
    const fn opaque_source(mode: i64, dst_alpha: DestAlpha, hold: bool) -> FaceBlend {
        FaceBlend {
            mode,
            treat_as_opaque: true,
            hold_alpha: hold,
            dst_alpha,
        }
    }
}

/// The blend one (operation mode, draw face) pair selects.
///
/// `check_operate_face` has already rejected every face these modes do not
/// know, so each arm below names an existing method for the pair. A pair whose
/// method has no implementation yields `None`: the pass leaves the
/// destination untouched.
fn face_blend(mode: i64, face: i32, layer_hold_alpha: bool, opa: u8) -> Option<FaceBlend> {
    let dst_alpha = face_dst_alpha(face);
    // The copy methods' hold spelling exists at the full opacity alone: the
    // below-full-opacity spelling is the same with and without the flag.
    let hold = layer_hold_alpha && opa == 255;
    match mode {
        // `omAlpha`: the source's alpha against the destination's.
        OM_ALPHA => Some(match face {
            // `bmAlpha` on an opaque face mixes the colours without an alpha
            // correction and holds the destination's alpha. Its kernel is the
            // one the Photoshop normal mode uses.
            DF_OPAQUE => FaceBlend::new(super::blend::LT_PS_NORMAL, dst_alpha),
            // `bmAlphaOnAlpha` and `bmAlphaOnAddAlpha`.
            _ => FaceBlend::new(OM_ALPHA, dst_alpha),
        }),
        // `omAddAlpha`: the additive-alpha blend.
        super::blend::LT_ADD_ALPHA => match face {
            // `bmAddAlphaOnAlpha` is the one method that is not implemented:
            // the operation leaves the destination untouched.
            DF_ALPHA => None,
            // `bmAddAlpha` and `bmAddAlphaOnAddAlpha`.
            _ => Some(FaceBlend::new(super::blend::LT_ADD_ALPHA, dst_alpha)),
        },
        // `omOpaque`: the copy of the source.
        OM_OPAQUE => match face {
            // `bmCopyOnAddAlpha` has no hold spelling at the full opacity, so
            // there is nothing to draw with.
            DF_ADD_ALPHA if hold => None,
            // `bmCopy`: the plain copy, with its hold spelling above.
            DF_OPAQUE => Some(if hold {
                FaceBlend::holding(OM_OPAQUE, dst_alpha)
            } else {
                FaceBlend::new(OM_OPAQUE, dst_alpha)
            }),
            // `bmCopyOnAlpha` and `bmCopyOnAddAlpha` copy the source as an
            // opaque image.
            _ => Some(FaceBlend::opaque_source(OM_OPAQUE, dst_alpha, hold)),
        },
        // Every other mode is face-independent: one method whatever the face,
        // and the destination's alpha handling is all it takes.
        _ => Some(FaceBlend::new(mode, dst_alpha)),
    }
}

/// The blend a blit method runs on a draw face.
///
/// The four blends are the plain copy, the copy of an opaque source, the
/// colour mix and the alpha-correct source-over.
#[derive(Clone, Copy)]
enum Blit {
    /// The plain copy: the source's colour and alpha replace the
    /// destination's.
    Copy,
    /// The copy of an opaque source: the source is read as an opaque image,
    /// so the source's colour replaces the destination's and the
    /// destination's alpha becomes opaque.
    CopyOnAlpha,
    /// The colour mix: the source's alpha mixes the colours and the
    /// destination's alpha is held.
    Alpha,
    /// The alpha-correct source-over: the destination's alpha takes part in
    /// the mix.
    AlphaOnAlpha,
}

impl Blit {
    /// The engine's operation mode the blend runs as.
    fn mode(self) -> i64 {
        match self {
            Blit::Copy | Blit::CopyOnAlpha => OM_OPAQUE,
            Blit::Alpha => super::blend::LT_PS_NORMAL,
            Blit::AlphaOnAlpha => OM_ALPHA,
        }
    }

    /// Whether the blend reads the source as an opaque image.
    fn reads_source_as_opaque(self) -> bool {
        matches!(self, Blit::CopyOnAlpha)
    }
}

/// The blit families: the methods that name their blend per draw face.
#[derive(Clone, Copy)]
enum BlitFamily {
    /// The pile methods: the source is mixed into the destination.
    Pile,
    /// The blend methods: the source is copied into the destination.
    Blend,
    /// The copy methods: the source is copied as it is.
    Copy,
}

/// The blend a family runs on a draw face, and the hold argument it passes.
///
/// The pile mixes the source into the destination: with the source's alpha
/// on an alpha face, and with the colour mix on an opaque face. The blend
/// copies the source: as an opaque image on an alpha face, and with its own
/// alpha on an opaque face. The copy copies the source as it is.
///
/// The copy blends take the layer's hold argument; the alpha blends ignore
/// it. The copy methods pass the hold argument on the opaque face alone —
/// the alpha faces copy the source's own alpha — and at the full opacity
/// alone, because the below-full-opacity spelling is the same with and
/// without the flag.
fn family_blit(family: BlitFamily, face: i32, layer_hold_alpha: bool, opa: u8) -> FaceBlit {
    let dst_alpha = face_dst_alpha(face);
    let hold = layer_hold_alpha && opa == 255;
    match family {
        BlitFamily::Pile => FaceBlit {
            blend: if face == DF_OPAQUE {
                Blit::Alpha
            } else {
                Blit::AlphaOnAlpha
            },
            hold: false,
            dst_alpha,
        },
        BlitFamily::Blend => FaceBlit {
            blend: if face == DF_OPAQUE {
                Blit::Copy
            } else {
                Blit::CopyOnAlpha
            },
            hold,
            dst_alpha,
        },
        BlitFamily::Copy => FaceBlit {
            blend: Blit::Copy,
            hold: face == DF_OPAQUE && hold,
            dst_alpha,
        },
    }
}

/// The blend a family runs on a draw face, and the hold argument it passes.
#[derive(Clone, Copy)]
struct FaceBlit {
    /// The blend the family runs.
    blend: Blit,
    /// Whether the family passes the layer's hold argument to the blend.
    hold: bool,
    /// The destination's stored alpha handling the blend runs against.
    dst_alpha: DestAlpha,
}

/// Whether the operation mode is one the engine accepts.
///
/// The `tTVPBlendOperationMode` enum names the modes it accepts: the
/// Photoshop modes (13 to 28), the plain modes (1 to 5 and 8 to 12) and the
/// automatic mode (128). Every other number is rejected; the layer-effect
/// and layer-filter types (6 and 7) are not operation modes.
fn is_operation_mode(mode: i64) -> bool {
    matches!(mode, 1..=5 | 8..=28)
}

/// The raise an operation carries when the mode and the face name no blend
/// method.
///
/// A mode the layer does not know and a face the mode does not know raise the
/// same text, which names the member and keeps the established spelling.
fn unsupported_face(method: &str) -> String {
    format!("Not drawble face type {method}")
}

/// Resolve an `omAuto` operation mode against the source layer's blend.
fn resolve_mode(mode: i64, src_type: Option<BlendMode>, method: &str) -> Result<i64, String> {
    let mode = if mode != OM_AUTO {
        mode
    } else {
        match src_type {
            Some(blend) => operation_mode_from_layer_type(blend),
            None => {
                // A Bitmap source has no layer blend; the reference keeps `omAlpha`.
                OM_ALPHA
            }
        }
    };
    if !is_operation_mode(mode) {
        return Err(unsupported_face(method));
    }
    Ok(mode)
}

/// Build the three destination corners for the `affine`/points argument
/// convention shared by `affineCopy`/`affinePile`/`affineBlend`/
/// `operateAffine`.
fn affine_points(
    affine: bool,
    coords: &[Value],
    src_rect: RectI,
) -> ((f64, f64), (f64, f64), (f64, f64)) {
    let c = [
        value_as_f64(&coords[0]),
        value_as_f64(&coords[1]),
        value_as_f64(&coords[2]),
        value_as_f64(&coords[3]),
        value_as_f64(&coords[4]),
        value_as_f64(&coords[5]),
    ];
    if affine {
        let [a, b, cc, d, tx, ty] = c;
        let width = f64::from(src_rect.2 - src_rect.0);
        let height = f64::from(src_rect.3 - src_rect.1);
        let map = |x: f64, y: f64| (a * x + cc * y + tx, b * x + d * y + ty);
        (
            map(-0.5, -0.5),
            map(width - 0.5, -0.5),
            map(-0.5, height - 0.5),
        )
    } else {
        ((c[0], c[1]), (c[2], c[3]), (c[4], c[5]))
    }
}

struct AffineRun<'a> {
    scene: &'a mut Scene,
    dst_id: u32,
    src_id: u32,
    corners: [(f64, f64); 3],
    src_rect: RectI,
    stretch_type: i64,
    mode: i64,
    opa: u8,
    hold_alpha: bool,
    treat_as_opaque: bool,
    dst_alpha: DestAlpha,
    clear: bool,
    neutral: [u8; 4],
    clip: RectI,
    pixel_center: bool,
}

/// Run an affine operation clipped to `clip`, emulating `HoldAlpha`.
fn affine_run(run: AffineRun<'_>) -> Result<(), String> {
    let AffineRun {
        scene,
        dst_id,
        src_id,
        corners: [p0, p1, p2],
        src_rect,
        stretch_type,
        mode,
        opa,
        hold_alpha,
        treat_as_opaque,
        dst_alpha,
        clear,
        neutral,
        clip,
        pixel_center,
    } = run;
    with_two_bitmaps(scene, dst_id, src_id, |dst, src| -> Result<(), String> {
        // The pass writes inside the clip alone, so the pixels outside it
        // keep their values; `HoldAlpha` still needs the alpha the
        // destination held inside the clip.
        let alpha = if hold_alpha {
            Some(snapshot_alpha_in_clip(dst, clip)?)
        } else {
            None
        };
        // The two entries differ in the corner convention alone.
        let affine = layer_ops::AffineBlit {
            corners: [p0, p1, p2],
            stretch_type,
            clear,
            clear_color: neutral,
        };
        let source = layer_ops::BlitSource {
            bitmap: src,
            rect: src_rect,
        };
        let options = layer_ops::BlitOptions {
            mode,
            opa,
            treat_as_opaque,
            ..Default::default()
        };
        if pixel_center {
            layer_ops::affine_blit_clipped_pixel_center_on(
                dst_alpha, dst, affine, source, options, clip,
            );
        } else {
            layer_ops::affine_blit_clipped_on(dst_alpha, dst, affine, source, options, clip);
        }
        if let Some(alpha) = alpha {
            restore_alpha_in_clip(dst, clip, &alpha)?;
        }
        Ok(())
    })??;
    Ok(())
}

// ---------------------------------------------------------------------------
// `operateRect` / `operateStretch` / `operateAffine`
// ---------------------------------------------------------------------------

/// `operateRect(dx, dy, src, sx, sy, sw, sh [, mode [, opa]])`.
extern "C" fn layer_operate_rect(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 7 {
        return report_error(out_error, "Layer.operateRect requires 7 arguments");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_operate_rect`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.operateRect: this layer is not constructed",
        );
    };
    let Ok((host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.operateRect: compat context is not bound");
    };
    let result: Result<(), String> = (|| {
        let dx = value_as_i64(&a[0]) as i32;
        let dy = value_as_i64(&a[1]) as i32;
        let (src_id, src_type) =
            resolve_source_bitmap(&ctx, host.engine(), &a[2], "operateRect", false)?;
        let sx = value_as_i64(&a[3]) as i32;
        let sy = value_as_i64(&a[4]) as i32;
        let sw = value_as_i64(&a[5]) as i32;
        let sh = value_as_i64(&a[6]) as i32;
        let mode = resolve_mode(opt_i64(a, 7, OM_AUTO), src_type, "operateRect")?;
        let opa = opt_i64(a, 8, 255).clamp(0, 255) as u8;
        let scene = scene_mut(&ctx)?;
        let Some((dst, layer_hold_alpha)) = dest_info(scene, id, "operateRect")? else {
            return Ok(());
        };
        let face = draw_face(scene, id);
        let clip = layer_clip(scene, id);
        let src_rect = (sx, sy, sx.saturating_add(sw), sy.saturating_add(sh));
        let Some((dx, dy, src_rect)) = clip_dest_src(dx, dy, src_rect, clip) else {
            return Ok(());
        };
        // Reference order: clip first (`LayerIntf.cpp:5232-5234`), then the
        // mode/face validation (`LayerIntf.cpp:5235-5242`).
        check_operate_face(mode, face, "operateRect")?;
        let Some(blit) = face_blend(mode, face, layer_hold_alpha, opa) else {
            return Ok(());
        };
        // A zero opacity draws nothing.
        if opa == 0 {
            return Ok(());
        }
        with_two_bitmaps(scene, dst, src_id, |d, s| {
            layer_ops::operate_rect_on(
                blit.dst_alpha,
                d,
                dx,
                dy,
                layer_ops::BlitSource {
                    bitmap: s,
                    rect: src_rect,
                },
                layer_ops::BlitOptions {
                    mode: blit.mode,
                    opa,
                    hold_alpha: blit.hold_alpha,
                    treat_as_opaque: blit.treat_as_opaque,
                },
            );
        })?;
        mark_image_modified(scene, id);
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `operateStretch(dx, dy, dw, dh, src, sx, sy, sw, sh [, mode [, opa
/// [, type]]])`.
extern "C" fn layer_operate_stretch(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 9 {
        return report_error(out_error, "Layer.operateStretch requires 9 arguments");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_operate_stretch`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.operateStretch: this layer is not constructed",
        );
    };
    let Ok((host, ctx)) = super::bound(engine) else {
        return report_error(
            out_error,
            "Layer.operateStretch: compat context is not bound",
        );
    };
    let result: Result<(), String> = (|| {
        let dx = value_as_i64(&a[0]) as i32;
        let dy = value_as_i64(&a[1]) as i32;
        let dw = value_as_i64(&a[2]) as i32;
        let dh = value_as_i64(&a[3]) as i32;
        let (src_id, src_type) =
            resolve_source_bitmap(&ctx, host.engine(), &a[4], "operateStretch", false)?;
        let sx = value_as_i64(&a[5]) as i32;
        let sy = value_as_i64(&a[6]) as i32;
        let sw = value_as_i64(&a[7]) as i32;
        let sh = value_as_i64(&a[8]) as i32;
        let mode = resolve_mode(opt_i64(a, 9, OM_AUTO), src_type, "operateStretch")?;
        let opa = opt_i64(a, 10, 255).clamp(0, 255) as u8;
        let stretch_type = opt_i64(a, 11, 0);
        let scene = scene_mut(&ctx)?;
        let Some((dst, layer_hold_alpha)) = dest_info(scene, id, "operateStretch")? else {
            return Ok(());
        };
        let face = draw_face(scene, id);
        let clip = layer_clip(scene, id);
        let dst_rect = normalize_rect((dx, dy, dx.saturating_add(dw), dy.saturating_add(dh)));
        let Some(dest) = layer_ops::intersect_rect(dst_rect, clip) else {
            return Ok(());
        };
        // Reference order: clip first (`LayerIntf.cpp:5387-5390`), then the
        // mode/face validation (`LayerIntf.cpp:5392-5399`).
        check_operate_face(mode, face, "operateStretch")?;
        let Some(blit) = face_blend(mode, face, layer_hold_alpha, opa) else {
            return Ok(());
        };
        let src_rect = (sx, sy, sx.saturating_add(sw), sy.saturating_add(sh));
        let src_trimmed = scale_rect(src_rect, dst_rect, dest);
        with_two_bitmaps(scene, dst, src_id, |d, s| {
            layer_ops::stretch_blit_on(
                blit.dst_alpha,
                d,
                dest,
                layer_ops::BlitSource {
                    bitmap: s,
                    rect: src_trimmed,
                },
                stretch_type,
                layer_ops::BlitOptions {
                    mode: blit.mode,
                    opa,
                    hold_alpha: blit.hold_alpha,
                    treat_as_opaque: blit.treat_as_opaque,
                },
            );
        })?;
        mark_image_modified(scene, id);
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// Shared body of the four affine methods.
fn affine_op(
    argc: c_int,
    a: &[Value],
    id: u32,
    ctx: &CompatCtx,
    host: &Tjs2Engine,
    method: &str,
    kind: AffineKind,
) -> Result<(), String> {
    if argc < 12 {
        return Err(format!("Layer.{method} requires 12 arguments"));
    }
    // Reference source classes: `affineCopy`/`operateAffine` accept Layer or
    // Bitmap (`LayerIntf.cpp:9227-9251`, `9423-9447`), `affinePile` and
    // `affineBlend` only a Layer (`LayerIntf.cpp:9299-9316`, `9361-9378`).
    let (src_id, src_type) = resolve_source_bitmap(
        ctx,
        host,
        &a[0],
        method,
        matches!(kind, AffineKind::Pile | AffineKind::Blend),
    )?;
    let sx = value_as_i64(&a[1]) as i32;
    let sy = value_as_i64(&a[2]) as i32;
    let sw = value_as_i64(&a[3]) as i32;
    let sh = value_as_i64(&a[4]) as i32;
    let affine = crate::natives::system::value_as_bool(&a[5]);
    let src_rect = (sx, sy, sx.saturating_add(sw), sy.saturating_add(sh));
    let coords = &a[6..12];
    let (p0, p1, p2) = affine_points(affine, coords, src_rect);
    let (mode, opa, stretch_type, clear) = match kind {
        AffineKind::Copy => {
            let stretch_type = opt_i64(a, 12, 0);
            let clear = opt_bool(a, 13, false);
            (OM_OPAQUE, 255, stretch_type, clear)
        }
        AffineKind::Pile => {
            let opa = opt_i64(a, 12, 255).clamp(0, 255) as u8;
            let stretch_type = opt_i64(a, 13, 0);
            (OM_ALPHA, opa, stretch_type, false)
        }
        AffineKind::Blend => {
            let opa = opt_i64(a, 12, 255).clamp(0, 255) as u8;
            let stretch_type = opt_i64(a, 13, 0);
            (OM_ALPHA, opa, stretch_type, false)
        }
        AffineKind::Operate => {
            let mode = resolve_mode(opt_i64(a, 12, OM_AUTO), src_type, method)?;
            let opa = opt_i64(a, 13, 255).clamp(0, 255) as u8;
            let stretch_type = opt_i64(a, 14, 0);
            (mode, opa, stretch_type, false)
        }
    };
    let scene = scene_mut(ctx)?;
    let Some((dst, layer_hold_alpha)) = dest_info(scene, id, method)? else {
        return Ok(());
    };
    // Reference face gates: `AffineCopy` requires `dfAlpha`/`dfAddAlpha`/
    // `dfOpaque` (`LayerIntf.cpp:4733-4786`), `AffinePile`/`AffineBlend`
    // require `dfAlpha`/`dfOpaque` (`LayerIntf.cpp:5427-5431`),
    // `OperateAffine` validates the mode/face pair
    // (`LayerIntf.cpp:5626-5638`).
    let face = draw_face(scene, id);
    match kind {
        AffineKind::Copy => {
            if !matches!(face, DF_ALPHA | DF_ADD_ALPHA | DF_OPAQUE) {
                return Err(format!("Layer.{method}: unsupported draw face {face}"));
            }
        }
        AffineKind::Pile | AffineKind::Blend => {
            if !matches!(face, DF_ALPHA | DF_OPAQUE) {
                return Err(format!("Layer.{method}: unsupported draw face {face}"));
            }
        }
        AffineKind::Operate => {
            check_operate_face(mode, face, method)?;
        }
    }
    let clip = layer_clip(scene, id);
    let neutral_color = {
        let color = scene.layer(id).map_or(0, |l| l.neutral_color);
        argb_to_rgba(color)
    };
    // The blend an affine method runs is named by the method and the draw
    // face. The affine methods do not share the operation-mode table the
    // layer's draw path uses; each of them names its own blend method for
    // each face.
    let blit = match kind {
        AffineKind::Copy => {
            let blit = family_blit(BlitFamily::Copy, face, layer_hold_alpha, opa);
            Some(FaceBlend {
                mode: blit.blend.mode(),
                treat_as_opaque: blit.blend.reads_source_as_opaque(),
                hold_alpha: blit.hold,
                dst_alpha: blit.dst_alpha,
            })
        }
        AffineKind::Pile => {
            let blit = family_blit(BlitFamily::Pile, face, layer_hold_alpha, opa);
            Some(FaceBlend {
                mode: blit.blend.mode(),
                treat_as_opaque: blit.blend.reads_source_as_opaque(),
                hold_alpha: blit.hold,
                dst_alpha: blit.dst_alpha,
            })
        }
        AffineKind::Blend => {
            let blit = family_blit(BlitFamily::Blend, face, layer_hold_alpha, opa);
            Some(FaceBlend {
                mode: blit.blend.mode(),
                treat_as_opaque: blit.blend.reads_source_as_opaque(),
                hold_alpha: blit.hold,
                dst_alpha: blit.dst_alpha,
            })
        }
        AffineKind::Operate => face_blend(mode, face, layer_hold_alpha, opa),
    };
    let Some(blit) = blit else {
        return Ok(());
    };
    let pixel_center = affine || p0.0.fract() != 0.0 || p0.1.fract() != 0.0;
    affine_run(AffineRun {
        scene,
        dst_id: dst,
        src_id,
        corners: [p0, p1, p2],
        src_rect,
        stretch_type,
        mode: blit.mode,
        opa,
        hold_alpha: blit.hold_alpha,
        treat_as_opaque: blit.treat_as_opaque,
        dst_alpha: blit.dst_alpha,
        clear,
        neutral: neutral_color,
        clip,
        pixel_center,
    })?;
    mark_image_modified(scene, id);
    Ok(())
}

#[derive(Clone, Copy)]
enum AffineKind {
    Copy,
    Pile,
    Blend,
    Operate,
}

macro_rules! affine_method {
    ($name:ident, $method:literal, $kind:expr) => {
        extern "C" fn $name(
            engine: *mut c_void,
            instance: *mut c_void,
            argc: c_int,
            argv: *const Value,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
            let a = unsafe { args(argv, argc) };
            // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live during this callback.
            let inst = unsafe { &*(instance as *const LayerInst) };
            let Some(id) = inst.id else {
                return report_error(
                    out_error,
                    concat!("Layer.", $method, ": this layer is not constructed"),
                );
            };
            let Ok((host, ctx)) = super::bound(engine) else {
                return report_error(
                    out_error,
                    concat!("Layer.", $method, ": compat context is not bound"),
                );
            };
            match affine_op(argc, a, id, &ctx, host.engine(), $method, $kind) {
                Ok(()) => {
                    set_void(out);
                    0
                }
                Err(error) => report_error(out_error, &error),
            }
        }
    };
}

affine_method!(layer_affine_copy, "affineCopy", AffineKind::Copy);
affine_method!(layer_affine_pile, "affinePile", AffineKind::Pile);
affine_method!(layer_affine_blend, "affineBlend", AffineKind::Blend);
affine_method!(layer_operate_affine, "operateAffine", AffineKind::Operate);

// ---------------------------------------------------------------------------
// `stretchCopy` / `stretchPile` / `stretchBlend`
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum StretchKind {
    Copy,
    Pile,
    Blend,
}

fn stretch_op(
    a: &[Value],
    id: u32,
    ctx: &CompatCtx,
    host: &Tjs2Engine,
    method: &str,
    kind: StretchKind,
) -> Result<(), String> {
    // Reference source classes: `stretchCopy` accepts Layer or Bitmap
    // (`LayerIntf.cpp:9002-9027`), `stretchPile`/`stretchBlend` only a Layer
    // (`LayerIntf.cpp:9057-9070`, `9104-9117`).
    let (src_id, _src_type) = resolve_source_bitmap(
        ctx,
        host,
        &a[4],
        method,
        matches!(kind, StretchKind::Pile | StretchKind::Blend),
    )?;
    let dx = value_as_i64(&a[0]) as i32;
    let dy = value_as_i64(&a[1]) as i32;
    let dw = value_as_i64(&a[2]) as i32;
    let dh = value_as_i64(&a[3]) as i32;
    let sx = value_as_i64(&a[5]) as i32;
    let sy = value_as_i64(&a[6]) as i32;
    let sw = value_as_i64(&a[7]) as i32;
    let sh = value_as_i64(&a[8]) as i32;
    let scene = scene_mut(ctx)?;
    let Some((dst, layer_hold_alpha)) = dest_info(scene, id, method)? else {
        return Ok(());
    };
    let face = draw_face(scene, id);
    // Reference face gates: `StretchPile`/`StretchBlend` require
    // `dfAlpha`/`dfOpaque` *before* clipping (`LayerIntf.cpp:5264-5281`,
    // `5319-5336`); `StretchCopy` validates after the clip intersection
    // (`LayerIntf.cpp:4677-4710`) and lands in its own arm below.
    if matches!(kind, StretchKind::Pile | StretchKind::Blend)
        && !matches!(face, DF_ALPHA | DF_OPAQUE)
    {
        return Err(format!("Layer.{method}: unsupported draw face {face}"));
    }
    let clip = layer_clip(scene, id);
    let dst_rect = normalize_rect((dx, dy, dx.saturating_add(dw), dy.saturating_add(dh)));
    let Some(dest) = layer_ops::intersect_rect(dst_rect, clip) else {
        return Ok(());
    };
    let src_rect = (sx, sy, sx.saturating_add(sw), sy.saturating_add(sh));
    let src_trimmed = scale_rect(src_rect, dst_rect, dest);
    match kind {
        StretchKind::Copy => {
            // `StretchCopy` throws `TVPNotDrawableFaceType` for every face
            // outside `dfAlpha`/`dfAddAlpha`/`dfOpaque`
            // (`LayerIntf.cpp:4683-4710`).
            if !matches!(face, DF_ALPHA | DF_ADD_ALPHA | DF_OPAQUE) {
                return Err(format!("Layer.{method}: unsupported draw face {face}"));
            }
            let stretch_type = opt_i64(a, 9, 0);
            let plane = if face == DF_OPAQUE && layer_hold_alpha {
                layer_ops::COPY_MAIN
            } else {
                layer_ops::COPY_MAIN | layer_ops::COPY_MASK
            };
            with_two_bitmaps(scene, dst, src_id, |d, s| {
                layer_ops::stretch_blit_plane(d, dest, s, src_trimmed, stretch_type, plane);
            })?;
        }
        StretchKind::Pile | StretchKind::Blend => {
            let opa = opt_i64(a, 9, 255).clamp(0, 255) as u8;
            let stretch_type = opt_i64(a, 10, 0);
            let family = if matches!(kind, StretchKind::Blend) {
                BlitFamily::Blend
            } else {
                BlitFamily::Pile
            };
            let blit = family_blit(family, face, layer_hold_alpha, opa);
            with_two_bitmaps(scene, dst, src_id, |d, s| {
                layer_ops::stretch_blit_on(
                    blit.dst_alpha,
                    d,
                    dest,
                    layer_ops::BlitSource {
                        bitmap: s,
                        rect: src_trimmed,
                    },
                    stretch_type,
                    layer_ops::BlitOptions {
                        mode: blit.blend.mode(),
                        opa,
                        hold_alpha: blit.hold,
                        treat_as_opaque: blit.blend.reads_source_as_opaque(),
                    },
                );
            })?;
        }
    }
    mark_image_modified(scene, id);
    Ok(())
}

macro_rules! stretch_method {
    ($name:ident, $method:literal, $kind:expr) => {
        extern "C" fn $name(
            engine: *mut c_void,
            instance: *mut c_void,
            argc: c_int,
            argv: *const Value,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            if argc < 9 {
                return report_error(
                    out_error,
                    concat!("Layer.", $method, " requires 9 arguments"),
                );
            }
            // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
            let a = unsafe { args(argv, argc) };
            // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live during this callback.
            let inst = unsafe { &*(instance as *const LayerInst) };
            let Some(id) = inst.id else {
                return report_error(
                    out_error,
                    concat!("Layer.", $method, ": this layer is not constructed"),
                );
            };
            let Ok((host, ctx)) = super::bound(engine) else {
                return report_error(
                    out_error,
                    concat!("Layer.", $method, ": compat context is not bound"),
                );
            };
            match stretch_op(a, id, &ctx, host.engine(), $method, $kind) {
                Ok(()) => {
                    set_void(out);
                    0
                }
                Err(error) => report_error(out_error, &error),
            }
        }
    };
}

stretch_method!(layer_stretch_copy, "stretchCopy", StretchKind::Copy);
stretch_method!(layer_stretch_pile, "stretchPile", StretchKind::Pile);
stretch_method!(layer_stretch_blend, "stretchBlend", StretchKind::Blend);

// ---------------------------------------------------------------------------
// `copyRect` / `pileRect` / `blendRect` / `piledCopy` / `copy9Patch`
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum RectKind {
    Copy,
    Pile,
    Blend,
    PiledCopy,
}

fn rect_op(
    a: &[Value],
    id: u32,
    ctx: &CompatCtx,
    host: &Tjs2Engine,
    method: &str,
    kind: RectKind,
) -> Result<(), String> {
    // Reference source classes: `copyRect` accepts Layer or Bitmap
    // (`LayerIntf.cpp:8779-8811`); `pileRect`, `blendRect` and `piledCopy`
    // only a Layer (`LayerIntf.cpp:8752-8767`, `8822-8840`, `8858-8876`).
    let (src_id, _src_type) = resolve_source_bitmap(
        ctx,
        host,
        &a[2],
        method,
        matches!(kind, RectKind::Pile | RectKind::Blend | RectKind::PiledCopy),
    )?;
    // `piledCopy` reads the source layer's composited subtree, so it also
    // needs the layer id. Resolve it before the mutable scene borrow.
    let piled_src_layer = if matches!(kind, RectKind::PiledCopy) {
        Some(resolve_source_layer(ctx, host, &a[2], method)?)
    } else {
        None
    };
    let dx = value_as_i64(&a[0]) as i32;
    let dy = value_as_i64(&a[1]) as i32;
    let sx = value_as_i64(&a[3]) as i32;
    let sy = value_as_i64(&a[4]) as i32;
    let sw = value_as_i64(&a[5]) as i32;
    let sh = value_as_i64(&a[6]) as i32;
    let scene = scene_mut(ctx)?;
    let Some((dst, hold_alpha)) = dest_info(scene, id, method)? else {
        return Ok(());
    };
    let face = draw_face(scene, id);
    let clip = layer_clip(scene, id);
    let src_rect = (sx, sy, sx.saturating_add(sw), sy.saturating_add(sh));
    let opa = opt_i64(a, 7, 255).clamp(0, 255) as u8;
    match kind {
        RectKind::PiledCopy => {
            // Copy the source layer's own MainImage composited with its
            // visible children, not its bare MainImage. A layer with no own
            // image is already rejected by the resolution above.
            let src_layer = piled_src_layer
                .ok_or_else(|| format!("Layer.{method}: the source must be a Layer object"))?;
            let content = layer_ops::render_layer_subtree(scene, src_layer)
                .ok_or_else(|| format!("Layer.{method}: cannot compose the source layer"))?;
            with_two_bitmaps(scene, dst, content, |d, s| {
                layer_ops::blit_plane(
                    d,
                    dx,
                    dy,
                    s,
                    src_rect,
                    clip,
                    layer_ops::COPY_MAIN | layer_ops::COPY_MASK,
                );
            })?;
        }
        RectKind::Copy => match face {
            DF_ALPHA | DF_ADD_ALPHA => {
                with_two_bitmaps(scene, dst, src_id, |d, s| {
                    layer_ops::blit_plane(
                        d,
                        dx,
                        dy,
                        s,
                        src_rect,
                        clip,
                        layer_ops::COPY_MAIN | layer_ops::COPY_MASK,
                    );
                })?;
            }
            DF_OPAQUE => {
                let plane = if hold_alpha {
                    layer_ops::COPY_MAIN
                } else {
                    layer_ops::COPY_MAIN | layer_ops::COPY_MASK
                };
                with_two_bitmaps(scene, dst, src_id, |d, s| {
                    layer_ops::blit_plane(d, dx, dy, s, src_rect, clip, plane);
                })?;
            }
            DF_MASK => {
                with_two_bitmaps(scene, dst, src_id, |d, s| {
                    layer_ops::blit_plane(d, dx, dy, s, src_rect, clip, layer_ops::COPY_MASK);
                })?;
            }
            DF_PROVINCE => {
                clear_province_region(scene, id, (dx, dy, sx, sy, sw, sh));
            }
            // Reference `CopyRect` has no `default:` case — any other face
            // (including a resolved-away `dfAuto`) falls through silently
            // (`reference/cpp/core/visual/LayerIntf.cpp:4574-4657`).
            _ => {}
        },
        RectKind::Pile | RectKind::Blend => {
            if face != DF_ALPHA && face != DF_OPAQUE {
                return Err(format!("Layer.{method}: unsupported draw face {face}"));
            }
            let family = if matches!(kind, RectKind::Pile) {
                BlitFamily::Pile
            } else {
                BlitFamily::Blend
            };
            let blit = family_blit(family, face, hold_alpha, opa);
            // A zero opacity draws nothing.
            if opa == 0 {
                return Ok(());
            }
            // The destination rectangle is clipped to the layer's clip
            // rectangle before the source rectangle is read.
            let Some((dx, dy, src_rect)) = clip_dest_src(dx, dy, src_rect, clip) else {
                return Ok(());
            };
            with_two_bitmaps(scene, dst, src_id, |d, s| {
                let Some((dx, dy, src_rect)) =
                    clip_source_to_image(dx, dy, src_rect, s.width, s.height)
                else {
                    return;
                };
                layer_ops::operate_rect_on(
                    blit.dst_alpha,
                    d,
                    dx,
                    dy,
                    layer_ops::BlitSource {
                        bitmap: s,
                        rect: src_rect,
                    },
                    layer_ops::BlitOptions {
                        mode: blit.blend.mode(),
                        opa,
                        hold_alpha: blit.hold,
                        treat_as_opaque: blit.blend.reads_source_as_opaque(),
                    },
                );
            })?;
        }
    }
    mark_image_modified(scene, id);
    Ok(())
}

/// Zero the province plane over the destination region (`copyRect` on the
/// `dfProvince` face with no province source).
fn clear_province_region(scene: &mut Scene, id: u32, args: (i32, i32, i32, i32, i32, i32)) {
    let (dx, dy, _sx, _sy, sw, sh) = args;
    let clip = scene
        .layer(id)
        .map_or(crate::engine_api::layer_ops::clip_rect(None), |l| {
            crate::engine_api::layer_ops::clip_rect(l.clip)
        });
    let Some((dx, dy, src_rect)) = clip_dest_src(dx, dy, (0, 0, sw, sh), clip) else {
        return;
    };
    let Some(layer) = scene.layer_mut(id) else {
        return;
    };
    let (Some(province), pw) = (layer.province.as_deref_mut(), layer.province_width) else {
        return;
    };
    let ph = layer.province_height;
    for y in dy.max(0)..dy.saturating_add(src_rect.3 - src_rect.1).min(ph as i32) {
        for x in dx.max(0)..dx.saturating_add(src_rect.2 - src_rect.0).min(pw as i32) {
            province[y as usize * pw as usize + x as usize] = 0;
        }
    }
    layer.image_modified = true;
}

macro_rules! rect_method {
    ($name:ident, $method:literal, $kind:expr) => {
        extern "C" fn $name(
            engine: *mut c_void,
            instance: *mut c_void,
            argc: c_int,
            argv: *const Value,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            if argc < 7 {
                return report_error(
                    out_error,
                    concat!("Layer.", $method, " requires 7 arguments"),
                );
            }
            // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
            let a = unsafe { args(argv, argc) };
            // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live during this callback.
            let inst = unsafe { &*(instance as *const LayerInst) };
            let Some(id) = inst.id else {
                return report_error(
                    out_error,
                    concat!("Layer.", $method, ": this layer is not constructed"),
                );
            };
            let Ok((host, ctx)) = super::bound(engine) else {
                return report_error(
                    out_error,
                    concat!("Layer.", $method, ": compat context is not bound"),
                );
            };
            match rect_op(a, id, &ctx, host.engine(), $method, $kind) {
                Ok(()) => {
                    set_void(out);
                    0
                }
                Err(error) => report_error(out_error, &error),
            }
        }
    };
}

rect_method!(layer_copy_rect, "copyRect", RectKind::Copy);
rect_method!(layer_pile_rect, "pileRect", RectKind::Pile);
rect_method!(layer_blend_rect, "blendRect", RectKind::Blend);
rect_method!(layer_piled_copy, "piledCopy", RectKind::PiledCopy);

/// `copy9Patch(src)` → the margin `Rect` or `void`.
extern "C" fn layer_copy_9patch(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Layer.copy9Patch requires 1 argument");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_copy_9patch`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.copy9Patch: this layer is not constructed");
    };
    let Ok((host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.copy9Patch: compat context is not bound");
    };
    let (src_id, _) = match resolve_source_bitmap(&ctx, host.engine(), &a[0], "copy9Patch", false) {
        Ok(v) => v,
        Err(error) => return report_error(out_error, &error),
    };
    let scene = match scene_mut(&ctx) {
        Ok(s) => s,
        Err(error) => return report_error(out_error, &error),
    };
    let dst = match main_bitmap(scene, id, "copy9Patch") {
        Ok(d) => d,
        Err(error) => return report_error(out_error, &error),
    };
    let result = with_two_bitmaps(scene, dst, src_id, layer_ops::copy_9patch);
    match result {
        Ok(margins) => {
            util::set_rect_ltrb_out(host.engine(), out, margins);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

// ---------------------------------------------------------------------------
// `colorRect`
// ---------------------------------------------------------------------------

/// `colorRect(x, y, w, h, color [, opa])`.
extern "C" fn layer_color_rect(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 5 {
        return report_error(out_error, "Layer.colorRect requires 5 arguments");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_color_rect`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.colorRect: this layer is not constructed");
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.colorRect: compat context is not bound");
    };
    let result: Result<(), String> = (|| {
        let x = value_as_i64(&a[0]) as i32;
        let y = value_as_i64(&a[1]) as i32;
        let w = value_as_i64(&a[2]) as i32;
        let h = value_as_i64(&a[3]) as i32;
        let script_color = value_as_i64(&a[4]);
        let color = argb_to_rgba(script_color);
        // The faces that paint the colour take it through the system-colour
        // table first, so an identifier names its table entry and the red and
        // blue bytes are swapped. The resolved colour names no alpha.
        let rgb = actual_color_bytes(script_color);
        let resolved_color = [rgb[0], rgb[1], rgb[2], 0];
        let opa = opt_i64(a, 5, 255).clamp(-255, 255) as i32;
        let scene = scene_mut(&ctx)?;
        let Some((dst, _hold_alpha)) = dest_info(scene, id, "colorRect")? else {
            return Ok(());
        };
        let face = draw_face(scene, id);
        let clip = layer_clip(scene, id);
        let Some(dest) =
            layer_ops::intersect_rect((x, y, x.saturating_add(w), y.saturating_add(h)), clip)
        else {
            return Ok(());
        };
        if face == DF_PROVINCE {
            let dest = {
                let bitmap = scene
                    .bitmap(dst)
                    .ok_or_else(|| "Layer.colorRect: this layer has no main image".to_string())?;
                layer_ops::intersect(bitmap, dest)
            };
            if let Some(dest) = dest {
                fill_province_region(scene, id, dest, color[2]);
            }
            return Ok(());
        }
        let bitmap = scene
            .bitmap_mut(dst)
            .ok_or_else(|| "Layer.colorRect: this layer has no main image".to_string())?;
        let Some(dest) = layer_ops::intersect(bitmap, dest) else {
            return Ok(());
        };
        let modified = match face {
            DF_ALPHA => {
                if opa > 0 {
                    layer_ops::fill_color_on_alpha(bitmap, dest, resolved_color, opa);
                } else {
                    layer_ops::remove_const_opacity(bitmap, dest, -opa);
                }
                opa != 0
            }
            DF_ADD_ALPHA => {
                if opa < 0 {
                    return Err(
                        "Layer.colorRect: Negative opacity not supported on this face".into(),
                    );
                }
                layer_ops::fill_color_on_add_alpha(bitmap, dest, resolved_color, opa);
                opa > 0
            }
            DF_OPAQUE => {
                layer_ops::fill_color_hold_alpha(bitmap, dest, resolved_color, opa);
                opa > 0
            }
            DF_MASK => {
                layer_ops::fill_mask(bitmap, dest, color[2]);
                true
            }
            // Reference `ColorRect` switches on the resolved face with no
            // `default:` (`reference/cpp/core/visual/LayerIntf.cpp:4338-4411`):
            // an out-of-vocabulary face falls through silently.
            _ => false,
        };
        if modified {
            mark_image_modified(scene, id);
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

pub(super) fn fill_province_region(scene: &mut Scene, id: u32, dest: RectI, value: u8) {
    let Some(layer) = scene.layer_mut(id) else {
        return;
    };
    if layer.province.is_none() {
        scene.allocate_province_image(id);
    }
    let Some(layer) = scene.layer_mut(id) else {
        return;
    };
    let (Some(province), pw, ph) = (
        layer.province.as_deref_mut(),
        layer.province_width,
        layer.province_height,
    ) else {
        return;
    };
    for y in dest.1.max(0)..dest.3.min(ph as i32) {
        for x in dest.0.max(0)..dest.2.min(pw as i32) {
            province[y as usize * pw as usize + x as usize] = value;
        }
    }
    layer.image_modified = true;
}

// ---------------------------------------------------------------------------
// `convertType` / flips / blur / gamma / gray / light
// ---------------------------------------------------------------------------

/// `convertType(fromtype)`.
extern "C" fn layer_convert_type(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Layer.convertType requires 1 argument");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_convert_type`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.convertType: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.convertType: compat context is not bound");
    };
    let result: Result<(), String> = (|| {
        let fromtype = value_as_i64(&a[0]) as i32;
        let scene = scene_mut(&ctx)?;
        let Some((dst, _)) = dest_info(scene, id, "convertType")? else {
            return Ok(());
        };
        let face = draw_face(scene, id);
        let clip = layer_clip(scene, id);
        let bitmap = scene
            .bitmap_mut(dst)
            .ok_or_else(|| "Layer.convertType: this layer has no main image".to_string())?;
        if face == DF_ADD_ALPHA && fromtype == DF_ALPHA {
            layer_ops::convert_alpha_to_add_alpha(bitmap, clip);
        } else if face == DF_ALPHA && fromtype == DF_ADD_ALPHA {
            layer_ops::convert_add_alpha_to_alpha(bitmap, clip);
        } else {
            return Err(
                "Layer.convertType: cannot convert the layer type in the given direction".into(),
            );
        }
        if let Some(layer) = scene.layer_mut(id) {
            layer.image_modified = true;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `flipLR()`.
extern "C" fn layer_flip_lr(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    flip_axis(engine, instance, out, out_error, true)
}

/// `flipUD()`.
extern "C" fn layer_flip_ud(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    flip_axis(engine, instance, out, out_error, false)
}

fn flip_axis(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    horizontal: bool,
) -> c_int {
    let method = if horizontal { "flipLR" } else { "flipUD" };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `flip_axis`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            &format!("Layer.{method}: this layer is not constructed"),
        );
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(
            out_error,
            &format!("Layer.{method}: compat context is not bound"),
        );
    };
    let result: Result<(), String> = (|| {
        let scene = scene_mut(&ctx)?;
        let Some((dst, _)) = dest_info(scene, id, method)? else {
            return Ok(());
        };
        let bitmap = scene
            .bitmap_mut(dst)
            .ok_or_else(|| format!("Layer.{method}: this layer has no main image"))?;
        if horizontal {
            layer_ops::flip_lr(bitmap);
        } else {
            layer_ops::flip_ud(bitmap);
        }
        flip_province(scene, id, horizontal);
        if let Some(layer) = scene.layer_mut(id) {
            layer.image_modified = true;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

fn flip_province(scene: &mut Scene, id: u32, horizontal: bool) {
    let Some(layer) = scene.layer_mut(id) else {
        return;
    };
    let (Some(province), pw, ph) = (
        layer.province.as_deref_mut(),
        layer.province_width as usize,
        layer.province_height as usize,
    ) else {
        return;
    };
    if horizontal {
        for y in 0..ph {
            for x in 0..pw / 2 {
                province.swap(y * pw + x, y * pw + (pw - 1 - x));
            }
        }
    } else {
        for y in 0..ph / 2 {
            for x in 0..pw {
                province.swap(y * pw + x, (ph - 1 - y) * pw + x);
            }
        }
    }
}

/// `doBoxBlur([xblur [, yblur]])`.
extern "C" fn layer_do_box_blur(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_do_box_blur`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.doBoxBlur: this layer is not constructed");
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.doBoxBlur: compat context is not bound");
    };
    let result: Result<(), String> = (|| {
        let xblur = opt_i64(a, 0, 1) as i32;
        let yblur = opt_i64(a, 1, 1) as i32;
        if xblur == 0 && yblur == 0 {
            return Ok(());
        }
        let scene = scene_mut(&ctx)?;
        let Some((dst, _)) = dest_info(scene, id, "doBoxBlur")? else {
            return Ok(());
        };
        let face = draw_face(scene, id);
        let clip = layer_clip(scene, id);
        let bitmap = scene
            .bitmap_mut(dst)
            .ok_or_else(|| "Layer.doBoxBlur: this layer has no main image".to_string())?;
        let Some(region) = layer_ops::intersect(bitmap, clip) else {
            return Ok(());
        };
        let mut scratch = extract_region(bitmap, region);
        if face != DF_ALPHA {
            // `DoBoxBlur` blurs RGB only; hold the alpha plane.
            restore_region_alpha(bitmap, &mut scratch, region);
        }
        layer_ops::blur_bitmap_in_place(&mut scratch, xblur.unsigned_abs(), yblur.unsigned_abs());
        if face == DF_ALPHA {
            // `DoBoxBlurForAlpha` blurs alpha only; hold the RGB planes.
            restore_region_rgb(bitmap, &mut scratch, region);
        }
        let width = (region.2 - region.0) as u32;
        let height = (region.3 - region.1) as u32;
        layer_ops::blit_copy(
            bitmap,
            region.0,
            region.1,
            &scratch,
            (0, 0, width as i32, height as i32),
        );
        bitmap.mark_dirty();
        if let Some(layer) = scene.layer_mut(id) {
            layer.image_modified = true;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `adjustGamma(rgamma, rfloor, rceil, ggamma, gfloor, gceil, bgamma,
/// bfloor, bceil)`.
extern "C" fn layer_adjust_gamma(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_adjust_gamma`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.adjustGamma: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.adjustGamma: compat context is not bound");
    };
    let result: Result<(), String> = (|| {
        if argc == 0 {
            return Ok(());
        }
        let data = GammaAdjust {
            r_gamma: opt_f64(a, 0, 1.0),
            r_floor: opt_i64(a, 1, 0) as i32,
            r_ceil: opt_i64(a, 2, 255) as i32,
            g_gamma: opt_f64(a, 3, 1.0),
            g_floor: opt_i64(a, 4, 0) as i32,
            g_ceil: opt_i64(a, 5, 255) as i32,
            b_gamma: opt_f64(a, 6, 1.0),
            b_floor: opt_i64(a, 7, 0) as i32,
            b_ceil: opt_i64(a, 8, 255) as i32,
        };
        let scene = scene_mut(&ctx)?;
        let Some((dst, _)) = dest_info(scene, id, "adjustGamma")? else {
            return Ok(());
        };
        let face = draw_face(scene, id);
        let clip = layer_clip(scene, id);
        let bitmap = scene
            .bitmap_mut(dst)
            .ok_or_else(|| "Layer.adjustGamma: this layer has no main image".to_string())?;
        if face == DF_ADD_ALPHA {
            layer_ops::convert_add_alpha_to_alpha(bitmap, clip);
            layer_ops::adjust_gamma(bitmap, clip, data);
            layer_ops::convert_alpha_to_add_alpha(bitmap, clip);
        } else {
            layer_ops::adjust_gamma(bitmap, clip, data);
        }
        if let Some(layer) = scene.layer_mut(id) {
            layer.image_modified = true;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `gaussianBlur(radius [, sigma])`.
extern "C" fn layer_gaussian_blur(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Layer.gaussianBlur requires a radius");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_gaussian_blur`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.gaussianBlur: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.gaussianBlur: compat context is not bound");
    };
    let result: Result<(), String> = (|| {
        let radius = opt_i64(a, 0, 1) as i32;
        if radius <= 0 {
            return Err("Layer.gaussianBlur: radius must be positive".into());
        }
        let sigma = match a.get(1) {
            Some(v) if v.ty != tjs::VAL_VOID => value_as_f64(v) as f32,
            // Reference: a *present* but `void` second argument keeps the
            // `1.0f` default; only a *missing* argument derives
            // `max(1.0f, radius / 2.5f)`
            // (`reference/cpp/core/visual/LayerIntf.cpp:9584-9597`).
            Some(_) => 1.0,
            None => (radius as f32 / 2.5).max(1.0),
        };
        if sigma <= 0.0 {
            return Err("Layer.gaussianBlur: sigma must be positive".into());
        }
        let scene = scene_mut(&ctx)?;
        let has_image = scene
            .layer(id)
            .ok_or_else(|| "Layer.gaussianBlur: this layer no longer exists".to_string())?
            .bitmap
            .is_some();
        if !has_image {
            // Reference `ApplyGaussianBlur` logs and returns without an
            // error when the layer cannot have or does not have an image
            // (`reference/cpp/core/visual/LayerIntf.cpp:5749-5755`).
            return Ok(());
        }
        let Some((dst, _)) = dest_info(scene, id, "gaussianBlur")? else {
            return Ok(());
        };
        let clip = layer_clip(scene, id);
        let bitmap = scene
            .bitmap_mut(dst)
            .ok_or_else(|| "Layer.gaussianBlur: this layer has no main image".to_string())?;
        layer_ops::gaussian_blur(bitmap, clip, radius, sigma);
        if let Some(layer) = scene.layer_mut(id) {
            layer.image_modified = true;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `doGrayScale()`.
extern "C" fn layer_do_gray_scale(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_do_gray_scale`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.doGrayScale: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.doGrayScale: compat context is not bound");
    };
    let result: Result<(), String> = (|| {
        let scene = scene_mut(&ctx)?;
        let Some((dst, _)) = dest_info(scene, id, "doGrayScale")? else {
            return Ok(());
        };
        let clip = layer_clip(scene, id);
        let bitmap = scene
            .bitmap_mut(dst)
            .ok_or_else(|| "Layer.doGrayScale: this layer has no main image".to_string())?;
        layer_ops::do_gray_scale(bitmap, clip);
        if let Some(layer) = scene.layer_mut(id) {
            layer.image_modified = true;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `light(brightness, contrast)`.
extern "C" fn layer_light(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Layer.light requires 2 arguments");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_light`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.light: this layer is not constructed");
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.light: compat context is not bound");
    };
    let result: Result<(), String> = (|| {
        let brightness = opt_i64(a, 0, 0) as i32;
        let contrast = opt_i64(a, 1, 0) as i32;
        let scene = scene_mut(&ctx)?;
        let has_image = scene
            .layer(id)
            .ok_or_else(|| "Layer.light: this layer no longer exists".to_string())?
            .bitmap
            .is_some();
        if !has_image {
            // Reference `ApplyLightContrast` logs and returns without an
            // error when the layer cannot have or does not have an image
            // (`reference/cpp/core/visual/LayerIntf.cpp:1006-1025`).
            return Ok(());
        }
        let Some((dst, _)) = dest_info(scene, id, "light")? else {
            return Ok(());
        };
        let clip = layer_clip(scene, id);
        let bitmap = scene
            .bitmap_mut(dst)
            .ok_or_else(|| "Layer.light: this layer has no main image".to_string())?;
        layer_ops::light_contrast(bitmap, clip, brightness, contrast);
        if let Some(layer) = scene.layer_mut(id) {
            layer.image_modified = true;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

// ---------------------------------------------------------------------------
// Pixel accessors / province / bitmap copy / dump
// ---------------------------------------------------------------------------

/// `getMainPixel(x, y)` → the pixel's packed colour word: the red in the low
/// byte, the green in the second, the blue in the third.
extern "C" fn layer_get_main_pixel(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Layer.getMainPixel requires 2 arguments");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_get_main_pixel`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.getMainPixel: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.getMainPixel: compat context is not bound");
    };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let scene = match super::super::common::scene_mut(&ctx) {
        Ok(s) => s,
        Err(error) => return report_error(out_error, &error),
    };
    let value = main_bitmap(scene, id, "getMainPixel")
        .ok()
        .and_then(|dst| scene.bitmap(dst))
        .and_then(|b| read_pixel(b, x, y))
        .map_or(0, |px| rgba_to_pixel_word(px) & 0x00ff_ffff);
    set_int(out, value);
    0
}

/// `setMainPixel(x, y, color)` (the red, green and blue planes written, the
/// mask plane held).
extern "C" fn layer_set_main_pixel(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 3 {
        return report_error(out_error, "Layer.setMainPixel requires 3 arguments");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_set_main_pixel`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.setMainPixel: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.setMainPixel: compat context is not bound");
    };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let color = actual_color_bytes(value_as_i64(&a[2]));
    let scene = match scene_mut(&ctx) {
        Ok(s) => s,
        Err(error) => return report_error(out_error, &error),
    };
    let dst = match main_bitmap(scene, id, "setMainPixel") {
        Ok(d) => d,
        Err(error) => return report_error(out_error, &error),
    };
    let clip = layer_clip(scene, id);
    if x < clip.0 || y < clip.1 || x >= clip.2 || y >= clip.3 {
        set_void(out);
        return 0;
    }
    if let Some(bitmap) = scene.bitmap_mut(dst)
        && let Some(offset) = bitmap.pixel_offset(x as u32, y as u32)
        && let Some(pixel) = bitmap.rgba.get_mut(offset..offset + 3)
    {
        pixel.copy_from_slice(&color);
        bitmap.mark_dirty();
    }
    if let Some(layer) = scene.layer_mut(id) {
        layer.image_modified = true;
    }
    set_void(out);
    0
}

/// `getMaskPixel(x, y)` → alpha.
extern "C" fn layer_get_mask_pixel(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Layer.getMaskPixel requires 2 arguments");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_get_mask_pixel`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.getMaskPixel: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.getMaskPixel: compat context is not bound");
    };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let scene = match super::super::common::scene_mut(&ctx) {
        Ok(s) => s,
        Err(error) => return report_error(out_error, &error),
    };
    let value = main_bitmap(scene, id, "getMaskPixel")
        .ok()
        .and_then(|dst| scene.bitmap(dst))
        .and_then(|b| read_pixel(b, x, y))
        .map_or(0, |px| i64::from(px[3]));
    set_int(out, value);
    0
}

/// `setMaskPixel(x, y, alpha)`.
extern "C" fn layer_set_mask_pixel(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 3 {
        return report_error(out_error, "Layer.setMaskPixel requires 3 arguments");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_set_mask_pixel`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.setMaskPixel: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.setMaskPixel: compat context is not bound");
    };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    // Reference `SetPointMask` stores `(mask & 0xff)`
    // (`reference/cpp/core/visual/LayerBitmapIntf.cpp:210-220`), so the
    // script value is truncated to its low eight bits, not clamped.
    let mask = value_as_i64(&a[2]) as u8;
    let scene = match scene_mut(&ctx) {
        Ok(s) => s,
        Err(error) => return report_error(out_error, &error),
    };
    let dst = match main_bitmap(scene, id, "setMaskPixel") {
        Ok(d) => d,
        Err(error) => return report_error(out_error, &error),
    };
    let clip = layer_clip(scene, id);
    if x < clip.0 || y < clip.1 || x >= clip.2 || y >= clip.3 {
        set_void(out);
        return 0;
    }
    if let Some(bitmap) = scene.bitmap_mut(dst)
        && let Some(offset) = bitmap.pixel_offset(x as u32, y as u32)
        && let Some(alpha) = bitmap.rgba.get_mut(offset + 3)
    {
        *alpha = mask;
        bitmap.mark_dirty();
    }
    if let Some(layer) = scene.layer_mut(id) {
        layer.image_modified = true;
    }
    set_void(out);
    0
}

/// Read one bitmap pixel as straight-alpha RGBA, or `None` when the image
/// holds no pixel there.
fn read_pixel(bitmap: &BitmapState, x: i32, y: i32) -> Option<[u8; 4]> {
    let offset = bitmap.pixel_offset(u32::try_from(x).ok()?, u32::try_from(y).ok()?)?;
    let pixel = bitmap.rgba.get(offset..offset + 4)?;
    Some([pixel[0], pixel[1], pixel[2], pixel[3]])
}

/// `independProvinceImage([copy])`.
extern "C" fn layer_independ_province_image(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let copy = if argc >= 1 && a[0].ty != tjs::VAL_VOID {
        crate::natives::system::value_as_bool(&a[0])
    } else {
        true
    };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_independ_province_image`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.independProvinceImage: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(
            out_error,
            "Layer.independProvinceImage: compat context is not bound",
        );
    };
    let result: Result<(), String> = (|| {
        let scene = scene_mut(&ctx)?;
        if !copy {
            scene.deallocate_province_image(id);
        }
        scene.allocate_province_image(id);
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `copyToBitmapFromMainImage(bitmap)`.
extern "C" fn layer_copy_to_bitmap_from_main_image(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(
            out_error,
            "Layer.copyToBitmapFromMainImage requires a Bitmap",
        );
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_copy_to_bitmap_from_main_image`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.copyToBitmapFromMainImage: this layer is not constructed",
        );
    };
    let Ok((host, ctx)) = super::bound(engine) else {
        return report_error(
            out_error,
            "Layer.copyToBitmapFromMainImage: compat context is not bound",
        );
    };
    let result: Result<(), String> = (|| {
        let dst_id =
            match super::super::common::resolve_object_id(host.engine(), &a[0], &["Bitmap"]) {
                Ok(id) if id >= 0 => id as u32,
                _ => {
                    return Err(
                        "Layer.copyToBitmapFromMainImage: the argument is not a Bitmap".into(),
                    );
                }
            };
        let scene = scene_mut(&ctx)?;
        let src_id = main_bitmap(scene, id, "copyToBitmapFromMainImage")?;
        // Both images may still be decoding: the source is copied and the
        // destination replaced from the resolved pixels.
        let (width, height, rgba) =
            super::resolved_bitmap_pixels(scene, src_id, "Layer.copyToBitmapFromMainImage")?;
        if scene.bitmap(dst_id).is_none() {
            return Err(format!(
                "Layer.copyToBitmapFromMainImage: the destination bitmap {dst_id} does not exist"
            ));
        }
        // The copy replaces the destination's pixels: a decode the destination
        // still carries is cancelled, so the copied pixels are not overwritten
        // by the stale image.
        if !scene.replace_bitmap_image(dst_id, width, height, rgba) {
            return Err(format!(
                "Layer.copyToBitmapFromMainImage: the destination bitmap {dst_id} could not be \
                 replaced"
            ));
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `dump()` — log the layer structure (the reference `DumpStructure`).
extern "C" fn layer_dump(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_dump`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.dump: this layer is not constructed");
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.dump: compat context is not bound");
    };
    let scene = match super::super::common::scene_ref(&ctx) {
        Ok(s) => s,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some(layer) = scene.layer(id) {
        log::debug!(
            "Layer.dump: id={} window={} parent={:?} rect=({},{},{}x{}) visible={} \
             enabled={} focusable={} name={:?} bitmap={:?} children={:?}",
            layer.id,
            layer.window,
            layer.parent,
            layer.rect.x,
            layer.rect.y,
            layer.rect.w,
            layer.rect.h,
            layer.visible,
            layer.enabled,
            layer.focusable,
            layer.name,
            layer.bitmap,
            layer.children,
        );
    }
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// Deferred members (typed errors, never silent stubs)
// ---------------------------------------------------------------------------

/// `loadImages(name [, key])` — load the layer's main image (reference
/// `tTJSNI_BaseLayer::LoadImages`). The bytes are read through the attached
/// storage (probing image extensions), decoded by the host image codec, and
/// installed as the layer's bitmap with the image size set. Returns void:
/// the codec reports no per-format metainfo, so the reference's optional
/// metainfo dictionary is absent.
extern "C" fn layer_load_images(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Layer.loadImages requires 1 argument");
    }
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_load_images`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.loadImages: this layer is not constructed");
    };
    let Ok((host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.loadImages: compat context is not bound");
    };
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let name = crate::native_util::value_as_string(&a[0]);
    let colorkey = a
        .get(1)
        .filter(|value| value.ty != tjs::VAL_VOID)
        .map_or(COLOR_KEY_NONE, |value| value_as_i64(value) as u32);
    match load_layer_image(&ctx, id, &name, colorkey) {
        Ok(()) => {
            // The reference returns the graphic metainfo, which is `null`
            // when the format carries none (the common case). Returning
            // `void` would make the game's `ret != null` guard pass and then
            // fault on `ret.mode`.
            set_null_out(host.engine(), out);
            0
        }
        Err(error) => report_error(out_error, &error.message_with("Layer.loadImages: ")),
    }
}

/// Reads, decodes and installs `name` as the layer's main image, reusing the
/// layer's existing bitmap so repeated loads do not leak scene bitmaps.
/// Installing the image is followed by the image-size adjustment, which shrinks
/// the layer rectangle to a smaller image, clamps the image offsets and resets
/// the clip.
fn load_layer_image(
    ctx: &CompatCtx,
    layer_id: u32,
    name: &str,
    colorkey: u32,
) -> Result<(), ImageLoadError> {
    log::debug!("load_layer_image: layer {layer_id} <- {name:?}");
    // The decoded-image cache comes first: a hit is installed synchronously,
    // exactly as a cached graphic is installed.
    let key = DecodedImageKey::new(name, colorkey, DecodedImageMode::Full, 0, 0);
    let cached = {
        let scene = scene_mut(ctx)?;
        scene.lookup_decoded_image(&key)
    };
    if let Some(image) = cached {
        return install_layer_image(ctx, layer_id, image.width, image.height, image.rgba)
            .map_err(ImageLoadError::Message);
    }
    // The pool takes the decode when it can: the layer's image size is
    // known now, from the header probe, and its pixels land later.
    if let Some((width, height, pending)) =
        start_pending_decode(ctx, name, colorkey, DecodedImageMode::Full)?
    {
        let existing = {
            let scene = scene_mut(ctx)?;
            scene.layer(layer_id).and_then(|layer| layer.bitmap)
        };
        let id = install_pending_image(ctx, existing, width, height, pending)?;
        let scene = scene_mut(ctx)?;
        if let Some(layer) = scene.layer_mut(layer_id) {
            layer.bitmap = Some(id);
        }
        if !scene.internal_set_image_size(layer_id, width, height) {
            return Err(ImageLoadError::Message(
                "the layer image could not be sized".to_string(),
            ));
        }
        return Ok(());
    }
    // The synchronous path: the decode runs here and the cache keeps it.
    let image = load_cached_storage_image(ctx, name, colorkey, DecodedImageMode::Full)?;
    install_layer_image(ctx, layer_id, image.width, image.height, image.rgba)
        .map_err(ImageLoadError::Message)
}

/// Installs a decoded image as the layer's main image.
///
/// The image size follows the decoded one, and the layer's clip is reset to
/// it, exactly as a load does.
fn install_layer_image(
    ctx: &CompatCtx,
    layer_id: u32,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
) -> Result<(), String> {
    let scene = scene_mut(ctx)?;
    if scene.layer(layer_id).is_none() {
        return Err("the layer no longer exists".to_string());
    }
    match scene.layer(layer_id).and_then(|layer| layer.bitmap) {
        Some(id) => {
            scene.replace_bitmap_image(id, width, height, rgba);
        }
        None => {
            let id = scene.add_bitmap(width, height, rgba);
            if let Some(layer) = scene.layer_mut(layer_id) {
                layer.bitmap = Some(id);
            }
        }
    }
    if !scene.internal_set_image_size(layer_id, width, height) {
        return Err("the layer image could not be sized".to_string());
    }
    Ok(())
}

/// `loadProvinceImage(name)` — load an 8-bit province (mask) plane matching
/// the main image's size (reference `tTJSNI_BaseLayer::LoadProvinceImage`).
extern "C" fn layer_load_province_image(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Layer.loadProvinceImage requires 1 argument");
    }
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_load_province_image`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.loadProvinceImage: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(
            out_error,
            "Layer.loadProvinceImage: compat context is not bound",
        );
    };
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let name = crate::native_util::value_as_string(&a[0]);
    match load_layer_province(&ctx, id, &name) {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &format!("Layer.loadProvinceImage: {error}")),
    }
}

/// Reads and installs `name` as the layer's province plane. The decoded image
/// must not be larger than the main image; a smaller image is tiled up to the
/// main size. The image is the 8-bit index plane — the palette index of a
/// palettized image, not the colour the palette maps it to — and its red
/// channel becomes the 8-bit province value.
fn load_layer_province(ctx: &CompatCtx, layer_id: u32, name: &str) -> Result<(), String> {
    let (main_w, main_h) = {
        let scene = scene_mut(ctx)?;
        let dimensions = scene
            .layer(layer_id)
            .and_then(|layer| layer.bitmap)
            .and_then(|id| scene.bitmap(id))
            .map(|bitmap| (bitmap.width, bitmap.height))
            .ok_or_else(|| "the layer has no main image".to_string())?;
        // The plane is allocated before the load and dropped when the load
        // fails.
        scene.resize_province_image(layer_id, dimensions.0, dimensions.1);
        dimensions
    };
    let image =
        match load_cached_storage_image(ctx, name, COLOR_KEY_NONE, DecodedImageMode::Province) {
            Ok(image) => image,
            Err(error) => {
                return Err(discard_province_plane(ctx, layer_id, error.message()));
            }
        };
    let plane = match province_plane(&image, main_w, main_h) {
        Ok(plane) => plane,
        Err(error) => return Err(discard_province_plane(ctx, layer_id, error)),
    };
    let scene = scene_mut(ctx)?;
    let sized = scene.layer(layer_id).is_some_and(|layer| {
        layer.province_width == main_w
            && layer.province_height == main_h
            && layer
                .province
                .as_deref()
                .is_some_and(|province| province.len() == plane.len())
    });
    if !sized {
        scene.deallocate_province_image(layer_id);
        return Err("the province plane could not be sized to the main image".to_string());
    }
    let Some(layer) = scene.layer_mut(layer_id) else {
        return Err("the layer has no main image".to_string());
    };
    let Some(province) = layer.province.as_mut() else {
        return Err("the province plane is missing".to_string());
    };
    province.copy_from_slice(&plane);
    layer.image_modified = true;
    Ok(())
}

/// Drops the layer's province plane after a failed load and returns `error`
/// for the caller to raise. A plane that cannot be dropped is reported; it
/// does not replace the load's failure.
fn discard_province_plane(ctx: &CompatCtx, layer_id: u32, error: String) -> String {
    match scene_mut(ctx) {
        Ok(scene) => scene.deallocate_province_image(layer_id),
        Err(drop_error) => log::error!(
            "loadProvinceImage: the province plane of layer {layer_id} could not be dropped: {drop_error}"
        ),
    }
    error
}

/// Extracts the plane values of `image` into a province plane of `width` x
/// `height`, tiling a smaller image to the destination size.
///
/// `image` is the decoded 8-bit index plane, carried as a gray image: its
/// red channel is the plane value.
fn province_plane(image: &DecodedImage, width: u32, height: u32) -> Result<Vec<u8>, String> {
    if image.width > width || image.height > height {
        return Err(format!(
            "the province image is {}x{} but the main image is {width}x{height}",
            image.width, image.height,
        ));
    }
    if image.width == 0 || image.height == 0 {
        return Err("the province image has an empty dimension".to_string());
    }
    if !image.is_consistent() {
        return Err("the decoded image is malformed".to_string());
    }
    let image_width = usize::try_from(image.width)
        .map_err(|_| "the province image width is not representable".to_string())?;
    let image_height = usize::try_from(image.height)
        .map_err(|_| "the province image height is not representable".to_string())?;
    let width = usize::try_from(width)
        .map_err(|_| "the main image width is not representable".to_string())?;
    let height = usize::try_from(height)
        .map_err(|_| "the main image height is not representable".to_string())?;
    let byte_count = width
        .checked_mul(height)
        .ok_or_else(|| "the province plane size is not representable".to_string())?;
    let mut plane = Vec::new();
    plane
        .try_reserve_exact(byte_count)
        .map_err(|_| "the province plane is too large to allocate".to_string())?;
    plane.resize(byte_count, 0);
    for y in 0..height {
        let source_y = y % image_height;
        for x in 0..width {
            let source_x = x % image_width;
            let source = (source_y * image_width + source_x) * 4;
            let Some(value) = image.rgba.get(source).copied() else {
                return Err("the province image is malformed".to_string());
            };
            plane[y * width + x] = value;
        }
    }
    Ok(plane)
}

/// `saveLayerImage(name [, type])` — encode the layer's main image through
/// the host image codec and write it through the attached storage.
extern "C" fn layer_save_layer_image(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Layer.saveLayerImage requires a name");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let name = crate::native_util::value_as_string(&a[0]);
    let type_name = match a.get(1) {
        Some(value) if value.ty != tjs::VAL_VOID => {
            let text = crate::native_util::value_as_string(value);
            if text.is_empty() {
                "bmp".to_string()
            } else {
                text
            }
        }
        _ => "bmp".to_string(),
    };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_save_layer_image`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.saveLayerImage: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(
            out_error,
            "Layer.saveLayerImage: compat context is not bound",
        );
    };
    let result: Result<(), String> = (|| {
        let scene = scene_mut(&ctx)?;
        let bitmap_id = scene
            .layer(id)
            .and_then(|layer| layer.bitmap)
            .ok_or_else(|| {
                "Layer.saveLayerImage: unsupported layer type (no main image)".to_string()
            })?;
        // The image may still be decoding: the saved pixels are the decoded
        // ones, and a bitmap that holds none is a raise rather than an empty
        // file.
        let (width, height, rgba) =
            super::resolved_bitmap_pixels(scene, bitmap_id, "Layer.saveLayerImage")?;
        resource_state(&ctx)
            .save_image(&name, &type_name, width, height, &rgba)
            .map_err(|error| format!("Layer.saveLayerImage: {error}"))
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `drawText(...)` — draw a text run into the layer's main image.
extern "C" fn layer_draw_text(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_draw_text_common(
        engine,
        instance,
        argc,
        argv,
        out,
        out_error,
        TextKind {
            method: "drawText",
            glyph: false,
        },
    )
}

/// `drawGlyph(...)` — draw the glyph's text into the layer's main image.
extern "C" fn layer_draw_glyph(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_draw_text_common(
        engine,
        instance,
        argc,
        argv,
        out,
        out_error,
        TextKind {
            method: "drawGlyph",
            glyph: true,
        },
    )
}

struct TextKind<'a> {
    method: &'a str,
    glyph: bool,
}

/// `getTextWidth(text)` — the layer font's unrotated text width.
///
/// A mapped pre-rendered font contributes each character's `Inc`; otherwise
/// the width is the sum of the rasterizer advances.
extern "C" fn layer_get_text_width(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_text_metric(engine, instance, argc, argv, out, out_error, true)
}

/// `getTextHeight(text)` — the absolute height of the layer's current font.
extern "C" fn layer_get_text_height(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_text_metric(engine, instance, argc, argv, out, out_error, false)
}

/// Shared body of `getTextWidth`/`getTextHeight`: copy the layer font, then
/// measure outside the scene borrow.
fn layer_text_metric(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    width: bool,
) -> c_int {
    let method = if width {
        "getTextWidth"
    } else {
        "getTextHeight"
    };
    if argc < 1 {
        return report_error(
            out_error,
            &format!("Layer.{method} requires a text argument"),
        );
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let text = crate::native_util::value_as_string(&unsafe { args(argv, argc) }[0]);
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_text_metric`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            &format!("Layer.{method}: this layer is not constructed"),
        );
    };
    let result: Result<i64, String> = (|| {
        let (face, height, bold, italic, angle) = {
            let (_resolved, ctx) =
                bound(engine).map_err(|error| format!("Layer.{method}: {error}"))?;
            let scene = scene_ref(&ctx).map_err(|error| format!("Layer.{method}: {error}"))?;
            let layer = scene
                .layer(id)
                .ok_or_else(|| format!("Layer.{method}: this layer no longer exists"))?;
            match layer.font_id.and_then(|font_id| scene.font(font_id)) {
                Some(font) => (
                    font.face.clone(),
                    font.height,
                    font.bold,
                    font.italic,
                    font.angle,
                ),
                None => (
                    DEFAULT_FONT_FACE.to_string(),
                    DEFAULT_FONT_HEIGHT,
                    false,
                    false,
                    0.0,
                ),
            }
        };
        let font = super::super::font::MeasuredFont {
            size_px: height.unsigned_abs() as f32,
            height: height.unsigned_abs() as i32,
            face,
            bold,
            italic,
            angle,
        };
        let (width_px, height_px) = super::super::font::measure_layer_text(
            engine,
            &font,
            &text,
            &format!("Layer.{method}"),
        )?;
        Ok(i64::from(if width { width_px } else { height_px }))
    })();
    match result {
        Ok(value) => {
            set_int(out, value);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// Shared body of `drawText`/`drawGlyph` (the glyph argument only differs in
/// how the text is extracted).
fn layer_draw_text_common(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    kind: TextKind<'_>,
) -> c_int {
    let TextKind { method, glyph } = kind;
    if argc < 4 {
        return report_error(out_error, &format!("Layer.{method} requires 4 arguments"));
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_draw_text_common`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            &format!("Layer.{method}: this layer is not constructed"),
        );
    };
    let Ok((host, ctx)) = super::bound(engine) else {
        return report_error(
            out_error,
            &format!("Layer.{method}: compat context is not bound"),
        );
    };
    let text = if glyph {
        match glyph_text(host.engine(), &a[2]) {
            Some(text) => text,
            None => {
                return report_error(
                    out_error,
                    &format!("Layer.{method}: unsupported glyph argument"),
                );
            }
        }
    } else {
        crate::native_util::value_as_string(&a[2])
    };
    let result: Result<(), String> = (|| {
        let x = value_as_i64(&a[0]) as i32;
        let y = value_as_i64(&a[1]) as i32;
        // The main colour goes through the system-colour table; the shadow
        // colour is taken as it is.
        let rgb = actual_color_bytes(value_as_i64(&a[3]));
        let mut color = [rgb[0], rgb[1], rgb[2], 255];
        let opacity = opt_i64(a, 4, 255) as i32;
        {
            // The text is drawn with the blend method the draw face selects,
            // and the faces that are not drawable raise. The add-alpha face
            // cannot take a negative opacity.
            let scene = scene_mut(&ctx)?;
            let text_face = draw_face(scene, id);
            if !matches!(text_face, DF_ALPHA | DF_ADD_ALPHA | DF_OPAQUE) {
                return Err(format!("Layer.{method}: unsupported draw face {text_face}"));
            }
            if text_face == DF_ADD_ALPHA && opacity < 0 {
                return Err(format!(
                    "Layer.{method}: Negative opacity not supported on this face"
                ));
            }
        }
        let opacity = opacity.clamp(0, 255);
        let antialias = opt_bool(a, 5, true);
        let shadow_level = opt_i64(a, 6, 0) as i32;
        let mut shadow_color = argb_to_rgba(opt_i64(a, 7, 0));
        shadow_color[3] = 255;
        if glyph {
            // The glyph path hands the colour to the bitmap as it is, so the
            // drawn colour names the red and the blue the other way round.
            color = swap_red_blue(color);
            shadow_color = swap_red_blue(shadow_color);
        }
        let shadow_width = opt_i64(a, 8, 0) as i32;
        let shadow_offset = (opt_i64(a, 9, 0) as i32, opt_i64(a, 10, 0) as i32);
        let params = TextDrawParams {
            text: &text,
            x,
            y,
            color,
            opacity,
            antialias,
            shadow_level,
            shadow_color,
            shadow_width,
            shadow_offset,
        };
        let (bitmap_id, face, size_px, bold, italic, angle, clip) = {
            let scene = scene_mut(&ctx)?;
            let bitmap_id = ensure_layer_bitmap(scene, id, method)?;
            let layer = scene
                .layer(id)
                .ok_or_else(|| format!("Layer.{method}: this layer no longer exists"))?;
            let font = layer.font_id.and_then(|font_id| scene.font(font_id));
            let face = font
                .map(|font| font.face.clone())
                .filter(|face| !face.is_empty())
                .unwrap_or_else(|| DEFAULT_FONT_FACE.to_string());
            let size_px = font
                .map(|font| font.height.unsigned_abs() as f32)
                .filter(|size| *size > 0.0)
                .unwrap_or(DEFAULT_FONT_HEIGHT as f32);
            let bold = font.is_some_and(|font| font.bold);
            let italic = font.is_some_and(|font| font.italic);
            let angle = font.map_or(0.0, |font| font.angle);
            let clip = layer_clip(scene, id);
            (bitmap_id, face, size_px, bold, italic, angle, clip)
        };
        let ink = {
            let mut resolver = resource_state(&ctx);
            let scene = scene_mut(&ctx)?;
            let bitmap = scene
                .bitmap_mut(bitmap_id)
                .ok_or_else(|| format!("Layer.{method}: this layer has no main image"))?;
            draw_text_into_bitmap(
                &mut resolver,
                &ctx,
                bitmap,
                crate::resource::FaceSpec {
                    face: &face,
                    size_px,
                    bold,
                    italic,
                    angle_tenths: angle,
                },
                clip,
                &params,
            )?
        };
        if ink.is_some() {
            let scene = scene_mut(&ctx)?;
            if let Some(layer) = scene.layer_mut(id) {
                layer.image_modified = true;
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// Ensure the layer has a main-image bitmap, allocating a transparent one
/// sized from the layer rectangle when it does not. Returns the bitmap id.
fn ensure_layer_bitmap(scene: &mut Scene, layer_id: u32, method: &str) -> Result<u32, String> {
    let Some(layer) = scene.layer(layer_id) else {
        return Err(format!("Layer.{method}: this layer no longer exists"));
    };
    let width = layer.rect.w;
    let height = layer.rect.h;
    let existing = layer.bitmap;
    if let Some(id) = existing {
        // The image may still be decoding: the text is drawn into the decoded
        // pixels, and an image that resolved without them is an error rather
        // than a silently dropped text.
        scene.resolve_bitmap(id);
        let bitmap = scene
            .bitmap(id)
            .ok_or_else(|| format!("Layer.{method}: the layer has no main image"))?;
        util::bitmap_holds_pixels(bitmap, "layer image")?;
        return Ok(id);
    }
    if width == 0 || height == 0 {
        return Err(format!(
            "Layer.{method}: unsupported layer type (no main image)"
        ));
    }
    if !scene.ensure_layer_image(layer_id, width, height) {
        return Err(format!("Layer.{method}: the layer rectangle is too large"));
    }
    scene
        .layer(layer_id)
        .and_then(|layer| layer.bitmap)
        .ok_or_else(|| format!("Layer.{method}: the layer has no main image"))
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Every group-1 member as a `NativeInstanceMethodDef`.
pub(super) fn method_defs() -> Vec<NativeInstanceMethodDef> {
    vec![
        d("operateAffine", layer_operate_affine),
        d("operateRect", layer_operate_rect),
        d("operateStretch", layer_operate_stretch),
        d("affineCopy", layer_affine_copy),
        d("affineBlend", layer_affine_blend),
        d("affinePile", layer_affine_pile),
        d("stretchCopy", layer_stretch_copy),
        d("stretchBlend", layer_stretch_blend),
        d("stretchPile", layer_stretch_pile),
        d("pileRect", layer_pile_rect),
        d("piledCopy", layer_piled_copy),
        d("blendRect", layer_blend_rect),
        d("colorRect", layer_color_rect),
        d("copyRect", layer_copy_rect),
        d("copy9Patch", layer_copy_9patch),
        d(
            "copyToBitmapFromMainImage",
            layer_copy_to_bitmap_from_main_image,
        ),
        d("convertType", layer_convert_type),
        d("flipLR", layer_flip_lr),
        d("flipUD", layer_flip_ud),
        d("doBoxBlur", layer_do_box_blur),
        d("gaussianBlur", layer_gaussian_blur),
        d("adjustGamma", layer_adjust_gamma),
        d("doGrayScale", layer_do_gray_scale),
        d("light", layer_light),
        d("independProvinceImage", layer_independ_province_image),
        d("loadImages", layer_load_images),
        d("loadProvinceImage", layer_load_province_image),
        d("saveLayerImage", layer_save_layer_image),
        d("getMainPixel", layer_get_main_pixel),
        d("setMainPixel", layer_set_main_pixel),
        d("getMaskPixel", layer_get_mask_pixel),
        d("setMaskPixel", layer_set_mask_pixel),
        d("dump", layer_dump),
        d("drawText", layer_draw_text),
        d("drawGlyph", layer_draw_glyph),
        d("getTextWidth", layer_get_text_width),
        d("getTextHeight", layer_get_text_height),
    ]
}

fn d(name: &'static str, f: tjs::NativeInstanceMethodFn) -> NativeInstanceMethodDef {
    NativeInstanceMethodDef { name, f }
}
