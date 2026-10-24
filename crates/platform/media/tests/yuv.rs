//! Integration tests for the pure YUV420 -> RGBA conversion.

use media::yuv::{CropRect, Yuv420Layout, YuvSampling, micros_to_millis, yuv420_to_rgba};

/// Build a planar (I420) buffer whose Y plane is `ys`, U plane `us`, V
/// plane `vs`, with the given stride and slice height.
fn planar_buffer(ys: &[u8], us: &[u8], vs: &[u8], stride: usize, rows: usize) -> Vec<u8> {
    let mut buffer = Vec::new();
    buffer.extend_from_slice(ys);
    buffer.extend_from_slice(us);
    buffer.extend_from_slice(vs);
    assert_eq!(ys.len(), stride * rows);
    buffer
}

fn solid_planar(width: usize, height: usize, stride: usize) -> Yuv420Layout {
    Yuv420Layout::new(
        stride,
        height,
        YuvSampling::Planar,
        CropRect::full(width, height),
    )
}

#[test]
fn solid_black_and_white_planar() {
    // 2x2, no padding. Y=16/U=V=128 is black; Y=235/U=V=128 is white.
    let layout = solid_planar(2, 2, 2);
    let black = planar_buffer(&[16; 4], &[128; 2], &[128; 2], 2, 2);
    let rgba = yuv420_to_rgba(&black, &layout);
    assert_eq!(rgba.len(), 2 * 2 * 4);
    for pixel in rgba.as_chunks::<4>().0 {
        assert_eq!(pixel, &[0, 0, 0, 255], "black pixel");
    }

    let white = planar_buffer(&[235; 4], &[128; 2], &[128; 2], 2, 2);
    let rgba = yuv420_to_rgba(&white, &layout);
    for pixel in rgba.as_chunks::<4>().0 {
        assert_eq!(pixel, &[255, 255, 255, 255], "white pixel");
    }
}

#[test]
fn a_colour_is_converted() {
    // A single 1x1 frame with chroma pushed towards red.
    let layout = Yuv420Layout::new(1, 1, YuvSampling::Planar, CropRect::full(1, 1));
    let buffer = planar_buffer(&[81], &[90], &[240], 1, 1);
    let rgba = yuv420_to_rgba(&buffer, &layout);
    assert_eq!(rgba.len(), 4);
    // BT.601 limited: this is a strongly red pixel.
    assert!(rgba[0] > 200, "red channel {}", rgba[0]);
    assert!(rgba[1] < 60, "green channel {}", rgba[1]);
    assert!(rgba[2] < 60, "blue channel {}", rgba[2]);
    assert_eq!(rgba[3], 255);
}

#[test]
fn odd_width_rounds_chroma_up() {
    // 3x2 with a stride of 3: the chroma plane is ceil(3/2)=2 columns
    // wide and ceil(2/2)=1 row tall, so it has 2 bytes per plane.
    let layout = Yuv420Layout::new(3, 2, YuvSampling::Planar, CropRect::full(3, 2));
    assert_eq!(layout.uv_stride, 2);
    assert_eq!(layout.uv_rows, 1);
    let buffer = planar_buffer(&[235; 6], &[128; 2], &[128; 2], 3, 2);
    let rgba = yuv420_to_rgba(&buffer, &layout);
    assert_eq!(rgba.len(), 3 * 2 * 4);
    for pixel in rgba.as_chunks::<4>().0 {
        assert_eq!(pixel, &[255, 255, 255, 255]);
    }
}

#[test]
fn stride_larger_than_width_skips_padding() {
    // 2x2 visible, stride 4. The padding bytes are junk; a correct
    // converter never reads them.
    let layout = Yuv420Layout::new(4, 2, YuvSampling::Planar, CropRect::full(2, 2));
    let mut y = vec![0u8; 8];
    y[0] = 235;
    y[1] = 235;
    y[4] = 235;
    y[5] = 235;
    // junk in the padding columns
    y[2] = 0;
    y[3] = 0;
    y[6] = 0;
    y[7] = 0;
    let buffer = planar_buffer(&y, &[128; 2], &[128; 2], 4, 2);
    let rgba = yuv420_to_rgba(&buffer, &layout);
    assert_eq!(rgba.len(), 2 * 2 * 4);
    for pixel in rgba.as_chunks::<4>().0 {
        assert_eq!(pixel, &[255, 255, 255, 255]);
    }
}

#[test]
fn crop_rectangle_selects_the_visible_region() {
    // 4x4 coded frame, visible region (1,1)..(3,3) => 2x2.
    let layout = Yuv420Layout::new(
        4,
        4,
        YuvSampling::Planar,
        CropRect {
            left: 1,
            top: 1,
            right: 3,
            bottom: 3,
        },
    );
    assert_eq!(layout.width(), 2);
    assert_eq!(layout.height(), 2);

    // Luma is white only inside the crop; black outside.
    let mut y = vec![16u8; 16];
    for (index, value) in y.iter_mut().enumerate() {
        let (row, col) = (index / 4, index % 4);
        if (1..3).contains(&row) && (1..3).contains(&col) {
            *value = 235;
        }
    }
    let buffer = planar_buffer(&y, &[128; 4], &[128; 4], 4, 4);
    let rgba = yuv420_to_rgba(&buffer, &layout);
    assert_eq!(rgba.len(), 2 * 2 * 4);
    for pixel in rgba.as_chunks::<4>().0 {
        assert_eq!(pixel, &[255, 255, 255, 255], "cropped-in pixel is white");
    }
}

#[test]
fn semi_planar_nv12_interleaves_chroma() {
    // 2x2 NV12: Y plane then uv uv / uv uv.
    let layout = Yuv420Layout::new(2, 2, YuvSampling::SemiPlanar, CropRect::full(2, 2));
    assert_eq!(layout.uv_stride, 2);
    let buffer = vec![235, 235, 235, 235, 128, 128, 128, 128];
    let rgba = yuv420_to_rgba(&buffer, &layout);
    for pixel in rgba.as_chunks::<4>().0 {
        assert_eq!(pixel, &[255, 255, 255, 255]);
    }
}

#[test]
fn short_buffer_does_not_panic() {
    let layout = Yuv420Layout::new(2, 2, YuvSampling::Planar, CropRect::full(2, 2));
    let rgba = yuv420_to_rgba(&[], &layout);
    assert_eq!(rgba.len(), 2 * 2 * 4);
    // Every read is bounds-checked, so the pixel is opaque; the value is
    // otherwise meaningless (all-zero chroma is not neutral).
    for pixel in rgba.as_chunks::<4>().0 {
        assert_eq!(pixel[3], 255);
    }
}

#[test]
fn micros_rescale() {
    assert_eq!(micros_to_millis(0), 0);
    assert_eq!(micros_to_millis(999), 0);
    assert_eq!(micros_to_millis(1000), 1);
    assert_eq!(micros_to_millis(1_500_499), 1500);
    assert_eq!(micros_to_millis(5_000_000), 5000);
}
