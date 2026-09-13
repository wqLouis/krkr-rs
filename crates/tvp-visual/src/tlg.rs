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
use std::sync::OnceLock;

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
/// Cap on decoded image dimensions (defensive; real TLG files are small).
const MAX_DIM: usize = 1 << 15;
const MAX_PIXEL_BYTES: usize = 1 << 28;

/// Decode TLG5/TLG6 bytes into an RGBA8 image.
pub fn decode_tlg(bytes: &[u8]) -> Result<RgbaImage, String> {
    decode_tlg_with_info(bytes).map(|(img, _)| img)
}

/// Decode TLG5/TLG6 bytes into an RGBA8 image plus whether the file stores
/// an alpha plane (the color count byte: 3 = opaque, 4 = alpha). The
/// descriptor is used by `loadHeader`/`load` to report `bpp`.
pub fn decode_tlg_with_info(bytes: &[u8]) -> Result<(RgbaImage, bool), String> {
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
    if w > MAX_DIM || h > MAX_DIM {
        return Err("TLG: image dimensions too large".into());
    }
    let pixels = w.checked_mul(h).ok_or("TLG: image size overflow")?;
    let bytes = pixels.checked_mul(4).ok_or("TLG: image size overflow")?;
    if bytes > MAX_PIXEL_BYTES {
        return Err("TLG: image too large".into());
    }
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
    let width = read_i32(raw, 12)? as usize;
    let height = read_i32(raw, 16)? as usize;
    let blockheight = read_i32(raw, 20)? as usize;
    check_dims(width, height)?;
    if blockheight == 0 {
        return Err("TLG5: zero block height".into());
    }
    let blockcount = (height - 1) / blockheight + 1;
    let mut pos = 24 + blockcount * 4; // skip the block-size table

    let block_elems = blockheight * width;
    let mut text = [0u8; 4096]; // LZSS window, shared across blocks
    let mut r = 0usize;
    let mut outbufs: Vec<Vec<u8>> = vec![vec![0u8; block_elems]; colors];
    let mut img = vec![0u8; width * height * 4];
    let mut rowbuf = vec![0u8; width * 4];

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
                let mut tmp = Vec::with_capacity(elems);
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
    let width = read_i32(raw, 15)? as usize;
    let height = read_i32(raw, 19)? as usize;
    check_dims(width, height)?;
    // max_bit_length (offset 23) is only used for allocation in the
    // reference; we read bit pools per chunk instead.

    let x_block_count = (width - 1) / TLG6_W_BLOCK_SIZE + 1;
    let y_block_count = (height - 1) / TLG6_H_BLOCK_SIZE + 1;
    let main_count = width / TLG6_W_BLOCK_SIZE;
    let fraction = width - main_count * TLG6_W_BLOCK_SIZE;

    // Per-block filter/method map, LZSS-compressed with a fixed initial
    // dictionary (32×16 pattern, reference `TLG6InitializeColorFilterCompressor`).
    let mut pos = 27usize;
    let ft_size = read_i32(raw, pos)? as usize;
    pos += 4;
    let ft_bytes = raw
        .get(pos..pos + ft_size)
        .ok_or("TLG6: truncated filter-type data")?;
    pos += ft_size;
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

    let lzt = leading_zero_table();
    let gbt = golomb_bit_length_table();

    let zeroline_val: u32 = if colors == 3 { 0xff00_0000 } else { 0 };
    let mut prevline = vec![zeroline_val; width];
    let mut img = vec![0u8; width * height * 4];

    for y in (0..height).step_by(TLG6_H_BLOCK_SIZE) {
        let ylim = (y + TLG6_H_BLOCK_SIZE).min(height);
        let pixel_count = (ylim - y) * width;
        let skipbytes = (ylim - y) * TLG6_W_BLOCK_SIZE;

        // Decode each channel's residual lane into a per-pixel byte lane.
        let mut pixelbuf = vec![0u32; pixel_count];
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
            let lane = decode_golomb_lane(pool, pixel_count, c, c == 0 && colors != 1, lzt, gbt)?;
            for (i, g) in lane.chunks(4).enumerate() {
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
        let mut curline = vec![0u32; width];
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
                    &pixelbuf,
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
                    &pixelbuf,
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
/// with an adaptive `k` from the precomputed table.
fn decode_golomb_lane(
    pool: &[u8],
    pixel_count: usize,
    lane: usize,
    first: bool,
    lzt: &[u8; 4096],
    gbt: &[[u8; 4]; TLG6_GOLOMB_TABLE_LEN],
) -> Result<Vec<u8>, String> {
    if pool.is_empty() {
        return Err("TLG6: empty bit pool".into());
    }
    let mut out = vec![0u8; pixel_count * 4];
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
    Ok(out)
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

/// Leading-zero table (reference `TVPTLG6LeadingZeroTable`): for the low 12
/// bits, position of the lowest set bit + 1, or 0 if all zero.
fn leading_zero_table() -> &'static [u8; 4096] {
    static TABLE: OnceLock<[u8; 4096]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [0u8; 4096];
        for (i, e) in t.iter_mut().enumerate() {
            let mut cnt = 0u8;
            let mut j = 1usize;
            while j != 4096 && (i & j) == 0 {
                j <<= 1;
                cnt += 1;
            }
            cnt += 1;
            if j == 4096 {
                cnt = 0;
            }
            *e = cnt;
        }
        t
    })
}

/// Golomb bit-length table (reference `TVPTLG6GolombBitLengthTable`),
/// expanded from the compressed run-length representation.
fn golomb_bit_length_table() -> &'static [[u8; 4]; TLG6_GOLOMB_TABLE_LEN] {
    static TABLE: OnceLock<[[u8; 4]; TLG6_GOLOMB_TABLE_LEN]> = OnceLock::new();
    TABLE.get_or_init(|| {
        const COMPRESSED: [[u16; 9]; 4] = [
            [3, 7, 15, 27, 63, 108, 223, 448, 130],
            [3, 5, 13, 24, 51, 95, 192, 384, 257],
            [2, 5, 12, 21, 39, 86, 155, 320, 384],
            [2, 3, 9, 18, 33, 61, 129, 258, 511],
        ];
        let mut table = [[0u8; 4]; TLG6_GOLOMB_TABLE_LEN];
        for (n, row) in COMPRESSED.iter().enumerate() {
            let mut a = 0usize;
            for (i, &count) in row.iter().enumerate() {
                for _ in 0..count {
                    table[a][n] = i as u8;
                    a += 1;
                }
            }
            debug_assert_eq!(a, TLG6_GOLOMB_TABLE_LEN);
        }
        table
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // Decoder self-checks against the reference (Python port) on the real
    // game files. FNV-1a 64-bit checksums of the expected RGBA8 output
    // (computed with an independent reference-faithful decoder).
    // ------------------------------------------------------------------

    #[test]
    fn real_frm_0303a_decodes_like_reference() {
        let bytes = include_bytes!("../tests/fixtures/frm_0303a.tlg");
        let img = decode_tlg(bytes).expect("frm_0303a should decode");
        assert_eq!((img.width(), img.height()), (280, 200));
        assert!(
            img.as_raw().iter().any(|&a| a != 0),
            "image should not be empty"
        );
        // all 56_000 pixels have a non-zero alpha channel
        let alpha = img.as_raw().chunks_exact(4).filter(|p| p[3] != 0).count();
        assert_eq!(alpha, 280 * 200);
        assert_eq!(fnv1a(img.as_raw()), 0x2490_6a69_6669_fdbe);
    }

    #[test]
    fn real_frm_0303b_decodes_like_reference() {
        let bytes = include_bytes!("../tests/fixtures/frm_0303b.tlg");
        let img = decode_tlg(bytes).expect("frm_0303b should decode");
        assert_eq!((img.width(), img.height()), (280, 200));
        let alpha = img.as_raw().chunks_exact(4).filter(|p| p[3] != 0).count();
        assert_eq!(alpha, 280 * 200);
        assert_eq!(fnv1a(img.as_raw()), 0xb9cf_76f3_7184_8a40);
    }

    #[test]
    fn sds_wrapper_and_raw_payload_are_equivalent() {
        let bytes = include_bytes!("../tests/fixtures/frm_0303a.tlg");
        let img_wrapped = decode_tlg(bytes).unwrap();
        // strip the SDS wrapper manually and decode the raw payload
        let rawlen = i32::from_le_bytes(bytes[11..15].try_into().unwrap()) as usize;
        let raw = &bytes[15..15 + rawlen];
        assert_eq!(&raw[..11], &TLG6_MAGIC);
        let img_raw = decode_tlg(raw).unwrap();
        assert_eq!(img_wrapped.as_raw(), img_raw.as_raw());
    }

    // ------------------------------------------------------------------
    // Synthetic round trips (encoder + decoder)
    // ------------------------------------------------------------------

    #[test]
    fn tlg6_rgba_roundtrip_all_filters() {
        let (w, h) = (64usize, 32usize);
        let img = test_image(w, h);
        let encoded = encode_tlg6(&img, w, h, 4);
        let decoded = decode_tlg(&encoded).expect("synthetic TLG6 should decode");
        assert_eq!(
            (decoded.width() as usize, decoded.height() as usize),
            (w, h)
        );
        assert_eq!(decoded.as_raw(), &img, "TLG6 round trip must be lossless");
    }

    #[test]
    fn tlg6_rgb_roundtrip_all_filters() {
        let (w, h) = (40usize, 24usize);
        let mut img = test_image(w, h);
        for px in img.chunks_exact_mut(4) {
            px[3] = 0xff; // opaque 24-bit image
        }
        let encoded = encode_tlg6(&img, w, h, 3);
        let decoded = decode_tlg(&encoded).expect("synthetic 24-bit TLG6 should decode");
        assert_eq!(
            decoded.as_raw(),
            &img,
            "24-bit TLG6 round trip must be lossless"
        );
    }

    #[test]
    fn tlg6_narrow_width_roundtrip() {
        // width not a multiple of 8 (exercises the fractional block path)
        let (w, h) = (13usize, 17usize);
        let img = test_image(w, h);
        let encoded = encode_tlg6(&img, w, h, 4);
        let decoded = decode_tlg(&encoded).expect("narrow TLG6 should decode");
        assert_eq!(
            decoded.as_raw(),
            &img,
            "narrow TLG6 round trip must be lossless"
        );
    }

    #[test]
    fn tlg5_rgba_roundtrip() {
        let (w, h) = (24usize, 16usize);
        let img = test_image(w, h);
        let encoded = encode_tlg5(&img, w, h, 4, 4);
        let decoded = decode_tlg(&encoded).expect("synthetic TLG5 should decode");
        assert_eq!(
            (decoded.width() as usize, decoded.height() as usize),
            (w, h)
        );
        assert_eq!(decoded.as_raw(), &img, "TLG5 round trip must be lossless");
    }

    #[test]
    fn tlg5_rgb_roundtrip() {
        let (w, h) = (21usize, 19usize);
        let mut img = test_image(w, h);
        for px in img.chunks_exact_mut(4) {
            px[3] = 0xff;
        }
        let encoded = encode_tlg5(&img, w, h, 3, 4);
        let decoded = decode_tlg(&encoded).expect("synthetic 24-bit TLG5 should decode");
        assert_eq!(
            decoded.as_raw(),
            &img,
            "24-bit TLG5 round trip must be lossless"
        );
    }

    // ------------------------------------------------------------------
    // Error handling: garbage must return Err, never panic
    // ------------------------------------------------------------------

    #[test]
    fn garbage_inputs_return_err() {
        let cases: Vec<Vec<u8>> = vec![
            vec![],
            b"TLG6.0\x00raw\x1a".to_vec(), // header only
            b"TLG5.0\x00raw\x1a\x04\x00\x00\x00".to_vec(), // truncated TLG5
            b"TLG6.0\x00raw\x1a\x04\x00\x00\x00".to_vec(), // truncated TLG6
            b"TLG6.0\x00raw\x1a\x05\x00\x00\x00".to_vec(), // bad color count
            b"RIFF\x00\x00\x00\x00WEBPVP8 ".to_vec(), // webp masquerading as tlg
            b"GIF89a".to_vec(),            // wrong magic
            b"TLG0.0\x00sds\x1a\xff\xff\xff\x7f".to_vec(), // SDS with absurd raw length
            b"TLG0.0\x00sds\x1a\x00\x00\x00\x00".to_vec(), // SDS with no payload
            {
                // TLG6 with huge dimensions
                let mut v = TLG6_MAGIC.to_vec();
                v.extend_from_slice(&[4, 0, 0, 0]);
                v.extend_from_slice(&(1i32 << 30).to_le_bytes());
                v.extend_from_slice(&(1i32 << 30).to_le_bytes());
                v.extend_from_slice(&0i32.to_le_bytes());
                v
            },
            {
                // valid header but truncated filter-type section
                let mut v = TLG6_MAGIC.to_vec();
                v.extend_from_slice(&[4, 0, 0, 0]);
                v.extend_from_slice(&8i32.to_le_bytes());
                v.extend_from_slice(&8i32.to_le_bytes());
                v.extend_from_slice(&0i32.to_le_bytes());
                v.extend_from_slice(&100i32.to_le_bytes()); // filter size larger than file
                v
            },
        ];
        for (i, c) in cases.iter().enumerate() {
            assert!(decode_tlg(c).is_err(), "case {i} should fail");
        }
        // random bytes of various lengths must not panic
        let mut seed = 0x1234_5678u64;
        for len in [1usize, 5, 11, 64, 1024] {
            let mut v = Vec::with_capacity(len);
            for _ in 0..len {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                v.push((seed >> 33) as u8);
            }
            let _ = decode_tlg(&v);
        }
    }

    // ------------------------------------------------------------------
    // FNV-1a 64 (independent of the image crate)
    // ------------------------------------------------------------------

    fn fnv1a(data: &[u8]) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for &b in data {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }

    /// Deterministic test image: left half gradient + noise, right half
    /// flat colour blocks (flat regions exercise the zero-run lengths).
    fn test_image(w: usize, h: usize) -> Vec<u8> {
        let mut img = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                if x < w / 2 {
                    img[i] = (x * 7 + y * 3 + (x * y) % 13) as u8;
                    img[i + 1] = (x * 11 + y * 5 + 37) as u8;
                    img[i + 2] = (x * 3 + y * 17 + 91) as u8;
                    img[i + 3] = (x * 5 + y * 13 + 17) as u8;
                } else {
                    let block = x / 8;
                    img[i] = (40 + block * 20) as u8;
                    img[i + 1] = (60 + block * 13) as u8;
                    img[i + 2] = (80 + block * 7) as u8;
                    img[i + 3] = 255;
                }
            }
        }
        img
    }

    // ------------------------------------------------------------------
    // Test encoder: TLG6 (channel order matches the real game files:
    // plane 0 = blue, 1 = green, 2 = red, 3 = alpha)
    // ------------------------------------------------------------------

    struct BitWriter {
        bytes: Vec<u8>,
        bit: u8,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                bytes: vec![0],
                bit: 0,
            }
        }
        fn put1(&mut self, b: bool) {
            if b {
                *self.bytes.last_mut().unwrap() |= 1 << self.bit;
            }
            self.bit += 1;
            if self.bit == 8 {
                self.bytes.push(0);
                self.bit = 0;
            }
        }
        fn put_value(&mut self, mut v: u32, len: u32) {
            for _ in 0..len {
                self.put1(v & 1 == 1);
                v >>= 1;
            }
        }
        fn put_gamma(&mut self, mut v: u32) {
            let mut t = v >> 1;
            let mut cnt = 0u32;
            while t != 0 {
                self.put1(false);
                t >>= 1;
                cnt += 1;
            }
            self.put1(true);
            while cnt > 0 {
                self.put1(v & 1 == 1);
                v >>= 1;
                cnt -= 1;
            }
        }
        fn bit_len(&self) -> u32 {
            (self.bytes.len() - 1) as u32 * 8 + u32::from(self.bit)
        }
        fn into_bytes(mut self) -> Vec<u8> {
            if self.bit == 0 {
                self.bytes.pop();
            }
            self.bytes
        }
    }

    /// Golomb-Rice + run-length entropy coder (reference
    /// `CompressValuesGolomb`).
    fn golomb_compress(buf: &[i8]) -> (u32, Vec<u8>) {
        let gbt = golomb_bit_length_table();
        let mut bw = BitWriter::new();
        bw.put_value(u32::from(buf.first().copied() != Some(0)), 1);
        let mut count = 0usize;
        let mut n = TLG6_GOLOMB_N_COUNT - 1;
        let mut a = 0usize;
        let mut i = 0usize;
        while i < buf.len() {
            if buf[i] != 0 {
                if count > 0 {
                    bw.put_gamma(count as u32);
                }
                count = 0;
                let mut j = i;
                while j < buf.len() && buf[j] != 0 {
                    j += 1;
                }
                let run = j - i;
                bw.put_gamma(run as u32);
                for &e in &buf[i..j] {
                    let k = gbt[a][n] as u32;
                    let m = if e >= 0 {
                        2 * i32::from(e)
                    } else {
                        -2 * i32::from(e) - 1
                    } - 1;
                    // Golomb unary part with the reference's 4-byte "give
                    // up" escape (reference `GOLOMB_GIVE_UP_BYTES`): when
                    // the zero run would cross four bytes, the run length
                    // is written as 8 raw bits instead.
                    let un = (m >> k) as u32;
                    let store_limit = (bw.bytes.len() - 1) + 4;
                    let mut put1 = true;
                    for _ in 0..un {
                        if store_limit == bw.bytes.len() - 1 {
                            bw.put_value(un, 8);
                            put1 = false;
                            break;
                        }
                        bw.put1(false);
                    }
                    if store_limit == bw.bytes.len() - 1 {
                        bw.put_value(un, 8);
                        put1 = false;
                    }
                    if put1 {
                        bw.put1(true);
                    }
                    bw.put_value(m as u32, k);
                    a = (a + (m >> 1) as usize).min(TLG6_GOLOMB_TABLE_LEN - 1);
                    if n == 0 {
                        a >>= 1;
                        n = TLG6_GOLOMB_N_COUNT - 1;
                    } else {
                        n -= 1;
                    }
                }
                i = j;
            } else {
                count += 1;
                i += 1;
            }
        }
        if count > 0 {
            bw.put_gamma(count as u32);
        }
        (bw.bit_len(), bw.into_bytes())
    }

    /// Apply one of the 16 color filters (reference `ApplyColorFilter`).
    fn apply_filter(code: u8, b: &mut [i8], g: &mut [i8], r: &mut [i8]) {
        for d in 0..b.len() {
            match code {
                0 => {}
                1 => {
                    r[d] = r[d].wrapping_sub(g[d]);
                    b[d] = b[d].wrapping_sub(g[d]);
                }
                2 => {
                    r[d] = r[d].wrapping_sub(g[d]);
                    g[d] = g[d].wrapping_sub(b[d]);
                }
                3 => {
                    b[d] = b[d].wrapping_sub(g[d]);
                    g[d] = g[d].wrapping_sub(r[d]);
                }
                4 => {
                    r[d] = r[d].wrapping_sub(g[d]);
                    g[d] = g[d].wrapping_sub(b[d]);
                    b[d] = b[d].wrapping_sub(r[d]);
                }
                5 => {
                    g[d] = g[d].wrapping_sub(b[d]);
                    b[d] = b[d].wrapping_sub(r[d]);
                }
                6 => {
                    b[d] = b[d].wrapping_sub(g[d]);
                }
                7 => {
                    g[d] = g[d].wrapping_sub(b[d]);
                }
                8 => {
                    r[d] = r[d].wrapping_sub(g[d]);
                }
                9 => {
                    b[d] = b[d].wrapping_sub(g[d]);
                    g[d] = g[d].wrapping_sub(r[d]);
                    r[d] = r[d].wrapping_sub(b[d]);
                }
                10 => {
                    g[d] = g[d].wrapping_sub(r[d]);
                    b[d] = b[d].wrapping_sub(r[d]);
                }
                11 => {
                    r[d] = r[d].wrapping_sub(b[d]);
                    g[d] = g[d].wrapping_sub(b[d]);
                }
                12 => {
                    g[d] = g[d].wrapping_sub(r[d]);
                    r[d] = r[d].wrapping_sub(b[d]);
                }
                13 => {
                    g[d] = g[d].wrapping_sub(r[d]);
                    r[d] = r[d].wrapping_sub(b[d]);
                    b[d] = b[d].wrapping_sub(g[d]);
                }
                14 => {
                    r[d] = r[d].wrapping_sub(b[d]);
                    b[d] = b[d].wrapping_sub(g[d]);
                    g[d] = g[d].wrapping_sub(r[d]);
                }
                15 => {
                    let t = b[d].wrapping_mul(2);
                    r[d] = r[d].wrapping_sub(t);
                    g[d] = g[d].wrapping_sub(t);
                }
                _ => unreachable!(),
            }
        }
    }

    /// Assemble a TLG6 file from an RGBA8 image. `colors` is 3 or 4; block
    /// `i` uses filter `i % 16` with MED (i < 16 blocks) or AVG otherwise.
    fn encode_tlg6(img: &[u8], w: usize, h: usize, colors: usize) -> Vec<u8> {
        assert!(colors == 3 || colors == 4);
        let mut out = Vec::new();
        out.extend_from_slice(&TLG6_MAGIC);
        out.push(colors as u8);
        out.extend_from_slice(&[0, 0, 0]);
        out.extend_from_slice(&(w as i32).to_le_bytes());
        out.extend_from_slice(&(h as i32).to_le_bytes());
        let maxlen_pos = out.len();
        out.extend_from_slice(&0i32.to_le_bytes());

        let mut filter_types = Vec::new();
        let mut streams: Vec<(u32, Vec<u8>)> = Vec::new();
        let mut max_bit_length = 0u32;
        let mut block_idx = 0usize;

        for y in (0..h).step_by(8) {
            let ylim = (y + 8).min(h);
            let r = ylim - y;
            let mut block_buf: Vec<Vec<i8>> = vec![Vec::new(); colors];
            for x in (0..w).step_by(8) {
                let xlim = (x + 8).min(w);
                let bw = xlim - x;
                let xp = x / 8;
                let ft = (block_idx % 16) as u8;
                let method = (block_idx / 16) % 2; // 0 = MED, 1 = AVG
                block_idx += 1;

                // prediction errors per channel (channel order B,G,R,A)
                let n = r * bw;
                let mut errs: Vec<Vec<i8>> = vec![vec![0i8; n]; colors];
                for yy in y..ylim {
                    for xx in x..xlim {
                        for c in 0..colors {
                            let ch = [2usize, 1, 0, 3][c.min(3)]; // B,G,R,A -> img index
                            let px = img[(yy * w + xx) * 4 + ch] as i16;
                            let pa = if xx > 0 {
                                img[(yy * w + xx - 1) * 4 + ch] as i16
                            } else {
                                0
                            };
                            let pb = if yy > 0 {
                                img[((yy - 1) * w + xx) * 4 + ch] as i16
                            } else {
                                0
                            };
                            let py = if method == 0 {
                                let pc = if xx > 0 && yy > 0 {
                                    img[((yy - 1) * w + xx - 1) * 4 + ch] as i16
                                } else {
                                    0
                                };
                                let (mn, mx) = if pa < pb { (pa, pb) } else { (pb, pa) };
                                if pc >= mx {
                                    mn
                                } else if pc < mn {
                                    mx
                                } else {
                                    pa + pb - pc
                                }
                            } else {
                                (pa + pb + 1) >> 1
                            };
                            errs[c][(yy - y) * bw + (xx - x)] = ((px - py) & 0xff) as i8;
                        }
                    }
                }

                // zigzag reordering (reference reordering block)
                let mut reord: Vec<Vec<i8>> = vec![vec![0i8; n]; colors];
                let mut wp = 0usize;
                for yy in y..ylim {
                    let ofs = if xp % 2 == 0 {
                        (yy - y) * bw
                    } else {
                        (ylim - yy - 1) * bw
                    };
                    let dir = if r % 2 == 0 {
                        ((yy & 1) ^ (xp & 1)) != 0
                    } else if xp % 2 == 1 {
                        (yy & 1) != 0
                    } else {
                        ((yy & 1) ^ (xp & 1)) != 0
                    };
                    if !dir {
                        for xx in 0..bw {
                            for c in 0..colors {
                                reord[c][wp] = errs[c][ofs + xx];
                            }
                            wp += 1;
                        }
                    } else {
                        for xx in (0..bw).rev() {
                            for c in 0..colors {
                                reord[c][wp] = errs[c][ofs + xx];
                            }
                            wp += 1;
                        }
                    }
                }

                // color filter on this block's segment
                let (rb, rg) = reord.split_at_mut(1);
                let (rg, rr) = rg.split_at_mut(1);
                apply_filter(ft, &mut rb[0], &mut rg[0], &mut rr[0]);
                for c in 0..colors {
                    block_buf[c].extend_from_slice(&reord[c]);
                }
                filter_types.push((ft << 1) | (method as u8));
            }

            // entropy-code each channel of this block group
            for bc in &block_buf {
                let (bits, bytes) = golomb_compress(bc);
                max_bit_length = max_bit_length.max(bits);
                streams.push((bits, bytes));
            }
        }

        out[maxlen_pos..maxlen_pos + 4].copy_from_slice(&(max_bit_length as i32).to_le_bytes());

        // filter types, LZSS-compressed with the fixed initial dictionary
        let mut ft_text = [0u8; 4096];
        for i in 0..32usize {
            for j in 0..16usize {
                let base = (i * 16 + j) * 8;
                ft_text[base..base + 4].fill(i as u8);
                ft_text[base + 4..base + 8].fill(j as u8);
            }
        }
        let mut st = LzssState {
            text: ft_text,
            s: 0,
        };
        let comp = lzss_encode(&filter_types, &mut st);
        out.extend_from_slice(&(comp.len() as i32).to_le_bytes());
        out.extend_from_slice(&comp);

        for (bits, bytes) in streams {
            out.extend_from_slice(&(bits as i32).to_le_bytes());
            out.extend_from_slice(&bytes);
        }
        out
    }

    // ------------------------------------------------------------------
    // Test encoder: TLG5
    // ------------------------------------------------------------------

    fn encode_tlg5(img: &[u8], w: usize, h: usize, colors: usize, blockheight: usize) -> Vec<u8> {
        assert!(colors == 3 || colors == 4);
        let blockcount = (h - 1) / blockheight + 1;
        let mut out = Vec::new();
        out.extend_from_slice(&TLG5_MAGIC);
        out.push(colors as u8);
        out.extend_from_slice(&(w as i32).to_le_bytes());
        out.extend_from_slice(&(h as i32).to_le_bytes());
        out.extend_from_slice(&(blockheight as i32).to_le_bytes());
        let table_pos = out.len();
        out.resize(table_pos + blockcount * 4, 0);

        // LZSS window state persists across blocks, like the reference
        let mut st = LzssState {
            text: [0u8; 4096],
            s: 0,
        };
        let mut blocksizes = Vec::new();

        for blk_y in (0..h).step_by(blockheight) {
            let ylim = (blk_y + blockheight).min(h);
            let elems = (ylim - blk_y) * w;
            let mut planes: Vec<Vec<i8>> = vec![vec![0i8; elems]; colors];
            for (yy, y) in (blk_y..ylim).enumerate() {
                let mut prevcl = [0u8; 4];
                for x in 0..w {
                    let mut val = [0u8; 4];
                    for c in 0..colors {
                        let cur = img[(y * w + x) * 4 + c];
                        let up = if y > 0 {
                            img[((y - 1) * w + x) * 4 + c]
                        } else {
                            0
                        };
                        let cl = cur.wrapping_sub(up);
                        val[c] = cl.wrapping_sub(prevcl[c]);
                        prevcl[c] = cl;
                    }
                    let inp = yy * w + x;
                    // planes: 0 = B-G, 1 = G, 2 = R-G, 3 = A
                    planes[0][inp] = val[2].wrapping_sub(val[1]) as i8;
                    planes[1][inp] = val[1] as i8;
                    planes[2][inp] = val[0].wrapping_sub(val[1]) as i8;
                    if colors == 4 {
                        planes[3][inp] = val[3] as i8;
                    }
                }
            }
            let mut block = Vec::new();
            for pl in &planes {
                let raw: Vec<u8> = pl.iter().map(|&v| v as u8).collect();
                let comp = lzss_encode(&raw, &mut st);
                if comp.len() < raw.len() {
                    block.push(0u8);
                    block.extend_from_slice(&(comp.len() as i32).to_le_bytes());
                    block.extend_from_slice(&comp);
                } else {
                    block.push(1u8);
                    block.extend_from_slice(&(raw.len() as i32).to_le_bytes());
                    block.extend_from_slice(&raw);
                }
            }
            blocksizes.push(block.len() as i32);
            out.extend_from_slice(&block);
        }
        for (i, sz) in blocksizes.iter().enumerate() {
            out[table_pos + i * 4..table_pos + i * 4 + 4].copy_from_slice(&sz.to_le_bytes());
        }
        out
    }

    // ------------------------------------------------------------------
    // Test encoder: modified LZSS (mirrors the decoder's window state)
    // ------------------------------------------------------------------

    struct LzssState {
        text: [u8; 4096],
        s: usize,
    }

    /// Greedy LZSS encoder matching the decoder's flag format (groups of 8
    /// items, flag byte first, bit 0 = first item).
    fn lzss_encode(data: &[u8], st: &mut LzssState) -> Vec<u8> {
        let mut out = Vec::new();
        let mut group = [0u8; 40]; // flag byte + 8 items (matches take 2-3 bytes)
        let mut gcount = 0usize; // item count in the current group
        let mut glen = 1usize; // byte count (flag + item bytes)
        let mut i = 0usize;
        while i < data.len() {
            let remaining = data.len() - i;
            let mut best_pos = 0usize;
            let mut best_len = 0usize;
            if remaining >= 3 {
                let maxlen = remaining.min(273);
                for p in 0..4096usize {
                    if st.text[p] != data[i] {
                        continue;
                    }
                    let mut len = 0usize;
                    while len < maxlen && st.text[(p + len) & 0xfff] == data[i + len] {
                        len += 1;
                    }
                    if len > best_len {
                        best_len = len;
                        best_pos = p;
                        if best_len == maxlen {
                            break;
                        }
                    }
                }
            }
            if best_len >= 3 {
                group[0] |= 1 << gcount;
                if best_len >= 18 {
                    group[glen] = (best_pos & 0xff) as u8;
                    group[glen + 1] = (((best_pos >> 8) & 0xf) as u8) | 0xf0;
                    group[glen + 2] = (best_len - 18) as u8;
                    glen += 3;
                } else {
                    group[glen] = (best_pos & 0xff) as u8;
                    group[glen + 1] =
                        (((best_pos >> 8) & 0xf) as u8) | (((best_len - 3) as u8) << 4);
                    glen += 2;
                }
                for k in 0..best_len {
                    st.text[st.s & 0xfff] = data[i + k];
                    st.s = (st.s + 1) & 0xfff;
                }
                i += best_len;
            } else {
                group[glen] = data[i];
                st.text[st.s & 0xfff] = data[i];
                st.s = (st.s + 1) & 0xfff;
                glen += 1;
                i += 1;
            }
            gcount += 1;
            if gcount == 8 {
                out.extend_from_slice(&group[..glen]);
                group = [0u8; 40];
                gcount = 0;
                glen = 1;
            }
        }
        if gcount > 0 {
            out.extend_from_slice(&group[..glen]);
        }
        out
    }
}
