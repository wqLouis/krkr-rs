//! KiriKiri TLG5/TLG6 image decoders.
//!
//! Faithful port of the reference implementation
//! (`reference/cpp/core/visual/LoadTLG.cpp` plus the TLG routines in
//! `reference/cpp/core/visual/tvpgl.cpp` and `SaveTLG6.cpp`, which
//! documents the compression pipeline). The formats:
//!
//! * **TLG5** — "TLG5.0\0raw\x1a" — four (or three) per-channel planes of
//!   prediction residuals, each block of `blockheight` rows compressed
//!   independently with a modified LZSS (a 4096-byte sliding window shared
//!   across the whole image). Decoding reverses the residual chain:
//!   `R = Σ(plane2 + plane1)`, `G = Σ(plane1)`, `B = Σ(plane0 + plane1)`,
//!   `A = Σ(plane3)`, with the previous scanline added back for every line
//!   after the first.
//!
//! * **TLG6** — "TLG6.0\0raw\x1a" — a block-based (8×8) codec. For each
//!   block group the four byte-lane planes of prediction errors (MED or
//!   "average" predictor, one method chosen per block) are run through one
//!   of 16 color-correlation filters (chosen per block), zigzag-reordered
//!   per 8×8 block, then entropy-coded with a run-length Golomb-Rice coder.
//!   The per-block filter/method map itself is LZSS-compressed with a
//!   fixed initial dictionary. Decoding runs the pipeline backwards: LZSS →
//!   Golomb → per-line block decode with the color filter chain and the
//!   MED/average predictors (this also performs the zigzag de-interleave).
//!
//! Pixel byte order: the reference decodes straight into KiriKiri's 32bpp
//! scanline format (0xAARRGGBB in memory R,G,B,A), so the final RGBA8
//! output here is `[R, G, B, A]` with the file's channel 2 holding the
//! red-related residual plane (this matches how the real game files encode
//! their color channels — verified against actual assets).

use image::RgbaImage;

/// TLG5/6 raw-data magic. The reference memcmp's 11 bytes
/// (`"TLG5.0\x00raw\x1a\x00"` — the trailing NUL of the string literal is
/// not part of the comparison).
const TLG5_MAGIC: [u8; 11] = *b"TLG5.0\x00raw\x1a";
const TLG6_MAGIC: [u8; 11] = *b"TLG6.0\x00raw\x1a";
/// TLG0.0 "structured data stream" wrapper magic (raw TLG payload + raw
/// length + optional `tags` chunk).
const SDS_MAGIC: [u8; 11] = *b"TLG0.0\x00sds\x1a";

/// TLG6 block size (reference `TVP_TLG6_W/H_BLOCK_SIZE`).
const TLG6_W_BLOCK_SIZE: usize = 8;
const TLG6_H_BLOCK_SIZE: usize = 8;
/// Golomb adaptation window (reference `TVP_TLG6_GOLOMB_N_COUNT`).
const TLG6_GOLOMB_N_COUNT: usize = 4;
/// Number of entries in the Golomb bit-length table (4 × 2 × 128).
const TLG6_GOLOMB_TABLE_LEN: usize = 1024;

/// Decode TLG5/TLG6 bytes into an RGBA8 image plus whether the file stores
/// an alpha plane (the color count byte: 3 = opaque, 4 = alpha). The
/// descriptor is used by `loadHeader`/`load` to report `bpp`.
pub(crate) fn decode_tlg_with_info(bytes: &[u8]) -> Result<(RgbaImage, bool), String> {
    let raw = strip_sds(bytes)?;
    if raw.len() >= 12 && raw[..11] == TLG5_MAGIC {
        let colors = raw[11];
        return decode_tlg5(raw).map(|img| (img, colors >= 4));
    }
    if raw.len() >= 12 && raw[..11] == TLG6_MAGIC {
        let colors = raw[11];
        return decode_tlg6(raw).map(|img| (img, colors >= 4));
    }
    Err("not a TLG5/TLG6 image (bad magic)".into())
}

/// Content-based detection: true if `bytes` begins with a TLG5/TLG6 magic,
/// directly or inside a TLG0.0 `sds` wrapper. Used by the image pipeline's
/// magic-byte fallback (the `image` crate has no TLG detector), so a TLG file
/// with a wrong or missing extension still decodes.
pub fn has_tlg_magic(bytes: &[u8]) -> bool {
    if bytes.len() >= 11 && bytes[..11] == SDS_MAGIC {
        if bytes.len() < 15 {
            return false;
        }
        let rawlen = i32::from_le_bytes(match bytes[11..15].try_into() {
            Ok(b) => b,
            Err(_) => return false,
        });
        if rawlen < 11 {
            return false;
        }
        let start = 15usize;
        return bytes
            .get(start..start + 11)
            .is_some_and(|raw| raw == TLG5_MAGIC || raw == TLG6_MAGIC);
    }
    bytes.len() >= 11 && (bytes[..11] == TLG5_MAGIC || bytes[..11] == TLG6_MAGIC)
}

/// Header information reported by [`probe_tlg`] without decoding pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TlgHeader {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Whether the file stores an alpha plane.
    pub has_alpha: bool,
    /// Format name: `"tlg5"` or `"tlg6"`.
    pub format: &'static str,
}

/// Parse a TLG5/TLG6 header (or a TLG0.0 `sds` wrapper around one) without
/// decoding pixel data.
pub(crate) fn probe_tlg(bytes: &[u8]) -> Result<TlgHeader, String> {
    let raw = strip_sds(bytes)?;
    if raw.len() >= 12 && raw[..11] == TLG5_MAGIC {
        let colors = raw[11];
        if colors != 3 && colors != 4 {
            return Err(format!("TLG5: unsupported color count {colors}"));
        }
        let (width, height) = read_dims(raw, 12, 16)?;
        return Ok(TlgHeader {
            width: width as u32,
            height: height as u32,
            has_alpha: colors >= 4,
            format: "tlg5",
        });
    }
    if raw.len() >= 12 && raw[..11] == TLG6_MAGIC {
        let colors = raw[11];
        if colors != 1 && colors != 3 && colors != 4 {
            return Err(format!("TLG6: unsupported color count {colors}"));
        }
        let (width, height) = read_dims(raw, 15, 19)?;
        return Ok(TlgHeader {
            width: width as u32,
            height: height as u32,
            has_alpha: colors >= 4,
            format: "tlg6",
        });
    }
    Err("not a TLG5/TLG6 image (bad magic)".into())
}

/// Read and validate a `(width, height)` pair encoded at `woff`/`hoff`.
fn read_dims(raw: &[u8], woff: usize, hoff: usize) -> Result<(usize, usize), String> {
    let width = read_i32(raw, woff)?;
    let height = read_i32(raw, hoff)?;
    let w = usize::try_from(width).map_err(|_| "TLG: negative image width".to_string())?;
    let h = usize::try_from(height).map_err(|_| "TLG: negative image height".to_string())?;
    check_dims(w, h)?;
    Ok((w, h))
}

/// Strip the optional TLG0.0 SDS wrapper, returning the raw TLG payload.
fn strip_sds(bytes: &[u8]) -> Result<&[u8], String> {
    if bytes.len() >= 11 && bytes[..11] == SDS_MAGIC {
        let rawlen = read_i32(bytes, 11)? as usize;
        let start = 15usize;
        let end = start
            .checked_add(rawlen)
            .ok_or("TLG: SDS raw-length overflow")?;
        bytes
            .get(start..end)
            .ok_or_else(|| "TLG: truncated SDS raw payload".to_string())
    } else {
        Ok(bytes)
    }
}

fn read_i32(bytes: &[u8], off: usize) -> Result<i32, String> {
    let b: [u8; 4] = bytes
        .get(off..off + 4)
        .ok_or("TLG: unexpected end of data")?
        .try_into()
        .unwrap();
    Ok(i32::from_le_bytes(b))
}

fn check_dims(w: usize, h: usize) -> Result<(), String> {
    if w == 0 || h == 0 {
        return Err("TLG: zero image dimensions".into());
    }
    // No size policy: the only rejection is arithmetic overflow. A large but
    // well-formed image is decoded (or fails on allocation).
    let pixels = w.checked_mul(h).ok_or("TLG: image size overflow")?;
    pixels.checked_mul(4).ok_or("TLG: image size overflow")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// TLG5
// ---------------------------------------------------------------------------

fn decode_tlg5(raw: &[u8]) -> Result<RgbaImage, String> {
    let colors = *raw.get(11).ok_or("TLG5: truncated header")? as usize;
    if colors != 3 && colors != 4 {
        return Err(format!("TLG5: unsupported color count {colors}"));
    }
    let (width, height) = read_dims(raw, 12, 16)?;
    let blockheight = read_i32(raw, 20)?;
    let blockheight =
        usize::try_from(blockheight).map_err(|_| "TLG5: negative block height".to_string())?;
    if blockheight == 0 || blockheight > height {
        return Err("TLG5: invalid block height".into());
    }
    let blockcount = (height - 1) / blockheight + 1;
    let mut pos = 24usize
        .checked_add(
            blockcount
                .checked_mul(4)
                .ok_or("TLG5: block table size overflow")?,
        )
        .ok_or("TLG5: block table size overflow")?;

    let block_elems = blockheight
        .checked_mul(width)
        .ok_or("TLG5: block size overflow")?;
    let mut text = [0u8; 4096]; // LZSS window, shared across blocks
    let mut r = 0usize;
    let mut outbufs: Vec<Vec<u8>> = vec![vec![0u8; block_elems]; colors];
    let mut img = vec![0u8; width * height * 4];
    let mut rowbuf = vec![0u8; width * 4];
    // LZSS scratch reused across blocks; `lzss_decompress` fills it from the
    // start after each clear.
    let mut tmp = Vec::new();

    for y_blk in (0..height).step_by(blockheight) {
        let y_lim = (y_blk + blockheight).min(height);
        let elems = (y_lim - y_blk) * width;
        for out in outbufs.iter_mut().take(colors) {
            let mark = *raw.get(pos).ok_or("TLG5: truncated block header")?;
            pos += 1;
            let size = read_i32(raw, pos)? as usize;
            pos += 4;
            let data = raw
                .get(pos..pos + size)
                .ok_or("TLG5: truncated block data")?;
            pos += size;
            if mark == 0 {
                // modified-LZSS compressed
                tmp.clear();
                lzss_decompress(data, &mut text, &mut r, &mut tmp, elems)?;
                if tmp.len() < elems {
                    return Err("TLG5: LZSS stream underrun".into());
                }
                out[..elems].copy_from_slice(&tmp[..elems]);
            } else {
                // raw data
                if data.len() < elems {
                    return Err("TLG5: raw block underrun".into());
                }
                out[..elems].copy_from_slice(&data[..elems]);
            }
        }
        // Compose rows. Residual chain (reference `TVPTLG5ComposeColors*`):
        //   R = Σ(plane2 + plane1) + upperR, G = Σ(plane1) + upperG,
        //   B = Σ(plane0 + plane1) + upperB, A = Σ(plane3) + upperA.
        for y in y_blk..y_lim {
            let o = (y - y_blk) * width;
            let mut pc = [0u32; 4];
            let upper = if y > 0 {
                &img[(y - 1) * width * 4..]
            } else {
                &[]
            };
            for x in 0..width {
                let g = outbufs[1][o + x] as u32;
                let b = outbufs[0][o + x] as u32; // plane0
                let r = outbufs[2][o + x] as u32; // plane2
                let vals = [
                    (r + g) & 0xff, // red  = plane2 + plane1
                    g,              // green = plane1
                    (b + g) & 0xff, // blue = plane0 + plane1
                ];
                for (k, v) in vals.into_iter().enumerate() {
                    pc[k] = (pc[k] + v) & 0xff;
                    let upper_v = if y > 0 { upper[x * 4 + k] as u32 } else { 0 };
                    rowbuf[x * 4 + k] = ((pc[k] + upper_v) & 0xff) as u8;
                }
                if colors == 4 {
                    pc[3] = (pc[3] + outbufs[3][o + x] as u32) & 0xff;
                    let upper_v = if y > 0 { upper[x * 4 + 3] as u32 } else { 0 };
                    rowbuf[x * 4 + 3] = ((pc[3] + upper_v) & 0xff) as u8;
                } else {
                    // 24-bit TLG5 has no alpha plane: opaque (reference
                    // writes the 0xff000000 constant).
                    rowbuf[x * 4 + 3] = 0xff;
                }
            }
            img[y * width * 4..(y + 1) * width * 4].copy_from_slice(&rowbuf);
        }
    }

    RgbaImage::from_raw(width as u32, height as u32, img)
        .ok_or("TLG5: invalid image dimensions".into())
}

// ---------------------------------------------------------------------------
// TLG6
// ---------------------------------------------------------------------------

fn decode_tlg6(raw: &[u8]) -> Result<RgbaImage, String> {
    let colors = *raw.get(11).ok_or("TLG6: truncated header")? as usize;
    if colors != 1 && colors != 3 && colors != 4 {
        return Err(format!("TLG6: unsupported color count {colors}"));
    }
    // data flag, color type and external golomb table must all be zero
    for k in 0..3 {
        if raw.get(12 + k) != Some(&0) {
            return Err("TLG6: nonzero reserved header byte".into());
        }
    }
    let (width, height) = read_dims(raw, 15, 19)?;
    // max_bit_length (offset 23) is only used for allocation in the
    // reference; we read bit pools per chunk instead.

    let x_block_count = (width - 1) / TLG6_W_BLOCK_SIZE + 1;
    let y_block_count = (height - 1) / TLG6_H_BLOCK_SIZE + 1;
    let main_count = width / TLG6_W_BLOCK_SIZE;
    let fraction = width - main_count * TLG6_W_BLOCK_SIZE;

    // Per-block filter/method map, LZSS-compressed with a fixed initial
    // dictionary (32×16 pattern, reference `TLG6InitializeColorFilterCompressor`).
    let mut pos = 27usize;
    let ft_size = read_i32(raw, pos)?;
    pos += 4;
    let ft_size =
        usize::try_from(ft_size).map_err(|_| "TLG6: negative filter-type size".to_string())?;
    let ft_end = pos
        .checked_add(ft_size)
        .ok_or("TLG6: filter-type size overflow")?;
    let ft_bytes = raw
        .get(pos..ft_end)
        .ok_or("TLG6: truncated filter-type data")?;
    pos = ft_end;
    let mut ft_text = [0u8; 4096];
    for i in 0..32usize {
        for j in 0..16usize {
            let base = (i * 16 + j) * 8;
            ft_text[base..base + 4].fill(i as u8);
            ft_text[base + 4..base + 8].fill(j as u8);
        }
    }
    let mut r = 0usize;
    let mut filter_types = Vec::with_capacity(x_block_count * y_block_count);
    lzss_decompress(
        ft_bytes,
        &mut ft_text,
        &mut r,
        &mut filter_types,
        x_block_count * y_block_count,
    )?;
    if filter_types.len() < x_block_count * y_block_count {
        return Err("TLG6: filter-type stream underrun".into());
    }

    let lzt = &LEADING_ZERO_TABLE;
    let gbt = &GOLOMB_BIT_LENGTH_TABLE;

    let zeroline_val: u32 = if colors == 3 { 0xff00_0000 } else { 0 };
    let mut prevline = vec![zeroline_val; width];
    let mut img = vec![0u8; width * height * 4];
    // Scratch reused across block rows. A block row is at most
    // `TLG6_H_BLOCK_SIZE` scanlines; every byte read below is either written
    // by one of the color lanes or stays at the zeroed initial value.
    let band_pixels = TLG6_H_BLOCK_SIZE * width;
    let mut pixelbuf = vec![0u32; band_pixels];
    let mut curline = vec![0u32; width];
    let mut lane_buf = vec![0u8; band_pixels * 4];

    for y in (0..height).step_by(TLG6_H_BLOCK_SIZE) {
        let ylim = (y + TLG6_H_BLOCK_SIZE).min(height);
        let pixel_count = (ylim - y) * width;
        let lane_len = pixel_count * 4;
        let skipbytes = (ylim - y) * TLG6_W_BLOCK_SIZE;

        // Decode each channel's residual lane into a per-pixel byte lane.
        for c in 0..colors {
            let bit_length = read_i32(raw, pos)? as u32;
            pos += 4;
            let method = (bit_length >> 30) & 3;
            if method != 0 {
                return Err("TLG6: unsupported entropy coding method".into());
            }
            let bits = bit_length & 0x3fff_ffff;
            let byte_len = (bits as usize).div_ceil(8);
            let pool = raw
                .get(pos..pos + byte_len)
                .ok_or("TLG6: truncated bit pool")?;
            pos += byte_len;
            decode_golomb_lane(
                pool,
                pixel_count,
                c,
                c == 0 && colors != 1,
                lzt,
                gbt,
                &mut lane_buf[..lane_len],
            )?;
            for (i, g) in lane_buf[..lane_len].chunks(4).enumerate() {
                let v = pixelbuf[i];
                let b = v.to_le_bytes();
                let mut nb = b;
                nb[c] = g[c];
                pixelbuf[i] = u32::from_le_bytes(nb);
            }
        }

        // Per-line block decode (reference `TVPTLG6DecodeLineGeneric`).
        let ft_base = (y / TLG6_H_BLOCK_SIZE) * x_block_count;
        let initialp = if colors == 3 { 0xff00_0000 } else { 0 };
        for yy in y..ylim {
            let dir = (yy & 1) ^ 1;
            let oddskip = (ylim - yy - 1) as isize - (yy - y) as isize;
            if main_count > 0 {
                let start = width.min(TLG6_W_BLOCK_SIZE) * (yy - y);
                decode_line(
                    &prevline,
                    &mut curline,
                    width,
                    0,
                    main_count,
                    &filter_types,
                    ft_base,
                    skipbytes,
                    &pixelbuf[..pixel_count],
                    start,
                    initialp,
                    oddskip,
                    dir,
                )?;
            }
            if main_count != x_block_count {
                let ww = fraction.min(TLG6_W_BLOCK_SIZE);
                let start = ww * (yy - y);
                decode_line(
                    &prevline,
                    &mut curline,
                    width,
                    main_count,
                    x_block_count,
                    &filter_types,
                    ft_base,
                    skipbytes,
                    &pixelbuf[..pixel_count],
                    start,
                    initialp,
                    oddskip,
                    dir,
                )?;
            }
            let row = &mut img[yy * width * 4..(yy + 1) * width * 4];
            for (x, &px) in curline.iter().enumerate() {
                let b = px.to_le_bytes();
                row[x * 4] = b[0];
                row[x * 4 + 1] = b[1];
                row[x * 4 + 2] = b[2];
                row[x * 4 + 3] = b[3];
            }
            prevline.copy_from_slice(&curline);
        }
    }

    if colors == 1 {
        // Grayscale TLG6: the reference stores the luminance in the first
        // byte lane. Present it as proper grayscale with opaque alpha.
        for px in img.chunks_exact_mut(4) {
            let v = px[0];
            px[1] = v;
            px[2] = v;
            px[3] = 0xff;
        }
    }

    RgbaImage::from_raw(width as u32, height as u32, img)
        .ok_or("TLG6: invalid image dimensions".into())
}

/// Decode one scanline's worth of 8-wide blocks (reference
/// `TVPTLG6DecodeLineGeneric`): reads the residual lanes out of `pixelbuf`
/// in the encoder's zigzag order and runs the color filter + MED/average
/// predictor chain, writing RGBA pixels (in memory order R,G,B,A — byte 0
/// of each `u32` is the red-related plane).
#[allow(clippy::too_many_arguments)]
fn decode_line(
    prevline: &[u32],
    curline: &mut [u32],
    width: usize,
    start_block: usize,
    block_limit: usize,
    filter_types: &[u8],
    ft_base: usize,
    skipbytes: usize,
    pixelbuf: &[u32],
    in_start: usize,
    initialp: u32,
    oddskip: isize,
    dir: usize,
) -> Result<(), String> {
    let step: isize = if dir & 1 == 1 { 1 } else { -1 };
    let (mut p, mut up);
    if start_block > 0 {
        let prev = start_block * TLG6_W_BLOCK_SIZE - 1;
        p = *curline.get(prev).ok_or("TLG6: block index out of range")?;
        up = *prevline.get(prev).ok_or("TLG6: block index out of range")?;
    } else {
        p = initialp;
        up = initialp;
    }
    let mut inpos = in_start as isize + (skipbytes * start_block) as isize;
    for i in start_block..block_limit {
        let mut w = width - i * TLG6_W_BLOCK_SIZE;
        if w > TLG6_W_BLOCK_SIZE {
            w = TLG6_W_BLOCK_SIZE;
        }
        let ww = w as isize;
        if step == -1 {
            inpos += ww - 1;
        }
        if i & 1 == 1 {
            inpos += oddskip * ww;
        }
        let ft = *filter_types
            .get(ft_base + i)
            .ok_or("TLG6: filter map out of range")?;
        let code = ft >> 1;
        if code >= 16 {
            return Err("TLG6: unknown color filter".into());
        }
        let avg_mode = ft & 1 == 1;
        for x in (i * TLG6_W_BLOCK_SIZE..).take(w) {
            let idx = usize::try_from(inpos).map_err(|_| "TLG6: block index out of range")?;
            let err = pixelbuf
                .get(idx)
                .ok_or("TLG6: block data out of range")?
                .to_le_bytes();
            let (eb, eg, er) = color_filter(code, err[0] as i8, err[1] as i8, err[2] as i8);
            let ea = err[3] as i8;
            let u = prevline[x];
            let pb = p.to_le_bytes();
            let ub = u.to_le_bytes();
            let upb = up.to_le_bytes();
            let mut ob = [0u8; 4];
            // Byte order of the packed pixel is [R, G, B, A] (memory
            // order), matching the reference's `0xff0000 & (B << 16) |
            // 0xff00 & (G << 8) | R | (A << 24)` packed value.
            let errb = [er as u8, eg as u8, eb as u8, ea as u8];
            for c in 0..4 {
                let pred = if avg_mode {
                    avg_byte(pb[c], ub[c])
                } else {
                    med2_byte(pb[c], ub[c], upb[c])
                };
                ob[c] = pred.wrapping_add(errb[c]);
            }
            p = u32::from_le_bytes(ob);
            up = u;
            curline[x] = p;
            inpos += step;
        }
        if step == 1 {
            inpos += skipbytes as isize - ww;
        } else {
            inpos += skipbytes as isize + 1;
        }
        if i & 1 == 1 {
            inpos -= oddskip * ww;
        }
    }
    Ok(())
}

/// Apply one of the 16 color-correlation filters to the residual bytes
/// (reference `TVP_TLG6_DO_CHROMA_DECODE*`; all arithmetic is wrapping 8-bit
/// signed). Returns the (B, G, R) corrections.
fn color_filter(code: u8, ib: i8, ig: i8, ir: i8) -> (i8, i8, i8) {
    let a = |x: i8, y: i8| x.wrapping_add(y);
    match code {
        0 => (ib, ig, ir),
        1 => (a(ib, ig), ig, a(ir, ig)),
        2 => (ib, a(ig, ib), a(a(ir, ib), ig)),
        3 => (a(a(ib, ir), ig), a(ig, ir), ir),
        4 => (a(ib, ir), a(a(ig, ib), ir), a(a(a(ir, ib), ir), ig)),
        5 => (a(ib, ir), a(a(ig, ib), ir), ir),
        6 => (a(ib, ig), ig, ir),
        7 => (ib, a(ig, ib), ir),
        8 => (ib, ig, a(ir, ig)),
        9 => (a(a(a(ib, ig), ir), ib), a(a(ig, ir), ib), a(ir, ib)),
        10 => (a(ib, ir), a(ig, ir), ir),
        11 => (ib, a(ig, ib), a(ir, ib)),
        12 => (ib, a(a(ig, ir), ib), a(ir, ib)),
        13 => (a(ib, ig), a(a(a(ig, ir), ib), ig), a(a(ir, ib), ig)),
        14 => (a(a(ib, ig), ir), a(ig, ir), a(a(a(ir, ib), ig), ir)),
        15 => (ib, a(ig, ib.wrapping_mul(2)), a(ir, ib.wrapping_mul(2))),
        _ => unreachable!("filter code checked by caller"),
    }
}

/// Per-byte MED (Median Edge Detector / LOCO-I) predictor.
fn med2_byte(a: u8, b: u8, c: u8) -> u8 {
    let (mn, mx) = if a < b { (a, b) } else { (b, a) };
    if c >= mx {
        mn
    } else if c < mn {
        mx
    } else {
        a.wrapping_add(b).wrapping_sub(c)
    }
}

/// Per-byte "average" predictor: (a + b + 1) >> 1.
fn avg_byte(a: u8, b: u8) -> u8 {
    (((a as u16) + (b as u16) + 1) >> 1) as u8
}

// ---------------------------------------------------------------------------
// Modified-LZSS (reference `TVPTLG5DecompressSlide`)
// ---------------------------------------------------------------------------

/// Decompress a modified-LZSS stream into `out` (at most `out_len` bytes).
/// `text` (4096-byte window) and `r` (write position) carry state across
/// calls, exactly like the reference's per-image TLG5 state.
fn lzss_decompress(
    inp: &[u8],
    text: &mut [u8; 4096],
    r: &mut usize,
    out: &mut Vec<u8>,
    out_len: usize,
) -> Result<(), String> {
    let inlim = inp.len();
    let mut ipos = 0usize;
    let mut flags: u32 = 0;
    while ipos < inlim && out.len() < out_len {
        flags >>= 1;
        if (flags & 0x100) == 0 {
            flags = u32::from(*inp.get(ipos).ok_or("TLG: LZSS stream underrun")?) | 0xff00;
            ipos += 1;
            if flags == 0xff00 && *r < 4096 - 8 && ipos + 8 <= inlim {
                // raw 8-byte copy
                for _ in 0..8 {
                    let c = inp[ipos];
                    ipos += 1;
                    out.push(c);
                    text[*r] = c;
                    *r += 1;
                }
                flags = 0;
                continue;
            }
        }
        if flags & 1 != 0 {
            // LZSS match
            let b0 = *inp.get(ipos).ok_or("TLG: LZSS stream underrun")? as u32;
            let b1 = *inp.get(ipos + 1).ok_or("TLG: LZSS stream underrun")? as u32;
            ipos += 2;
            let mpos = (b0 | ((b1 & 0x0f) << 8)) as usize;
            let mut mlen = ((b1 & 0xf0) >> 4) as usize + 3;
            if mlen == 18 {
                mlen += *inp.get(ipos).ok_or("TLG: LZSS stream underrun")? as usize;
                ipos += 1;
            }
            for m in 0..mlen {
                if out.len() >= out_len {
                    break;
                }
                let c = text[(mpos + m) & 0xfff];
                out.push(c);
                text[*r & 0xfff] = c;
                *r += 1;
            }
        } else {
            // literal
            let c = *inp.get(ipos).ok_or("TLG: LZSS stream underrun")?;
            ipos += 1;
            out.push(c);
            text[*r & 0xfff] = c;
            *r += 1;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// TLG6 Golomb-Rice entropy decoding
// ---------------------------------------------------------------------------

/// Bit-level reader over a (zero-padded) byte pool. Mirrors the reference's
/// `bit_pool` byte pointer + `bit_pos` (LSB-first).
struct GolombBits<'a> {
    pool: &'a [u8],
    pos: usize,
    bit: u32,
}

impl GolombBits<'_> {
    fn fetch32(&self) -> u32 {
        if self.pos >= self.pool.len() {
            return 0;
        }
        let mut b = [0u8; 4];
        let n = (self.pool.len() - self.pos).min(4);
        b[..n].copy_from_slice(&self.pool[self.pos..self.pos + n]);
        u32::from_le_bytes(b)
    }
    fn window(&self) -> u32 {
        self.fetch32() >> self.bit
    }
    fn sync(&mut self) {
        self.pos += (self.bit >> 3) as usize;
        self.bit &= 7;
    }
    fn exhausted(&self) -> bool {
        self.pos > self.pool.len() + 4
    }
}

/// Decode one channel's residual lane (reference
/// `TVPTLG6DecodeGolombValues[ForFirst]`). The stream alternates zero/nonzero
/// run lengths (Elias-gamma coded), non-zero values use Golomb-Rice coding
/// with an adaptive `k` from the precomputed table. Writes the channel's byte
/// into `out`, which holds `pixel_count * 4` bytes.
fn decode_golomb_lane(
    pool: &[u8],
    pixel_count: usize,
    lane: usize,
    first: bool,
    lzt: &[u8; 4096],
    gbt: &[[u8; 4]; TLG6_GOLOMB_TABLE_LEN],
    out: &mut [u8],
) -> Result<(), String> {
    if pool.is_empty() {
        return Err("TLG6: empty bit pool".into());
    }
    let mut bs = GolombBits {
        pool,
        pos: 0,
        bit: 1,
    };
    let mut zero = if pool[0] & 1 != 0 { 0u8 } else { 1u8 };
    let mut n = TLG6_GOLOMB_N_COUNT - 1;
    let mut a = 0usize;
    let mut p = 0usize;
    while p < pixel_count {
        if bs.exhausted() {
            return Err("TLG6: bit pool exhausted".into());
        }
        // run length (Elias gamma)
        let mut t = bs.window();
        let mut b = lzt[(t & 0xfff) as usize] as u32;
        let mut bit_count = b;
        while b == 0 {
            bit_count += 12;
            bs.bit += 12;
            bs.sync();
            if bs.exhausted() {
                return Err("TLG6: bit pool exhausted".into());
            }
            t = bs.window();
            b = lzt[(t & 0xfff) as usize] as u32;
            bit_count += b;
        }
        bs.bit += b;
        bs.sync();
        bit_count -= 1;
        let bc = bit_count.min(24);
        let mut count = (1u32 << bc) as usize;
        let mask = count - 1;
        count += (bs.window() as usize) & mask;
        bs.bit += bc;
        bs.sync();
        let count = count.min(pixel_count - p);

        if zero != 0 {
            for _ in 0..count {
                if first {
                    out[p * 4..p * 4 + 4].fill(0);
                } else {
                    out[p * 4 + lane] = 0;
                }
                p += 1;
            }
        } else {
            for _ in 0..count {
                if bs.exhausted() {
                    return Err("TLG6: bit pool exhausted".into());
                }
                let k = gbt[a][n] as u32;
                let (val, half) = read_golomb_value(&mut bs, k, lzt);
                a = (a + half as usize).min(TLG6_GOLOMB_TABLE_LEN - 1);
                if first {
                    out[p * 4] = val as u8;
                } else {
                    out[p * 4 + lane] = val as u8;
                }
                p += 1;
                if n == 0 {
                    a >>= 1;
                    n = TLG6_GOLOMB_N_COUNT - 1;
                } else {
                    n -= 1;
                }
            }
        }
        zero ^= 1;
    }
    Ok(())
}

/// Decode one Golomb-Rice value; returns the signed residual and `m >> 1`
/// (used by the `a` accumulator).
fn read_golomb_value(bs: &mut GolombBits, k: u32, lzt: &[u8; 4096]) -> (i8, u32) {
    let mut t = bs.window();
    let (mut b, mut bit_count);
    if t != 0 {
        b = lzt[(t & 0xfff) as usize] as u32;
        bit_count = b;
        while b == 0 {
            bit_count += 12;
            bs.bit += 12;
            bs.sync();
            if bs.exhausted() {
                // Ran past the end of the pool: stop reading (the caller
                // fills the remainder with zeros).
                b = 1;
                break;
            }
            t = bs.window();
            b = lzt[(t & 0xfff) as usize] as u32;
            bit_count += b;
        }
        bit_count -= 1;
    } else {
        // Long unary run: the reference re-syncs to a byte-aligned escape
        // (used when the encoder's 4-byte "give up" limit was hit).
        bs.pos = bs.pos.saturating_add(5);
        bit_count = u32::from(*bs.pool.get(bs.pos.wrapping_sub(1)).unwrap_or(&0));
        bs.bit = 0;
        t = bs.window();
        b = 0;
    }
    let v = (i64::from(bit_count) << k) + i64::from((t >> b) & ((1u32 << k.min(8)) - 1));
    let sign = (v & 1) - 1;
    let half = v >> 1;
    let val = ((half ^ sign) + sign + 1) as i8;
    bs.bit += b + k;
    bs.sync();
    (val, half.min(i64::from(u32::MAX)) as u32)
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

/// Expanded leading-zero table (reference `TVPTLG6LeadingZeroTable`): for the
/// low 12 bits, position of the lowest set bit + 1, or 0 if all zero.
///
/// Computed at compile time as a `const`, so the decoder needs no lazy
/// initialization and no process-global mutable state.
const LEADING_ZERO_TABLE: [u8; 4096] = build_leading_zero_table();

const fn build_leading_zero_table() -> [u8; 4096] {
    let mut table = [0u8; 4096];
    let mut i = 0usize;
    while i < 4096 {
        let mut count = 0u8;
        let mut j = 1usize;
        while j != 4096 && (i & j) == 0 {
            j <<= 1;
            count += 1;
        }
        count += 1;
        if j == 4096 {
            count = 0;
        }
        table[i] = count;
        i += 1;
    }
    table
}

/// Compressed run-length form of the Golomb bit-length table (reference
/// `TVPTLG6GolombBitLengthTable`).
const GOLOMB_COMPRESSED: [[u16; 9]; 4] = [
    [3, 7, 15, 27, 63, 108, 223, 448, 130],
    [3, 5, 13, 24, 51, 95, 192, 384, 257],
    [2, 5, 12, 21, 39, 86, 155, 320, 384],
    [2, 3, 9, 18, 33, 61, 129, 258, 511],
];

/// Expanded Golomb bit-length table, computed at compile time (no lazy
/// initialization, no global mutable state).
const GOLOMB_BIT_LENGTH_TABLE: [[u8; 4]; TLG6_GOLOMB_TABLE_LEN] = build_golomb_bit_length_table();

const fn build_golomb_bit_length_table() -> [[u8; 4]; TLG6_GOLOMB_TABLE_LEN] {
    let mut table = [[0u8; 4]; TLG6_GOLOMB_TABLE_LEN];
    let mut n = 0usize;
    while n < 4 {
        let mut a = 0usize;
        let mut i = 0usize;
        while i < 9 {
            let count = GOLOMB_COMPRESSED[n][i];
            let mut c = 0u16;
            while c < count {
                table[a][n] = i as u8;
                a += 1;
                c += 1;
            }
            i += 1;
        }
        n += 1;
    }
    table
}
