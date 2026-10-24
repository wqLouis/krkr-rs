//! PSD pixel decoding: RLE / zip expansion and channel merging.
//!
//! Channels are decoded into byte buffers in the file's own big-endian layout,
//! then merged by colour mode into straight-alpha RGBA, which is what a scene
//! bitmap stores.
//!
//! # Cost model
//!
//! Channel payloads are independent, so a layer's channels expand on a
//! fork-join pool when the payload is large enough to pay for the pool;
//! `PARALLEL_DECODE_BYTES` chooses the schedule, never the output. The 8-bit
//! plane-to-RGBA and mask-to-alpha passes run through `crate::simd`. Nothing
//! is cached: every call owns its pool and its buffers.

use std::io::Read;

use flate2::read::ZlibDecoder;

use crate::error::{PsdError, PsdResult};
use crate::parallel;
use crate::parse::{
    BlendMode, COLOR_MODE_BITMAP, COLOR_MODE_CMYK, COLOR_MODE_DUOTONE, COLOR_MODE_GRAYSCALE,
    COLOR_MODE_INDEXED, COLOR_MODE_LAB, COLOR_MODE_MULTICHANNEL, COLOR_MODE_RGB, ChannelInfo,
    ImageMode, LayerInfo, PsdFile, Reader,
};
use crate::simd;

/// Channel payload size below which the decode stays on the calling thread,
/// where spawning a pool costs more than expanding the bytes.
///
/// This is a scheduling threshold: the buffers, the pixels and the errors are
/// the same on both paths.
const PARALLEL_DECODE_BYTES: usize = 256 * 1024;

/// A decoded layer image ready to be placed on a scene bitmap.
///
/// The image owns its pixels; the decoder keeps nothing.
pub struct LayerImage {
    /// The image left edge in document pixels.
    pub left: i32,
    /// The image top edge in document pixels.
    pub top: i32,
    /// The image width in pixels.
    pub width: u32,
    /// The image height in pixels.
    pub height: u32,
    /// The layer opacity (0..=255).
    pub opacity: u8,
    /// The blend mode to composite the image with.
    pub blend: BlendMode,
    /// The straight-alpha RGBA pixels, row by row.
    pub pixels: Vec<u8>,
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
/// A corrupt RLE or zip stream is a typed error rather than a zero-filled
/// buffer.
fn decode_channel(
    channel: &ChannelInfo,
    width: i32,
    height: i32,
    depth: u16,
) -> PsdResult<Vec<u8>> {
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
            inflate_channel(&mut out, &channel.data)?;
        }
        3 => {
            decode_zip_with_prediction(&mut out, &channel.data, width, height, depth)?;
        }
        _ => out.fill(0xff),
    }
    Ok(out)
}

/// One channel decode: the record, the rectangle it covers and the result.
struct ChannelJob<'a> {
    channel: &'a ChannelInfo,
    width: i32,
    height: i32,
    output: Vec<u8>,
    error: Option<PsdError>,
}

impl ChannelJob<'_> {
    /// Expand this channel into its own buffer.
    fn decode(&mut self, depth: u16) {
        match decode_channel(self.channel, self.width, self.height, depth) {
            Ok(bytes) => self.output = bytes,
            Err(error) => {
                self.output.clear();
                self.error = Some(error);
            }
        }
    }
}

/// Expand every channel, spreading the independent payloads over a pool when
/// they are worth it.
///
/// The first failing job in channel order decides the error, so the reported
/// error does not depend on the schedule.
fn decode_channels(mut jobs: Vec<ChannelJob<'_>>, depth: u16) -> PsdResult<Vec<Vec<u8>>> {
    let payload: usize = jobs
        .iter()
        .map(|job| channel_byte_size(job.width, job.height, depth))
        .sum();
    if jobs.len() > 1 && payload >= PARALLEL_DECODE_BYTES {
        parallel::for_each(&mut jobs, |job| job.decode(depth));
    } else {
        for job in jobs.iter_mut() {
            job.decode(depth);
        }
    }
    let mut channels = Vec::with_capacity(jobs.len());
    for job in jobs {
        let ChannelJob { output, error, .. } = job;
        match error {
            Some(error) => return Err(error),
            None => channels.push(output),
        }
    }
    Ok(channels)
}

/// Expand PackBits rows into `dst`.
///
/// `channels`/`target_ch`/`line_data_offset` mirror the merged-image call: the
/// row-count table covers every channel and the packed body follows it.
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

/// Inflate `expected` bytes and return them with the number of input bytes
/// consumed.
fn inflate_exact(src: &[u8], expected: usize) -> PsdResult<(Vec<u8>, usize)> {
    let mut decoder = ZlibDecoder::new(src);
    let mut buffer = Vec::with_capacity(expected);
    decoder
        .read_to_end(&mut buffer)
        .map_err(|error| PsdError::Decompress(error.to_string()))?;
    let consumed = decoder.total_in() as usize;
    Ok((buffer, consumed))
}

/// Inflate a zlib stream into `dst`, returning the number of input bytes the
/// stream consumed.
fn inflate_channel(dst: &mut [u8], src: &[u8]) -> PsdResult<usize> {
    let (buffer, consumed) = inflate_exact(src, dst.len())?;
    let count = buffer.len().min(dst.len());
    dst[..count].copy_from_slice(&buffer[..count]);
    Ok(consumed)
}

/// Inflate a zip-compressed channel and undo the per-channel prediction,
/// returning the number of input bytes the zlib stream consumed.
fn decode_zip_with_prediction(
    dst: &mut [u8],
    src: &[u8],
    width: i32,
    height: i32,
    depth: u16,
) -> PsdResult<usize> {
    if width <= 0 || height <= 0 {
        return Ok(0);
    }
    let width = width as usize;
    let height = height as usize;
    let (mut buffer, consumed) = inflate_exact(src, dst.len())?;
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
    Ok(consumed)
}

// ---------------------------------------------------------------------------
// Sampling / merging
// ---------------------------------------------------------------------------

/// The channels a merge reads: red, green, blue and alpha, as indices into
/// the decoded channel list.
type Planes = [Option<usize>; 4];

/// The index of the first decoded channel with `id`.
fn find_channel(ids: &[i16], id: i16) -> Option<usize> {
    ids.iter().position(|&candidate| candidate == id)
}

/// Read one colour component scaled to 8 bits: 8-bit channels keep the byte,
/// 16-bit channels keep their high byte and 32-bit channels are floats.
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

/// One colour component of a merged pixel: the channel sample, or `fill` when
/// the channel is missing.
fn component(plane: Option<usize>, channels: &[Vec<u8>], pixel: usize, depth: u16, fill: u8) -> u8 {
    match plane {
        Some(index) => sample(&channels[index], pixel, depth),
        None => fill,
    }
}

/// Merge the four planes into straight-alpha RGBA.
fn merge_planes(
    width: usize,
    height: usize,
    channels: &[Vec<u8>],
    depth: u16,
    planes: Planes,
) -> Vec<u8> {
    let mut out = vec![0u8; width * height * 4];
    if depth == 8 {
        let slices = [
            planes[0].map(|index| channels[index].as_slice()),
            planes[1].map(|index| channels[index].as_slice()),
            planes[2].map(|index| channels[index].as_slice()),
            planes[3].map(|index| channels[index].as_slice()),
        ];
        simd::pack_rgba8(&mut out, slices);
        return out;
    }
    for pixel in 0..width * height {
        let offset = pixel * 4;
        out[offset] = component(planes[0], channels, pixel, depth, 0);
        out[offset + 1] = component(planes[1], channels, pixel, depth, 0);
        out[offset + 2] = component(planes[2], channels, pixel, depth, 0);
        out[offset + 3] = component(planes[3], channels, pixel, depth, 0xff);
    }
    out
}

/// Merge CMYK planes into RGBA.
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

/// Merge an indexed plane through the palette.
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

/// Merge a 1-bit bitmap plane into RGBA.
fn merge_bitmap(width: usize, height: usize, src: &[u8]) -> Vec<u8> {
    let stride = width.div_ceil(8);
    let mut out = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let byte = src.get(y * stride + x / 8).copied().unwrap_or(0);
            // Bit set = black, clear = white.
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

/// Merge a document's decoded channels into straight-alpha RGBA.
fn merge_channels(
    mode: u16,
    width: usize,
    height: usize,
    ids: &[i16],
    channels: &[Vec<u8>],
    depth: u16,
    palette: &[[u8; 4]],
) -> Vec<u8> {
    match mode {
        COLOR_MODE_BITMAP => channels.first().map_or_else(
            || vec![0; width * height * 4],
            |c| merge_bitmap(width, height, c),
        ),
        COLOR_MODE_GRAYSCALE => {
            let gray = find_channel(ids, 0);
            merge_planes(
                width,
                height,
                channels,
                depth,
                [gray, gray, gray, find_channel(ids, -1)],
            )
        }
        COLOR_MODE_RGB => merge_planes(
            width,
            height,
            channels,
            depth,
            [
                find_channel(ids, 0),
                find_channel(ids, 1),
                find_channel(ids, 2),
                find_channel(ids, -1),
            ],
        ),
        COLOR_MODE_INDEXED => channels.first().map_or_else(
            || vec![0; width * height * 4],
            |c| merge_index(width, height, c, palette),
        ),
        COLOR_MODE_CMYK => merge_cmyk(width, height, ids, channels, depth),
        // Multichannel / duotone / LAB have no merge.
        COLOR_MODE_MULTICHANNEL | COLOR_MODE_DUOTONE | COLOR_MODE_LAB => {
            vec![0; width * height * 4]
        }
        _ => vec![0; width * height * 4],
    }
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

/// Fold a layer mask into its alpha channel.
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
        if depth == 8 {
            for y in 0..height as usize {
                let a_start = (a_offset_y + y) * aw + a_offset_x;
                let m_start = (m_offset_y + y) * mw + m_offset_x;
                let a_row = &mut alpha[a_start..a_start + width as usize];
                let m_row = &mask[m_start..m_start + width as usize];
                simd::mul_alpha8(a_row, m_row);
            }
        } else {
            for y in 0..height as usize {
                for x in 0..width as usize {
                    let ai = (a_offset_y + y) * aw + (a_offset_x + x);
                    let mi = (m_offset_y + y) * mw + (m_offset_x + x);
                    match depth {
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

/// Decode one layer with `mode` applied.
///
/// `mode` selects which image of the layer is decoded (the colour composite,
/// a single channel or the layer mask). `Ok(None)` means the layer stores no
/// image for that mode; the pixels are RGBA8, row-major.
///
/// # Errors
///
/// [`PsdError::Eof`] when a channel ends early,
/// [`PsdError::Decompress`] when a compressed channel cannot be expanded, and
/// [`PsdError::Invalid`] when the channel data is malformed.
pub fn layer_image(
    psd: &PsdFile,
    layer: &LayerInfo,
    mode: ImageMode,
) -> PsdResult<Option<LayerImage>> {
    let mask = &layer.extra_data.layer_mask;

    // A mask with no stored image is a 1x1 fill with the default colour.
    if mode == ImageMode::Mask && (mask.width <= 0 || mask.height <= 0) {
        let color = mask.default_color as u8;
        return Ok(Some(LayerImage {
            left: 0,
            top: 0,
            width: 1,
            height: 1,
            opacity: 255,
            blend: BlendMode::Normal,
            pixels: vec![color, color, color, 255],
        }));
    }

    let (left, top, width, height, opacity, blend) = match mode {
        ImageMode::Mask => (
            mask.left,
            mask.top,
            mask.width,
            mask.height,
            255,
            BlendMode::Normal,
        ),
        _ => (
            layer.left,
            layer.top,
            layer.width,
            layer.height,
            layer.opacity,
            layer.blend_mode,
        ),
    };

    if width <= 0 || height <= 0 {
        return Ok(None);
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

    let mut jobs: Vec<ChannelJob<'_>> = Vec::new();
    let mut ids: Vec<i16> = Vec::new();
    let mut alpha_index: Option<usize> = None;
    let mut mask_index: Option<usize> = None;

    for channel in &layer.channels {
        let is_mask = channel.is_mask_channel();
        let selected = match mode {
            // The masked-image view is the only one that needs the mask
            // channel: it folds it into alpha below.
            ImageMode::Image => !is_mask,
            ImageMode::Mask => is_mask,
            ImageMode::MaskedImage => true,
        };
        if !selected {
            continue;
        }
        let (cw, ch) = if is_mask {
            (mask_rect.width(), mask_rect.height())
        } else {
            (image_rect.width(), image_rect.height())
        };
        match channel.id {
            -1 => alpha_index = Some(jobs.len()),
            -2 | -3 => mask_index = Some(jobs.len()),
            _ => {}
        }
        jobs.push(ChannelJob {
            channel,
            width: cw,
            height: ch,
            output: Vec::new(),
            error: None,
        });
        ids.push(channel.id);
    }

    if mode == ImageMode::Mask && mask_index.is_none() {
        return Ok(None);
    }

    let mut channels = decode_channels(jobs, depth)?;

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
        let mask_channel = std::mem::take(&mut channels[mask_index]);
        let alpha = &mut channels[alpha_index];
        merge_mask_to_alpha(
            alpha,
            image_rect,
            &mask_channel,
            mask_rect,
            mask.default_color as u8,
            depth,
        );
    }

    // In mask mode the sole selected channel is presented as grayscale.
    if mode == ImageMode::Mask
        && let Some(first) = ids.first_mut()
    {
        *first = 0;
    }

    let pixels = merge_channels(
        color_mode,
        output_width,
        output_height,
        &ids,
        &channels,
        depth,
        &psd.color_table.colors,
    );

    Ok(Some(LayerImage {
        left,
        top,
        width: output_width as u32,
        height: output_height as u32,
        opacity,
        blend,
        pixels,
    }))
}

/// Decode the merged composite image.
///
/// The result is the composite's width, height and row-major RGBA8 pixels.
/// `Ok(None)` means the document carries no merged composite or no usable
/// image data.
///
/// # Errors
///
/// [`PsdError::Eof`] when a channel ends early,
/// [`PsdError::Decompress`] when a compressed channel cannot be expanded, and
/// [`PsdError::Invalid`] when the composite is malformed.
pub fn merged_image(psd: &PsdFile) -> PsdResult<Option<(u32, u32, Vec<u8>)>> {
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

    let mut reader = Reader::new(image_data);
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
                let mut decoded = vec![0u8; size];
                let consumed = inflate_channel(&mut decoded, &body[offset..])?;
                offset += consumed;
                channels.push(decoded);
            }
        }
        3 => {
            let mut offset = 0usize;
            for _ in 0..channel_count {
                let mut decoded = vec![0u8; size];
                // The prediction consumes exactly `size` decompressed bytes.
                let consumed = decode_zip_with_prediction(
                    &mut decoded,
                    &body[offset..],
                    width,
                    height,
                    depth,
                )?;
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
    let pixels = merge_channels(
        psd.header.mode,
        width,
        height,
        &ids,
        &channels,
        depth,
        &psd.color_table.colors,
    );

    Ok(Some((width as u32, height as u32, pixels)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A corrupt zlib stream is a typed decompression error, not a silent
    /// zero fill (`reference/cpp/plugins/psdfile/psdparse/psdimage.cpp:589-605`
    /// returns false when inflate fails).
    #[test]
    fn corrupt_zip_channel_is_an_error() {
        let channel = ChannelInfo {
            id: 0,
            length: 0,
            compression: 2,
            data: b"not a zlib stream".to_vec(),
        };
        assert!(matches!(
            decode_channel(&channel, 8, 1, 8),
            Err(PsdError::Decompress(_))
        ));
    }

    /// One PackBits-compressed channel: a per-row byte count, then literal runs
    /// of 128 bytes (`reference/cpp/plugins/psdfile/psdparse/psdimage.cpp:548-580`).
    fn packbits_channel(id: i16, plane: &[u8], width: usize, height: usize) -> ChannelInfo {
        let runs = width / 128;
        let line_bytes = runs * 129;
        let mut data = Vec::new();
        for _ in 0..height {
            data.extend_from_slice(&(line_bytes as u16).to_be_bytes());
        }
        for row in 0..height {
            for run in 0..runs {
                data.push(127); // 128 literal bytes follow.
                let start = row * width + run * 128;
                data.extend_from_slice(&plane[start..start + 128]);
            }
        }
        ChannelInfo {
            id,
            length: (data.len() + 2) as i32,
            compression: 1,
            data,
        }
    }

    /// The channel bytes do not depend on the schedule: the same payload
    /// expanded on the fork-join pool and on the calling thread yields the
    /// same bytes (`reference/cpp/plugins/psdfile/psdparse/psdimage.cpp:548-580`).
    #[test]
    fn channel_decode_is_the_same_on_both_schedules() {
        let width = 512i32;
        let height = 256i32;
        let plane: Vec<u8> = (0..(width * height) as usize)
            .map(|index| (index % 251) as u8)
            .collect();
        let channel = packbits_channel(0, &plane, width as usize, height as usize);

        fn jobs_for(
            channel: &ChannelInfo,
            width: i32,
            height: i32,
            count: usize,
        ) -> Vec<ChannelJob<'_>> {
            (0..count)
                .map(|_| ChannelJob {
                    channel,
                    width,
                    height,
                    output: Vec::new(),
                    error: None,
                })
                .collect()
        }

        // Four channels of 128 KiB each clear the parallel threshold; a single
        // channel of the same size stays on the calling thread.
        let spread =
            decode_channels(jobs_for(&channel, width, height, 4), 8).expect("the payload decodes");
        let serial =
            decode_channels(jobs_for(&channel, width, height, 1), 8).expect("the payload decodes");

        assert_eq!(spread[0], serial[0]);
        assert_eq!(spread[0], plane);
    }
}
