//! Parity between the optimized TLG decoder and the decoder as it was
//! before the throughput change, plus a repeatable measurement.
//!
//! The `legacy` module below is a verbatim copy of the pre-change TLG5
//! path: the byte-at-a-time LZSS output loop and the per-pixel compose.
//! The optimized decoder writes into pre-sized buffers and copies whole
//! runs, so it must still produce the same bytes — and the same error
//! message — for every fixture, truncated ones included.

use std::time::Instant;

use media::{Media, decode_image};
use testkit::{SDS_MAGIC, encode_tlg5, encode_tlg6, test_image};

/// The TLG5 decoder as it was before the throughput change, kept as the
/// byte-for-byte oracle for the optimized path.
mod legacy {
    const TLG5_MAGIC: [u8; 11] = *b"TLG5.0\x00raw\x1a";
    const SDS_MAGIC: [u8; 11] = *b"TLG0.0\x00sds\x1a";

    /// Decodes a TLG5 payload (bare or inside the TLG0.0 wrapper) into raw
    /// RGBA8 bytes.
    pub fn decode_tlg5(bytes: &[u8]) -> Result<Vec<u8>, String> {
        let raw = strip_sds(bytes)?;
        if raw.len() < 12 || raw[..11] != TLG5_MAGIC {
            return Err("not a TLG5/TLG6 image (bad magic)".into());
        }
        decode_tlg5_raw(raw)
    }

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
        let pixels = w.checked_mul(h).ok_or("TLG: image size overflow")?;
        pixels.checked_mul(4).ok_or("TLG: image size overflow")?;
        Ok(())
    }

    fn read_dims(raw: &[u8], woff: usize, hoff: usize) -> Result<(usize, usize), String> {
        let width = read_i32(raw, woff)?;
        let height = read_i32(raw, hoff)?;
        let w = usize::try_from(width).map_err(|_| "TLG: negative image width".to_string())?;
        let h = usize::try_from(height).map_err(|_| "TLG: negative image height".to_string())?;
        check_dims(w, h)?;
        Ok((w, h))
    }

    fn decode_tlg5_raw(raw: &[u8]) -> Result<Vec<u8>, String> {
        let colors = *raw.get(11).ok_or("TLG5: truncated header")? as usize;
        if colors != 3 && colors != 4 {
            return Err(format!("TLG5: unsupported color count {colors}"));
        }
        let (width, height) = read_dims(raw, 12, 16)?;
        let blockheight = read_i32(raw, 20)?;
        let blockheight =
            usize::try_from(blockheight).map_err(|_| "TLG5: negative block height".to_string())?;
        if blockheight == 0 {
            return Err("TLG5: zero block height".into());
        }
        let blockheight = blockheight.min(height);
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
                        rowbuf[x * 4 + 3] = 0xff;
                    }
                }
                img[y * width * 4..(y + 1) * width * 4].copy_from_slice(&rowbuf);
            }
        }

        if img.len() != width * height * 4 {
            return Err("TLG5: invalid image dimensions".into());
        }
        Ok(img)
    }

    /// Decompress a modified-LZSS stream into `out` (at most `out_len`
    /// bytes), the pre-change implementation.
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
}

/// Decodes through the crate's public router: the raw RGBA8 bytes, or the
/// error text.
fn crate_decode(bytes: &[u8]) -> Result<Vec<u8>, String> {
    match decode_image(&Media::new(), bytes, None) {
        Ok(image) => Ok(image.rgba),
        Err(error) => Err(error.to_string()),
    }
}

/// Asserts that the crate and the pre-change decoder agree on `bytes`.
fn assert_same(bytes: &[u8], what: &str) {
    let optimized = crate_decode(bytes);
    let before = legacy::decode_tlg5(bytes);
    match (optimized, before) {
        (Ok(got), Ok(want)) => assert_eq!(
            got, want,
            "{what}: the optimized decoder must produce the same bytes"
        ),
        (Err(got), Err(want)) => assert!(
            got.contains(want.as_str()),
            "{what}: error text differs: optimized {got:?} vs before {want:?}"
        ),
        (got, want) => panic!("{what}: result differs: optimized {got:?} vs before {want:?}"),
    }
}

/// The TLG5 fixtures the parity checks run on. The larger ones push the
/// LZSS output past the 4096-byte window, so the window position wraps
/// inside a single image.
fn tlg5_fixtures() -> Vec<(String, Vec<u8>)> {
    let mut fixtures = Vec::new();
    for (w, h, colors, blockheight) in [
        (24usize, 16usize, 4usize, 4usize),
        (21, 19, 3, 4),
        (9, 7, 4, 12),  // block height above the image height
        (32, 32, 4, 1), // one row per block
        (64, 48, 4, 8),
        (128, 96, 4, 8), // more than one window of LZSS output
        (128, 96, 3, 16),
    ] {
        let img = test_image(w, h);
        let encoded = encode_tlg5(&img, w, h, colors, blockheight);
        fixtures.push((format!("tlg5 {w}x{h} c{colors} b{blockheight}"), encoded));
    }
    fixtures
}

/// A raw-8 LZSS stream: a zero flag byte (eight literals) followed by the
/// eight literal bytes of one plane.
static RAW8_PLANE0: [u8; 9] = [0x00, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17];
static RAW8_PLANE1: [u8; 9] = [0x00, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27];
static RAW8_PLANE2: [u8; 9] = [0x00, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37];
static RAW8_PLANE3: [u8; 9] = [0x00, 0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47];
/// A match LZSS stream: the flag byte selects a match, its header asks for
/// twenty bytes out of the (empty) window.
static MATCH20: [u8; 4] = [0x01, 0x00, 0xf0, 0x02];

/// Builds a TLG5 file with one scanline and one row per block whose four
/// plane streams are given verbatim. A hand-built stream can overrun the
/// plane it is decoded into, which the encoder never produces.
fn tlg5_with_plane_streams(width: usize, streams: [&[u8]; 4]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"TLG5.0\x00raw\x1a");
    out.push(4); // four channels
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&1i32.to_le_bytes()); // one scanline
    out.extend_from_slice(&1i32.to_le_bytes()); // one row per block
    let table_pos = out.len();
    out.resize(table_pos + 4, 0); // one block size
    let mut block = Vec::new();
    for stream in streams {
        block.push(0u8); // compressed
        block.extend_from_slice(&(stream.len() as i32).to_le_bytes());
        block.extend_from_slice(stream);
    }
    out.extend_from_slice(&block);
    let size = (block.len() as i32).to_le_bytes();
    out[table_pos..table_pos + 4].copy_from_slice(&size);
    out
}

/// A raw-8 LZSS token produces eight bytes. A plane that holds fewer must
/// stop the decoder at the plane size, not run past the output buffer.
#[test]
fn tlg5_raw8_token_stops_at_the_plane_size() {
    let streams: [&[u8]; 4] = [&RAW8_PLANE0, &RAW8_PLANE1, &RAW8_PLANE2, &RAW8_PLANE3];
    let bytes = tlg5_with_plane_streams(1, streams);
    // The pre-change decoder stops at the plane size as well.
    assert_same(&bytes, "tlg5 raw-8 token");
    let decoded = crate_decode(&bytes).expect("the raw-8 token must stop at the plane size");
    // plane0 = 0x10, plane1 = 0x20, plane2 = 0x30, plane3 = 0x40.
    assert_eq!(decoded, vec![0x50, 0x20, 0x30, 0x40]);
}

/// The same overrun with a plane that holds three bytes instead of one.
#[test]
fn tlg5_raw8_token_stops_at_the_plane_size_of_three() {
    let streams: [&[u8]; 4] = [&RAW8_PLANE0, &RAW8_PLANE1, &RAW8_PLANE2, &RAW8_PLANE3];
    let bytes = tlg5_with_plane_streams(3, streams);
    assert_same(&bytes, "tlg5 raw-8 token, three byte plane");
    let decoded = crate_decode(&bytes).expect("the raw-8 token must stop at the plane size");
    assert_eq!(decoded.len(), 3 * 4);
}

/// A match token that runs past the plane size stops at the plane size.
#[test]
fn tlg5_match_token_stops_at_the_plane_size() {
    let streams: [&[u8]; 4] = [&MATCH20, &MATCH20, &MATCH20, &MATCH20];
    let bytes = tlg5_with_plane_streams(1, streams);
    assert_same(&bytes, "tlg5 match token");
    let decoded = crate_decode(&bytes).expect("the match must stop at the plane size");
    assert_eq!(decoded, vec![0, 0, 0, 0]);
}

#[test]
fn tlg5_matches_the_pre_change_decoder() {
    for (what, bytes) in tlg5_fixtures() {
        assert_same(&bytes, &what);
    }
}

#[test]
fn tlg5_sds_wrapper_matches_the_pre_change_decoder() {
    let img = test_image(40, 24);
    let raw = encode_tlg5(&img, 40, 24, 4, 8);
    let mut wrapped = Vec::new();
    wrapped.extend_from_slice(&SDS_MAGIC);
    wrapped.extend_from_slice(&(raw.len() as i32).to_le_bytes());
    wrapped.extend_from_slice(&raw);
    assert_same(&wrapped, "tlg5 sds");
}

#[test]
fn tlg5_truncated_matches_the_pre_change_decoder() {
    let (_, bytes) = tlg5_fixtures().into_iter().nth(5).expect("fixture");
    // Every truncation that keeps the magic must fail the same way in both
    // decoders, with the same message.
    for len in [11usize, 12, 20, 24, 40, 64, 100, 256, 1000, 2000] {
        if len >= bytes.len() {
            continue;
        }
        assert_same(&bytes[..len], &format!("tlg5 truncated to {len}"));
    }
}

#[test]
fn tlg5_corrupted_matches_the_pre_change_decoder() {
    let (_, bytes) = tlg5_fixtures().into_iter().nth(5).expect("fixture");
    // Flipping payload bytes may still decode (to different pixels); both
    // decoders must agree either way.
    for offset in [30usize, 64, 100, 333, 700, 1500] {
        let mut damaged = bytes.clone();
        if offset >= damaged.len() {
            continue;
        }
        damaged[offset] ^= 0xff;
        assert_same(&damaged, &format!("tlg5 corrupted at {offset}"));
    }
}

#[test]
fn tlg6_still_decodes_after_the_change() {
    // The TLG6 path shares the LZSS primitive with TLG5; the filter map is
    // the only LZSS stream in it, and the round trip must stay lossless.
    let (w, h) = (48usize, 32usize);
    let img = test_image(w, h);
    let encoded = encode_tlg6(&img, w, h, 4);
    let decoded = crate_decode(&encoded).expect("synthetic TLG6 decodes");
    assert_eq!(decoded, img, "TLG6 round trip must be lossless");
}

/// Throughput measurement: the same fixture through the optimized decoder
/// and through the pre-change decoder. Run with
/// `cargo test --release -p media --test tlg_parity -- --nocapture` to see
/// the numbers.
#[test]
fn tlg5_throughput_measurement() {
    let (w, h) = (256usize, 256usize);
    let img = test_image(w, h);
    let encoded = encode_tlg5(&img, w, h, 4, 8);
    let rounds = 8usize;

    let start = Instant::now();
    for _ in 0..rounds {
        let decoded = crate_decode(&encoded).expect("optimized decode");
        assert_eq!(decoded.len(), w * h * 4);
    }
    let optimized = start.elapsed();

    let start = Instant::now();
    for _ in 0..rounds {
        let decoded = legacy::decode_tlg5(&encoded).expect("pre-change decode");
        assert_eq!(decoded.len(), w * h * 4);
    }
    let before = start.elapsed();

    eprintln!(
        "tlg5 {w}x{h} x{rounds} rounds: optimized {:.2?} ({:.2} ms/image), \
         pre-change {:.2?} ({:.2} ms/image), speedup {:.2}x",
        optimized,
        optimized.as_secs_f64() * 1000.0 / rounds as f64,
        before,
        before.as_secs_f64() * 1000.0 / rounds as f64,
        before.as_secs_f64() / optimized.as_secs_f64().max(f64::MIN_POSITIVE),
    );
}
