//! Decode equivalence for the layer paths.
//!
//! Each test builds a small PSD in memory, decodes it through the public API
//! and compares every pixel against an independently computed expectation, so
//! the vectorized merge, the scalar tail and the fork-join channel schedule are
//! all held to the same bytes.

use psd::decode::{self, LayerImage};
use psd::parse::{COLOR_MODE_GRAYSCALE, COLOR_MODE_RGB, ImageMode, parse};

/// Build a one-layer PSD with raw (uncompressed) channels.
///
/// `channels` are `(channel id, plane bytes)` in file order. A mask record is
/// written when `mask_rect` is present.
fn build_psd(
    mode: u16,
    width: i32,
    height: i32,
    channels: &[(i16, Vec<u8>)],
    mask_rect: Option<[i32; 4]>,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"8BPS");
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&[0u8; 6]);
    out.extend_from_slice(&(channels.len() as u16).to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&8u16.to_be_bytes());
    out.extend_from_slice(&mode.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // colour mode data
    out.extend_from_slice(&0u32.to_be_bytes()); // image resources

    let mut layer = Vec::new();
    layer.extend_from_slice(&1i16.to_be_bytes()); // one layer record
    layer.extend_from_slice(&0i32.to_be_bytes()); // top
    layer.extend_from_slice(&0i32.to_be_bytes()); // left
    layer.extend_from_slice(&height.to_be_bytes()); // bottom
    layer.extend_from_slice(&width.to_be_bytes()); // right
    layer.extend_from_slice(&(channels.len() as u16).to_be_bytes());
    for (id, data) in channels {
        layer.extend_from_slice(&id.to_be_bytes());
        layer.extend_from_slice(&((data.len() + 2) as i32).to_be_bytes());
    }
    layer.extend_from_slice(b"8BIM");
    layer.extend_from_slice(b"norm");
    layer.push(255); // opacity
    layer.push(0); // clipping
    layer.push(0); // flags: visible
    layer.push(0); // filler
    let extra = layer_extra(mask_rect);
    layer.extend_from_slice(&(extra.len() as u32).to_be_bytes());
    layer.extend_from_slice(&extra);
    for (_, data) in channels {
        layer.extend_from_slice(&0u16.to_be_bytes()); // raw compression
        layer.extend_from_slice(data);
    }

    let mut section = Vec::new();
    section.extend_from_slice(&(layer.len() as u32).to_be_bytes());
    section.extend_from_slice(&layer);
    section.extend_from_slice(&0u32.to_be_bytes()); // global layer mask info
    out.extend_from_slice(&(section.len() as u32).to_be_bytes());
    out.extend_from_slice(&section);
    out
}

/// The layer extra block: an optional mask record, no blending ranges and a
/// unicode name.
fn layer_extra(mask_rect: Option<[i32; 4]>) -> Vec<u8> {
    let mut extra = Vec::new();
    match mask_rect {
        Some([top, left, bottom, right]) => {
            extra.extend_from_slice(&20i32.to_be_bytes());
            extra.extend_from_slice(&top.to_be_bytes());
            extra.extend_from_slice(&left.to_be_bytes());
            extra.extend_from_slice(&bottom.to_be_bytes());
            extra.extend_from_slice(&right.to_be_bytes());
            extra.push(0); // default colour
            extra.push(0); // flags
            extra.extend_from_slice(&[0u8; 2]); // record padding
        }
        None => extra.extend_from_slice(&0i32.to_be_bytes()),
    }
    extra.extend_from_slice(&0i32.to_be_bytes()); // blending ranges
    extra.push(0); // legacy name length
    extra.extend_from_slice(&[0u8; 3]); // name padding
    extra.extend_from_slice(b"8BIM");
    extra.extend_from_slice(b"luni");
    extra.extend_from_slice(&0u32.to_be_bytes());
    extra
}

/// Decode the only layer of `psd` and return its image.
fn decode_layer(psd: &[u8], mode: ImageMode) -> LayerImage {
    let file = parse(psd).expect("the built PSD parses");
    let layer = file.layers.first().expect("the built PSD has one layer");
    decode::layer_image(&file, layer, mode)
        .expect("the layer decodes")
        .expect("the layer has an image")
}

/// Plane bytes with a distinct pattern per channel index.
fn plane(index: u8, pixels: usize) -> Vec<u8> {
    (0..pixels)
        .map(|pixel| {
            (pixel as u8)
                .wrapping_mul(7)
                .wrapping_add(index.wrapping_mul(59))
        })
        .collect()
}

/// A one-layer RGB PSD whose four planes merge into RGBA
/// (`reference/cpp/plugins/psdfile/psdparse/psdimage.cpp:306-420`, the
/// `mergeChannelsRgb` interleave).
#[test]
fn rgb_planes_merge_into_rgba() {
    let (width, height) = (40i32, 7i32);
    let pixels = (width * height) as usize;
    let planes: Vec<(i16, Vec<u8>)> = [-1i16, 0, 1, 2]
        .into_iter()
        .enumerate()
        .map(|(index, id)| (id, plane(index as u8, pixels)))
        .collect();
    let psd = build_psd(COLOR_MODE_RGB, width, height, &planes, None);
    let image = decode_layer(&psd, ImageMode::Image);

    assert_eq!((image.left, image.top), (0, 0));
    assert_eq!((image.width, image.height), (width as u32, height as u32));
    assert_eq!(image.opacity, 255);
    assert_eq!(image.pixels.len(), pixels * 4);
    let red = &planes[1].1;
    let green = &planes[2].1;
    let blue = &planes[3].1;
    let alpha = &planes[0].1;
    for pixel in 0..pixels {
        let offset = pixel * 4;
        assert_eq!(
            &image.pixels[offset..offset + 4],
            [red[pixel], green[pixel], blue[pixel], alpha[pixel]],
            "pixel {pixel}"
        );
    }
}

/// A grayscale plane is repeated across the colour channels
/// (`reference/cpp/plugins/psdfile/psdparse/psdimage.cpp:244-305`, the
/// `mergeChannelsGray` interleave).
#[test]
fn grayscale_plane_is_repeated() {
    let (width, height) = (37i32, 5i32);
    let pixels = (width * height) as usize;
    let gray = plane(3, pixels);
    let alpha = plane(9, pixels);
    let planes = [(0i16, gray.clone()), (-1i16, alpha.clone())];
    let psd = build_psd(COLOR_MODE_GRAYSCALE, width, height, &planes, None);
    let image = decode_layer(&psd, ImageMode::Image);

    for pixel in 0..pixels {
        let offset = pixel * 4;
        assert_eq!(
            &image.pixels[offset..offset + 4],
            [gray[pixel], gray[pixel], gray[pixel], alpha[pixel]],
            "pixel {pixel}"
        );
    }
}

/// A masked layer folds `alpha * mask / 255` over the overlap of the layer and
/// mask rects (`reference/cpp/plugins/psdfile/psdparse/psdimage.cpp:422-460`,
/// `mergeMaskToAlpha`), and the masked-image mode keeps the mask channel
/// (`:786-789`).
#[test]
fn masked_layer_folds_the_mask_into_alpha() {
    let (width, height) = (37i32, 5i32);
    let pixels = (width * height) as usize;
    let red = plane(1, pixels);
    let green = plane(2, pixels);
    let blue = plane(4, pixels);
    let alpha = plane(6, pixels);
    let mask = plane(11, pixels);
    let planes = [
        (0i16, red.clone()),
        (1i16, green.clone()),
        (2i16, blue.clone()),
        (-1i16, alpha.clone()),
        (-2i16, mask.clone()),
    ];
    let rect = [0, 0, height, width];
    let psd = build_psd(COLOR_MODE_RGB, width, height, &planes, Some(rect));
    let image = decode_layer(&psd, ImageMode::MaskedImage);

    for pixel in 0..pixels {
        let offset = pixel * 4;
        let expected = u32::from(alpha[pixel]) * u32::from(mask[pixel]) / 255;
        assert_eq!(
            &image.pixels[offset..offset + 3],
            [red[pixel], green[pixel], blue[pixel]]
        );
        assert_eq!(image.pixels[offset + 3], expected as u8, "pixel {pixel}");
    }
}

/// A layer whose channel payload is large enough to expand on the fork-join
/// pool decodes to the same bytes as the per-pixel rule
/// (`reference/cpp/plugins/psdfile/psdparse/psdimage.cpp:306-420`).
#[test]
fn large_layer_decodes_like_the_scalar_merge() {
    let (width, height) = (320i32, 240i32);
    let pixels = (width * height) as usize;
    let planes: Vec<(i16, Vec<u8>)> = [-1i16, 0, 1, 2]
        .into_iter()
        .enumerate()
        .map(|(index, id)| (id, plane(index as u8 + 20, pixels)))
        .collect();
    let psd = build_psd(COLOR_MODE_RGB, width, height, &planes, None);
    let image = decode_layer(&psd, ImageMode::Image);

    let mut expected = Vec::with_capacity(pixels * 4);
    for pixel in 0..pixels {
        expected.extend_from_slice(&[
            planes[1].1[pixel],
            planes[2].1[pixel],
            planes[3].1[pixel],
            planes[0].1[pixel],
        ]);
    }
    assert_eq!(image.pixels, expected);
}
