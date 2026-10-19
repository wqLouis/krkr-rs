//! PSD pixel decoding: RLE / zip expansion and channel merging.
//!
//! Behavioral reference: `reference/cpp/plugins/psdfile/psdparse/psdimage.cpp`.
//! The reference decodes each channel into a typed buffer (`uint8_t`,
//! `uint16_t` or `uint32_t`/float) and merges by colour mode into a packed
//! BGRA buffer. This port decodes into byte buffers with the same big-endian
//! layout and merges into straight-alpha **RGBA**, which is what the L2
//! `engine_api::Scene` bitmaps store.

use std::io::Read;

use flate2::read::ZlibDecoder;

use super::parse::{
    BlendMode, COLOR_MODE_BITMAP, COLOR_MODE_CMYK, COLOR_MODE_DUOTONE, COLOR_MODE_GRAYSCALE,
    COLOR_MODE_INDEXED, COLOR_MODE_LAB, COLOR_MODE_MULTICHANNEL, COLOR_MODE_RGB, ChannelInfo,
    ImageMode, LayerInfo, PsdError, PsdFile, Result,
};

/// A decoded layer image ready to be placed on an L2 bitmap.
pub(crate) struct LayerImage {
    pub(crate) left: i32,
    pub(crate) top: i32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) opacity: u8,
    /// The TVP `tTVPLayerType` to map onto the layer.
    pub(crate) native_type: i32,
    pub(crate) pixels: Vec<u8>,
}

/// A half-open pixel rectangle.
#[derive(Debug, Clone, Copy)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl Rect {
    fn width(self) -> i32 {
        self.right - self.left
    }

    fn height(self) -> i32 {
        self.bottom - self.top
    }
}

/// The byte size of one `width` x `height` channel at `depth` bits.
fn channel_byte_size(width: i32, height: i32, depth: u16) -> usize {
    if width <= 0 || height <= 0 {
        return 0;
    }
    let pixels = width as usize * height as usize;
    match depth {
        1 => (width as usize).div_ceil(8) * height as usize,
        8 => pixels,
        16 => pixels * 2,
        32 => pixels * 4,
        _ => 0,
    }
}

/// Decode one channel, returning its raw (big-endian) channel buffer.
///
/// The reference `getLayerImage` ignores the RLE/zip return value, so a
/// corrupt stream yields whatever the destination held; this port surfaces a
/// decompression failure as a typed error instead of a silent zero fill.
fn decode_channel(channel: &ChannelInfo, width: i32, height: i32, depth: u16) -> Result<Vec<u8>> {
    let size = channel_byte_size(width, height, depth);
    let mut out = vec![0u8; size];
    match channel.compression {
        0 => {
            let count = channel.data.len().min(size);
            out[..count].copy_from_slice(&channel.data[..count]);
        }
        1 => {
            decode_packbits(&mut out, &channel.data, height.max(0) as usize, 1, 0, 0);
        }
        2 => {
            inflate_into(&mut out, &channel.data)?;
        }
        3 => {
            decode_zip_with_prediction(&mut out, &channel.data, width, height, depth)?;
        }
        _ => out.fill(0xff),
    }
    Ok(out)
}

/// Expand PackBits rows into `dst` (reference `decodePackBits`).
///
/// `channels`/`target_ch`/`line_data_offset` mirror the merged-image call:
/// the row-count table covers every channel and the packed body follows it.
fn decode_packbits(
    dst: &mut [u8],
    src: &[u8],
    height: usize,
    channels: usize,
    target_ch: usize,
    line_data_offset: usize,
) -> usize {
    let header_size = 2usize.saturating_mul(height).saturating_mul(channels);
    let mut body = header_size.saturating_add(line_data_offset);
    let mut out = 0usize;
    let mut read_bytes = 0usize;

    for y in 0..height {
        let count_offset = 2 * (height.saturating_mul(target_ch).saturating_add(y));
        if count_offset + 1 >= src.len() {
            break;
        }
        let line_bytes = u16::from_be_bytes([src[count_offset], src[count_offset + 1]]) as usize;
        let mut consumed = 0usize;
        while consumed < line_bytes {
            if body >= src.len() {
                return read_bytes;
            }
            let length = src[body];
            body += 1;
            consumed += 1;
            if length > 128 {
                // Repeat the next byte `257 - length` times.
                let count = 257 - length as usize;
                if body >= src.len() {
                    return read_bytes;
                }
                let value = src[body];
                body += 1;
                consumed += 1;
                for _ in 0..count {
                    if out < dst.len() {
                        dst[out] = value;
                    }
                    out += 1;
                }
            } else if length < 128 {
                // Copy the next `length + 1` bytes literally.
                let count = length as usize + 1;
                for _ in 0..count {
                    if body < src.len() && out < dst.len() {
                        dst[out] = src[body];
                    }
                    body += 1;
                    out += 1;
                }
                consumed += count;
            }
            // `128` (0x80) is a no-op.
        }
        read_bytes = read_bytes.saturating_add(line_bytes);
    }
    read_bytes
}

/// Inflate a zlib stream into `dst` (reference `decodeZipWithoutPrediction`).
fn inflate_into(dst: &mut [u8], src: &[u8]) -> Result<()> {
    let (buffer, _) = inflate_exact(src, dst.len())?;
    let count = buffer.len().min(dst.len());
    dst[..count].copy_from_slice(&buffer[..count]);
    Ok(())
}

/// Inflate `expected` bytes and return them with the number of input bytes
/// consumed.
fn inflate_exact(src: &[u8], expected: usize) -> Result<(Vec<u8>, usize)> {
    let mut decoder = ZlibDecoder::new(src);
    let mut buffer = Vec::with_capacity(expected);
    decoder
        .read_to_end(&mut buffer)
        .map_err(|error| PsdError::Decompress(error.to_string()))?;
    let consumed = decoder.total_in() as usize;
    Ok((buffer, consumed))
}

/// Inflate a zip-compressed channel and undo the per-channel prediction
/// (reference `decodeZipWithPrediction`).
fn decode_zip_with_prediction(
    dst: &mut [u8],
    src: &[u8],
    width: i32,
    height: i32,
    depth: u16,
) -> Result<()> {
    if width <= 0 || height <= 0 {
        return Ok(());
    }
    let width = width as usize;
    let height = height as usize;
    let (mut buffer, _) = inflate_exact(src, dst.len())?;
    buffer.resize(dst.len(), 0);

    match depth {
        8 => {
            for row in 0..height {
                let start = row * width;
                for x in 1..width {
                    let index = start + x;
                    if index < buffer.len() {
                        buffer[index] = buffer[index].wrapping_add(buffer[index - 1]);
                    }
                }
            }
            dst.copy_from_slice(&buffer);
        }
        16 => {
            for row in 0..height {
                let base = row * width * 2;
                for x in (0..(width - 1) * 2).step_by(2) {
                    let a = buffer[base + x];
                    let b = buffer[base + x + 1];
                    let c = buffer[base + x + 2];
                    let d = buffer[base + x + 3];
                    buffer[base + x + 2] = c
                        .wrapping_add(a)
                        .wrapping_add(((d as u32 + b as u32) / 256) as u8);
                    buffer[base + x + 3] = d.wrapping_add(b);
                }
            }
            dst.copy_from_slice(&buffer);
        }
        32 => {
            // Byte-wise delta restore, then de-interleave the four planes.
            for row in 0..height {
                let base = row * width * 4;
                for x in 1..width * 4 {
                    let index = base + x;
                    if index < buffer.len() {
                        buffer[index] = buffer[index].wrapping_add(buffer[index - 1]);
                    }
                }
            }
            let offset1 = width;
            let offset2 = 2 * width;
            let offset3 = 3 * width;
            for row in 0..height {
                let base = row * width * 4;
                for x in 0..width {
                    dst[base + x * 4] = buffer[base + x];
                    dst[base + x * 4 + 1] = buffer[base + x + offset1];
                    dst[base + x * 4 + 2] = buffer[base + x + offset2];
                    dst[base + x * 4 + 3] = buffer[base + x + offset3];
                }
            }
        }
        _ => return Err(PsdError::Decompress(format!("unsupported depth {depth}"))),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Sampling / merging
// ---------------------------------------------------------------------------

/// Read one colour component scaled to 8 bits, matching the reference byte
/// order (16-bit channels keep their low byte, 32-bit channels are floats).
fn sample(bytes: &[u8], index: usize, depth: u16) -> u8 {
    match depth {
        8 => bytes.get(index).copied().unwrap_or(0xff),
        16 => bytes.get(index * 2 + 1).copied().unwrap_or(0xff),
        32 => {
            let offset = index * 4;
            if offset + 4 <= bytes.len() {
                let value = f32::from_be_bytes([
                    bytes[offset],
                    bytes[offset + 1],
                    bytes[offset + 2],
                    bytes[offset + 3],
                ]);
                (value * 255.0).clamp(0.0, 255.0) as u8
            } else {
                0xff
            }
        }
        _ => 0xff,
    }
}

/// The index of the first decoded channel with `id`.
fn find_channel(ids: &[i16], id: i16) -> Option<usize> {
    ids.iter().position(|&candidate| candidate == id)
}

/// Merge RGB channels into RGBA (reference `mergeChannelsRgb`).
fn merge_rgb(
    width: usize,
    height: usize,
    ids: &[i16],
    channels: &[Vec<u8>],
    depth: u16,
) -> Vec<u8> {
    let r = find_channel(ids, 0);
    let g = find_channel(ids, 1);
    let b = find_channel(ids, 2);
    let a = find_channel(ids, -1);
    let mut out = vec![0u8; width * height * 4];
    for pixel in 0..width * height {
        let offset = pixel * 4;
        out[offset] = r.map_or(0, |c| sample(&channels[c], pixel, depth));
        out[offset + 1] = g.map_or(0, |c| sample(&channels[c], pixel, depth));
        out[offset + 2] = b.map_or(0, |c| sample(&channels[c], pixel, depth));
        out[offset + 3] = a.map_or(0xff, |c| sample(&channels[c], pixel, depth));
    }
    out
}

/// Merge a grayscale channel into RGBA (reference `mergeChannelsGray`).
fn merge_gray(
    width: usize,
    height: usize,
    ids: &[i16],
    channels: &[Vec<u8>],
    depth: u16,
) -> Vec<u8> {
    let g = find_channel(ids, 0);
    let a = find_channel(ids, -1);
    let mut out = vec![0u8; width * height * 4];
    for pixel in 0..width * height {
        let value = g.map_or(0, |c| sample(&channels[c], pixel, depth));
        let alpha = a.map_or(0xff, |c| sample(&channels[c], pixel, depth));
        let offset = pixel * 4;
        out[offset] = value;
        out[offset + 1] = value;
        out[offset + 2] = value;
        out[offset + 3] = alpha;
    }
    out
}

/// Merge CMYK channels into RGBA (reference `cmykCompoToRgba32`).
fn merge_cmyk(
    width: usize,
    height: usize,
    ids: &[i16],
    channels: &[Vec<u8>],
    depth: u16,
) -> Vec<u8> {
    let c = find_channel(ids, 0);
    let m = find_channel(ids, 1);
    let y = find_channel(ids, 2);
    let k = find_channel(ids, 3);
    let a = find_channel(ids, -1);
    let mut out = vec![0u8; width * height * 4];
    for pixel in 0..width * height {
        let cc = 255 - c.map_or(0, |ch| sample(&channels[ch], pixel, depth)) as i32;
        let mm = 255 - m.map_or(0, |ch| sample(&channels[ch], pixel, depth)) as i32;
        let yy = 255 - y.map_or(0, |ch| sample(&channels[ch], pixel, depth)) as i32;
        let kk = k.map_or(0, |ch| sample(&channels[ch], pixel, depth)) as i32;
        let inv_k = kk & 0xff;
        let offset = pixel * 4;
        out[offset] = ((inv_k * (255 - cc)) >> 8) as u8;
        out[offset + 1] = ((inv_k * (255 - mm)) >> 8) as u8;
        out[offset + 2] = ((inv_k * (255 - yy)) >> 8) as u8;
        out[offset + 3] = a.map_or(0xff, |ch| sample(&channels[ch], pixel, depth));
    }
    out
}

/// Merge an indexed channel through the palette (reference
/// `mergeChannelsIndex`).
fn merge_index(width: usize, height: usize, src: &[u8], table: &[[u8; 4]]) -> Vec<u8> {
    let mut out = vec![0u8; width * height * 4];
    for pixel in 0..width * height {
        let index = src.get(pixel).copied().unwrap_or(0) as usize;
        let color = table.get(index).copied().unwrap_or([0, 0, 0, 0xff]);
        let offset = pixel * 4;
        out[offset..offset + 4].copy_from_slice(&color);
    }
    out
}

/// Merge a 1-bit bitmap channel into RGBA (reference `mergeChannelsBitmap`).
fn merge_bitmap(width: usize, height: usize, src: &[u8]) -> Vec<u8> {
    let stride = width.div_ceil(8);
    let mut out = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let byte = src.get(y * stride + x / 8).copied().unwrap_or(0);
            // Bit set = black, clear = white (the reference's `c[1]`/`c[0]`).
            let value = if byte & (0x80 >> (x % 8)) != 0 {
                0
            } else {
                255
            };
            let offset = (y * width + x) * 4;
            out[offset] = value;
            out[offset + 1] = value;
            out[offset + 2] = value;
            out[offset + 3] = 255;
        }
    }
    out
}

/// Read a 16-bit big-endian element.
fn read_u16(bytes: &[u8], index: usize) -> u16 {
    let offset = index * 2;
    if offset + 2 <= bytes.len() {
        u16::from_be_bytes([bytes[offset], bytes[offset + 1]])
    } else {
        0
    }
}

/// Write a 16-bit big-endian element.
fn write_u16(bytes: &mut [u8], index: usize, value: u16) {
    let offset = index * 2;
    if offset + 2 <= bytes.len() {
        bytes[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
    }
}

/// Fold a layer mask into its alpha channel (reference `mergeMaskToAlpha`).
fn merge_mask_to_alpha(
    alpha: &mut [u8],
    alpha_rect: Rect,
    mask: &[u8],
    mask_rect: Rect,
    default_color: u8,
    depth: u16,
) {
    let aw = alpha_rect.width();
    let ah = alpha_rect.height();
    let mw = mask_rect.width();
    let mh = mask_rect.height();
    if aw <= 0 || ah <= 0 || mw <= 0 || mh <= 0 {
        return;
    }
    let aw = aw as usize;
    let ah = ah as usize;
    let mw = mw as usize;

    let left = alpha_rect.left.max(mask_rect.left);
    let top = alpha_rect.top.max(mask_rect.top);
    let right = alpha_rect.right.min(mask_rect.right);
    let bottom = alpha_rect.bottom.min(mask_rect.bottom);
    let width = right - left;
    let height = bottom - top;

    if width > 0 && height > 0 {
        let a_offset_x = (left - alpha_rect.left) as usize;
        let a_offset_y = (top - alpha_rect.top) as usize;
        let m_offset_x = (left - mask_rect.left) as usize;
        let m_offset_y = (top - mask_rect.top) as usize;
        for y in 0..height as usize {
            for x in 0..width as usize {
                let ai = (a_offset_y + y) * aw + (a_offset_x + x);
                let mi = (m_offset_y + y) * mw + (m_offset_x + x);
                match depth {
                    8 => {
                        alpha[ai] = (alpha[ai] as u32 * mask[mi] as u32 / 255) as u8;
                    }
                    16 => {
                        let av = read_u16(alpha, ai) as u32;
                        let mv = read_u16(mask, mi) as u32;
                        write_u16(alpha, ai, (av * mv / 65535) as u16);
                    }
                    32 => {
                        let ao = ai * 4;
                        let mo = mi * 4;
                        if ao + 4 <= alpha.len() && mo + 4 <= mask.len() {
                            let av = f32::from_be_bytes([
                                alpha[ao],
                                alpha[ao + 1],
                                alpha[ao + 2],
                                alpha[ao + 3],
                            ]);
                            let mv = f32::from_be_bytes([
                                mask[mo],
                                mask[mo + 1],
                                mask[mo + 2],
                                mask[mo + 3],
                            ]);
                            alpha[ao..ao + 4].copy_from_slice(&(av * mv).to_be_bytes());
                        }
                    }
                    _ => {}
                }
            }
        }
    } else if default_color == 0 {
        alpha.fill(0);
        return;
    }

    // A zero default colour also clears the alpha outside the mask rectangle.
    if default_color == 0 {
        for y in 0..ah {
            for x in 0..aw {
                let gx = alpha_rect.left + x as i32;
                let gy = alpha_rect.top + y as i32;
                if gx < mask_rect.left
                    || gx >= mask_rect.right
                    || gy < mask_rect.top
                    || gy >= mask_rect.bottom
                {
                    let index = y * aw + x;
                    match depth {
                        8 => alpha[index] = 0,
                        16 => write_u16(alpha, index, 0),
                        32 => {
                            let offset = index * 4;
                            if offset + 4 <= alpha.len() {
                                alpha[offset..offset + 4].copy_from_slice(&0f32.to_be_bytes());
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Decode one layer (reference `PSDFile::getLayerImage`).
pub(crate) fn layer_image(
    psd: &PsdFile,
    layer: &LayerInfo,
    mode: ImageMode,
) -> Result<Option<LayerImage>> {
    let mask = &layer.extra_data.layer_mask;

    let (left, top, width, height, opacity, native_type) = match mode {
        ImageMode::Mask => {
            let (left, top, width, height) = if mask.width <= 0 || mask.height <= 0 {
                (0, 0, 1, 1)
            } else {
                (mask.left, mask.top, mask.width, mask.height)
            };
            (left, top, width, height, 255, BlendMode::Normal.as_tvp())
        }
        _ => (
            layer.left,
            layer.top,
            layer.width,
            layer.height,
            layer.opacity,
            layer.blend_mode.as_tvp(),
        ),
    };

    if width <= 0 || height <= 0 {
        return Ok(None);
    }

    // A mask with no stored image is a 1x1 fill with the default colour.
    if mode == ImageMode::Mask && (mask.width <= 0 || mask.height <= 0) {
        let color = mask.default_color as u8;
        return Ok(Some(LayerImage {
            left: 0,
            top: 0,
            width: 1,
            height: 1,
            opacity: 255,
            native_type: BlendMode::Normal.as_tvp(),
            pixels: vec![color, color, color, 255],
        }));
    }

    let depth = psd.header.depth;
    let image_rect = Rect {
        left: layer.left,
        top: layer.top,
        right: layer.right,
        bottom: layer.bottom,
    };
    let mask_rect = Rect {
        left: mask.left,
        top: mask.top,
        right: mask.right,
        bottom: mask.bottom,
    };

    let mut channels: Vec<Vec<u8>> = Vec::new();
    let mut ids: Vec<i16> = Vec::new();
    let mut alpha_index: Option<usize> = None;
    let mut mask_index: Option<usize> = None;

    for channel in &layer.channels {
        let is_mask = channel.is_mask_channel();
        match mode {
            ImageMode::Image | ImageMode::MaskedImage if is_mask => continue,
            ImageMode::Mask if !is_mask => continue,
            _ => {}
        }
        let (cw, ch) = if is_mask {
            (mask_rect.width(), mask_rect.height())
        } else {
            (image_rect.width(), image_rect.height())
        };
        let decoded = decode_channel(channel, cw, ch, depth)?;
        match channel.id {
            -1 => alpha_index = Some(channels.len()),
            -2 | -3 => mask_index = Some(channels.len()),
            _ => {}
        }
        channels.push(decoded);
        ids.push(channel.id);
    }

    if mode == ImageMode::Mask && mask_index.is_none() {
        return Ok(None);
    }

    let (output_width, output_height) = match mode {
        ImageMode::Mask => (
            mask_rect.width().max(1) as usize,
            mask_rect.height().max(1) as usize,
        ),
        _ => (
            image_rect.width().max(1) as usize,
            image_rect.height().max(1) as usize,
        ),
    };

    let color_mode = if mode == ImageMode::Mask {
        COLOR_MODE_GRAYSCALE
    } else {
        psd.header.mode
    };

    if mode == ImageMode::MaskedImage
        && let (Some(mask_index), Some(alpha_index)) = (mask_index, alpha_index)
    {
        let mask_channel = channels[mask_index].clone();
        if let Some(alpha) = channels.get_mut(alpha_index) {
            merge_mask_to_alpha(
                alpha,
                image_rect,
                &mask_channel,
                mask_rect,
                mask.default_color as u8,
                depth,
            );
        }
    }

    // In mask mode the sole selected channel is presented as grayscale.
    if mode == ImageMode::Mask
        && let Some(first) = ids.first_mut()
    {
        *first = 0;
    }

    let pixels = match color_mode {
        COLOR_MODE_BITMAP => channels.first().map_or_else(
            || vec![0; output_width * output_height * 4],
            |c| merge_bitmap(output_width, output_height, c),
        ),
        COLOR_MODE_GRAYSCALE => merge_gray(output_width, output_height, &ids, &channels, depth),
        COLOR_MODE_RGB => merge_rgb(output_width, output_height, &ids, &channels, depth),
        COLOR_MODE_INDEXED => channels.first().map_or_else(
            || vec![0; output_width * output_height * 4],
            |c| merge_index(output_width, output_height, c, &psd.color_table.colors),
        ),
        COLOR_MODE_CMYK => merge_cmyk(output_width, output_height, &ids, &channels, depth),
        // Multichannel / duotone / LAB are not merged by the reference either.
        COLOR_MODE_MULTICHANNEL | COLOR_MODE_DUOTONE | COLOR_MODE_LAB => {
            vec![0; output_width * output_height * 4]
        }
        _ => vec![0; output_width * output_height * 4],
    };

    Ok(Some(LayerImage {
        left,
        top,
        width: output_width as u32,
        height: output_height as u32,
        opacity,
        native_type,
        pixels,
    }))
}

/// Decode the merged composite image (reference `PSDFile::getMergedImage`).
pub(crate) fn merged_image(psd: &PsdFile) -> Result<Option<(u32, u32, Vec<u8>)>> {
    let Some(image_data) = psd.image_data.as_ref() else {
        return Ok(None);
    };
    let width = psd.header.width;
    let height = psd.header.height;
    if width <= 0 || height <= 0 {
        return Ok(None);
    }
    let depth = psd.header.depth;
    let channel_count = psd.header.channels as usize;
    if channel_count == 0 {
        return Ok(None);
    }
    let size = channel_byte_size(width, height, depth);
    if size == 0 {
        return Ok(None);
    }

    let mut reader = super::parse::Reader::new(image_data);
    let compression = reader.u16()?;
    let body = &image_data[2..];

    let mut channels: Vec<Vec<u8>> = Vec::with_capacity(channel_count);
    match compression {
        0 => {
            if body.len() < size * channel_count {
                return Err(PsdError::Decompress("truncated merged image data".into()));
            }
            for i in 0..channel_count {
                channels.push(body[i * size..(i + 1) * size].to_vec());
            }
        }
        1 => {
            let mut offset = 0usize;
            for i in 0..channel_count {
                let mut decoded = vec![0u8; size];
                offset += decode_packbits(
                    &mut decoded,
                    body,
                    height as usize,
                    channel_count,
                    i,
                    offset,
                );
                channels.push(decoded);
            }
        }
        2 => {
            let mut offset = 0usize;
            for _ in 0..channel_count {
                let (buffer, consumed) = inflate_exact(&body[offset..], size)?;
                offset += consumed;
                let mut decoded = vec![0u8; size];
                let count = buffer.len().min(size);
                decoded[..count].copy_from_slice(&buffer[..count]);
                channels.push(decoded);
            }
        }
        3 => {
            let mut offset = 0usize;
            for _ in 0..channel_count {
                let mut decoded = vec![0u8; size];
                // Decode from the remaining stream; the prediction consumes
                // exactly `size` decompressed bytes.
                let remaining = &body[offset..];
                decode_zip_with_prediction(&mut decoded, remaining, width, height, depth)?;
                // Advance past the zlib stream by re-inflating to measure the
                // consumed input length.
                let (_, consumed) = inflate_exact(remaining, size)?;
                offset += consumed;
                channels.push(decoded);
            }
        }
        _ => {
            for _ in 0..channel_count {
                channels.push(vec![0xff; size]);
            }
        }
    }

    let mut ids: Vec<i16> = (0..channel_count as i16).collect();
    if psd.header.mode == COLOR_MODE_RGB && ids.len() > 3 {
        ids[3] = -1;
    }

    let width = width as usize;
    let height = height as usize;
    let pixels = match psd.header.mode {
        COLOR_MODE_BITMAP => channels.first().map_or_else(
            || vec![0; width * height * 4],
            |c| merge_bitmap(width, height, c),
        ),
        COLOR_MODE_GRAYSCALE => merge_gray(width, height, &ids, &channels, depth),
        COLOR_MODE_RGB => merge_rgb(width, height, &ids, &channels, depth),
        COLOR_MODE_INDEXED => channels.first().map_or_else(
            || vec![0; width * height * 4],
            |c| merge_index(width, height, c, &psd.color_table.colors),
        ),
        COLOR_MODE_CMYK => merge_cmyk(width, height, &ids, &channels, depth),
        COLOR_MODE_MULTICHANNEL | COLOR_MODE_DUOTONE | COLOR_MODE_LAB => {
            vec![0; width * height * 4]
        }
        _ => vec![0; width * height * 4],
    };

    Ok(Some((width as u32, height as u32, pixels)))
}

#[cfg(test)]
mod tests {
    use super::inflate_into;

    /// A corrupt zlib stream is a typed decompression error, not a silent
    /// zero fill (`reference/cpp/plugins/psdfile/psdparse/psdimage.cpp:589-605`
    /// returns false when inflate fails).
    #[test]
    fn corrupt_zip_channel_is_an_error() {
        let mut dst = [0u8; 8];
        assert!(inflate_into(&mut dst, b"not a zlib stream").is_err());
    }
}
