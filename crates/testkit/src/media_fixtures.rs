//! Synthetic image, audio, and TLG fixtures shared by the `media`
//! integration tests.
//!
//! The builders here are a deliberate test-only copy of the reference
//! encoders. They live in this dev-support crate so the production `media`
//! crate contains no `#[cfg(test)]` code and so every integration-test binary
//! links only the fixtures it actually uses.

// ---------------------------------------------------------------------------
// Synthetic image fixtures (PNG + deterministic RGBA pattern)
// ---------------------------------------------------------------------------

/// Deterministic RGBA8 pattern used by the image-router tests.
pub fn pattern(width: u32, height: u32) -> Vec<u8> {
    (0..width * height)
        .flat_map(|i| {
            let x = i % width;
            let y = i / width;
            [(x * 7) as u8, (y * 11) as u8, 128, 255]
        })
        .collect()
}

/// Encodes an RGBA8 buffer as PNG in memory.
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    use image::ImageEncoder;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
        .expect("png encode");
    out
}

// ---------------------------------------------------------------------------
// Synthetic audio fixture (minimal valid PCM WAV)
// ---------------------------------------------------------------------------

/// Builds a minimal valid 16-bit mono PCM WAV file.
pub fn tiny_wav_mono(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let bits_per_sample: u16 = 16;
    let channels: u16 = 1;
    let block_align = channels * bits_per_sample / 8;
    let byte_rate = sample_rate * u32::from(block_align);
    let data_len = (samples.len() as u32) * 2;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&bits_per_sample.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    wav
}

// ---------------------------------------------------------------------------
// Synthetic TLG fixtures
// ---------------------------------------------------------------------------

/// TLG5/6 raw-data magic (copied from `src/tlg.rs`; test-only fixture use).
pub const TLG5_MAGIC: [u8; 11] = *b"TLG5.0\x00raw\x1a";
/// TLG6 raw-data magic.
pub const TLG6_MAGIC: [u8; 11] = *b"TLG6.0\x00raw\x1a";
/// TLG0.0 "structured data stream" wrapper magic.
pub const SDS_MAGIC: [u8; 11] = *b"TLG0.0\x00sds\x1a";

/// Golomb adaptation window (reference `TVP_TLG6_GOLOMB_N_COUNT`).
const TLG6_GOLOMB_N_COUNT: usize = 4;
/// Number of entries in the Golomb bit-length table (4 x 2 x 128).
const TLG6_GOLOMB_TABLE_LEN: usize = 1024;

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

/// Deterministic test image: left half gradient + noise, right half
/// flat colour blocks (flat regions exercise the zero-run lengths).
pub fn test_image(w: usize, h: usize) -> Vec<u8> {
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
    let gbt = &GOLOMB_BIT_LENGTH_TABLE;
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
pub fn encode_tlg6(img: &[u8], w: usize, h: usize, colors: usize) -> Vec<u8> {
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

/// Assembles a TLG5 file from an RGBA8 image. `colors` is 3 or 4; the image
/// is split into `blockheight`-row blocks, each LZSS-compressed per plane.
pub fn encode_tlg5(img: &[u8], w: usize, h: usize, colors: usize, blockheight: usize) -> Vec<u8> {
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
                group[glen + 1] = (((best_pos >> 8) & 0xf) as u8) | (((best_len - 3) as u8) << 4);
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
