//! Stream decompression for PSB containers.
//!
//! The reference `PSBStreamDecompress.cpp` / `PSBLz4Stream.cpp` recognise
//! three stream shapes:
//!
//! * a raw Emote **LZ4 frame** (`04 22 4D 18 ...`) whose blocks are either
//!   stored uncompressed or compressed with the raw LZ4 block algorithm;
//! * an **MDF** archive (`MDF\0` magic) whose payload after the first eight
//!   bytes is a zlib stream;
//! * anything else, which is treated as a plain PSB byte stream.
//!
//! The frame *looks* like the standard LZ4 frame format, but the reference
//! only reads and discards the header, block and content checksums (its
//! `ReadI8LE`/`ReadI32LE` results are ignored) and decodes blocks with its
//! own `decompressBlock`, whose malformed-input behaviour this port
//! reproduces. A standards-validating decoder — `lz4_flex`'s frame reader
//! verifies those checksums — could therefore reject streams the reference
//! accepts, so the frame walk and the small raw-block decoder below stay a
//! faithful port instead of a crate.

use std::io::Read;

use super::error::{PsbError, PsbResult};

/// The Emote LZ4 frame magic.
const LZ4_MAGIC: [u8; 4] = [0x04, 0x22, 0x4d, 0x18];
/// The MDF archive magic.
const MDF_MAGIC: &[u8; 3] = b"MDF";

/// Decompress (or pass through) an on-disk PSB/MDF stream.
///
/// Mirrors the reference `openDecompressedStream`: short streams and unknown
/// magics are errors, the LZ4 and MDF containers are inflated, and every
/// other byte sequence is returned unchanged.
pub(crate) fn open_decompressed_stream(input: &[u8]) -> PsbResult<Vec<u8>> {
    if input.len() < 9 {
        return Err(PsbError::Truncated("stream header"));
    }
    if input[..4] == LZ4_MAGIC {
        return decompress_lz4_stream(input);
    }
    if &input[..3] == MDF_MAGIC {
        return decompress_mdf_stream(input);
    }
    Ok(input.to_vec())
}

/// Inflate the zlib payload of an MDF archive.
fn decompress_mdf_stream(input: &[u8]) -> PsbResult<Vec<u8>> {
    // Bytes 0..4 are the `MDF\0` magic, 4..8 the uncompressed length.
    let uncompressed_size = u32::from_le_bytes([input[4], input[5], input[6], input[7]]);
    let mut decoder = flate2::read::ZlibDecoder::new(&input[8..]);
    let mut output = Vec::with_capacity(uncompressed_size as usize);
    decoder
        .read_to_end(&mut output)
        .map_err(|error| PsbError::Decompress(error.to_string()))?;
    Ok(output)
}

/// The parsed Emote LZ4 frame header.
struct Lz4FrameInfo {
    block_size: usize,
    has_block_checksum: bool,
    has_content_checksum: bool,
    /// Offset of the first block, just past the frame header.
    blocks_offset: usize,
}

impl Lz4FrameInfo {
    fn parse(input: &[u8]) -> PsbResult<Self> {
        if input.len() < 6 {
            return Err(PsbError::Truncated("LZ4 frame header"));
        }
        let flags = input[4];
        if flags >> 6 != 1 {
            return Err(PsbError::Lz4("invalid frame version".into()));
        }
        let has_block_checksum = flags & 0x10 != 0;
        let has_content_length = flags & 0x08 != 0;
        let has_content_checksum = flags & 0x04 != 0;
        let has_dictionary = flags & 0x01 != 0;

        let code = input[5];
        let block_size = match (code >> 4) & 7 {
            4 => 0x10000,
            5 => 0x40000,
            6 => 0x100000,
            7 => 0x400000,
            _ => return Err(PsbError::Lz4("invalid max block size".into())),
        };

        let mut offset = 6;
        if has_content_length {
            // The reference reads two 32-bit halves and combines them but
            // never uses the value for decompression.
            if input.len() < offset + 8 {
                return Err(PsbError::Truncated("LZ4 content length"));
            }
            offset += 8;
        }
        if has_dictionary {
            if input.len() < offset + 4 {
                return Err(PsbError::Truncated("LZ4 dictionary id"));
            }
            offset += 4;
        }
        if input.len() <= offset {
            return Err(PsbError::Truncated("LZ4 header checksum"));
        }
        offset += 1; // Header checksum byte.

        Ok(Self {
            block_size,
            has_block_checksum,
            has_content_checksum,
            blocks_offset: offset,
        })
    }
}

/// Decompress an Emote LZ4 frame into a flat byte buffer.
fn decompress_lz4_stream(input: &[u8]) -> PsbResult<Vec<u8>> {
    let info = Lz4FrameInfo::parse(input)?;
    let mut offset = info.blocks_offset;
    let mut output = Vec::new();
    let mut buffer = vec![0u8; info.block_size];

    loop {
        if input.len() < offset + 4 {
            return Err(PsbError::Lz4("truncated block header".into()));
        }
        let block_size = i32::from_le_bytes([
            input[offset],
            input[offset + 1],
            input[offset + 2],
            input[offset + 3],
        ]);
        offset += 4;

        if block_size == 0 {
            if info.has_content_checksum && input.len() < offset + 4 {
                return Err(PsbError::Lz4("truncated content checksum".into()));
            }
            break;
        }

        if block_size < 0 {
            // Stored (uncompressed) block: the high bit marks the length.
            let size = (block_size as u32 & 0x7fff_ffff) as usize;
            if input.len() < offset + size {
                return Err(PsbError::Lz4("truncated stored block".into()));
            }
            output.extend_from_slice(&input[offset..offset + size]);
            offset += size;
        } else {
            let size = block_size as usize;
            if input.len() < offset + size {
                return Err(PsbError::Lz4("truncated compressed block".into()));
            }
            let written = decompress_block(&input[offset..offset + size], &mut buffer)?;
            output.extend_from_slice(&buffer[..written]);
            offset += size;
        }

        if info.has_block_checksum {
            if input.len() < offset + 4 {
                return Err(PsbError::Lz4("truncated block checksum".into()));
            }
            offset += 4;
        }
    }

    Ok(output)
}

/// Decode one raw LZ4 block into `output`, returning the number of bytes
/// written. A faithful translation of the reference `decompressBlock`.
fn decompress_block(block: &[u8], output: &mut [u8]) -> PsbResult<usize> {
    let iend = block.len();
    let oend = output.len();
    let mut src = 0usize;
    let mut dst = 0usize;

    loop {
        let token = *block
            .get(src)
            .ok_or_else(|| PsbError::Lz4("truncated block token".into()))?;
        src += 1;

        // Literal length: 4-bit nibble, extended by 0xFF runs.
        let mut length = (token >> 4) as usize;
        if length == 0xF {
            loop {
                let byte = *block
                    .get(src)
                    .ok_or_else(|| PsbError::Lz4("truncated literal length".into()))?;
                src += 1;
                length = length
                    .checked_add(byte as usize)
                    .ok_or_else(|| PsbError::Lz4("literal length overflow".into()))?;
                if !(src + 15 < iend && byte == 0xFF) {
                    break;
                }
            }
        }

        let copy_end = dst
            .checked_add(length)
            .ok_or_else(|| PsbError::Lz4("literal length overflow".into()))?;
        if copy_end > oend.saturating_sub(12) || src + length > iend.saturating_sub(8) {
            // Last literals: they must reach exactly the end of both buffers.
            if src + length != iend || copy_end > oend {
                return Err(PsbError::Lz4("invalid final literal run".into()));
            }
            output[dst..copy_end].copy_from_slice(&block[src..src + length]);
            dst = copy_end;
            break;
        }
        output[dst..copy_end].copy_from_slice(&block[src..src + length]);
        src += length;
        dst = copy_end;

        if block.len() < src + 2 {
            return Err(PsbError::Lz4("truncated match offset".into()));
        }
        let offset = u16::from_le_bytes([block[src], block[src + 1]]) as usize;
        src += 2;
        let match_pos = dst
            .checked_sub(offset)
            .ok_or_else(|| PsbError::Lz4("match offset before output".into()))?;

        let mut match_length = (token & 0xF) as usize;
        if match_length == 0xF {
            loop {
                let byte = *block
                    .get(src)
                    .ok_or_else(|| PsbError::Lz4("truncated match length".into()))?;
                src += 1;
                if src > iend.saturating_sub(5) {
                    return Err(PsbError::Lz4("invalid match length run".into()));
                }
                match_length = match_length
                    .checked_add(byte as usize)
                    .ok_or_else(|| PsbError::Lz4("match length overflow".into()))?;
                if byte != 0xFF {
                    break;
                }
            }
        }
        match_length += 4;

        let match_end = match_pos
            .checked_add(match_length)
            .ok_or_else(|| PsbError::Lz4("match length overflow".into()))?;
        let write_end = dst
            .checked_add(match_length)
            .ok_or_else(|| PsbError::Lz4("match length overflow".into()))?;
        if match_end > oend || write_end > oend {
            return Err(PsbError::Lz4("match overruns output".into()));
        }
        // `copy_within` has memmove semantics, which is exactly the
        // reference's `copyOverlapped`.
        output.copy_within(match_pos..match_end, dst);
        dst = write_end;
    }

    Ok(dst)
}

/// The Emote per-file decryption context (`EMoteCTX.h`).
#[derive(Debug, Clone)]
pub(crate) struct EmoteCtx {
    key: [u32; 4],
    v: u32,
    count: u32,
}

impl EmoteCtx {
    /// Initialise the key schedule from the four seed words.
    ///
    /// The reference builds `[0x075BCD15, 0x159A55E5, 0x1F123BB5, seed]`.
    pub(crate) fn new(seed: u32) -> Self {
        Self {
            key: [0x075B_CD15, 0x159A_55E5, 0x1F12_3BB5, seed],
            v: 0,
            count: 0,
        }
    }

    /// XOR `data` with the generated keystream (the reference
    /// `emote_decrypt`).
    pub(crate) fn decrypt(&mut self, data: &mut [u8]) {
        for byte in data.iter_mut() {
            if self.v == 0 {
                let b = self.key[3];
                let a = self.key[0] ^ (self.key[0] << 0xB);
                self.key[0] = self.key[1];
                self.key[1] = self.key[2];
                let c = a ^ b ^ ((a ^ (b >> 0xB)) >> 8);
                self.key[2] = b;
                self.key[3] = c;
                self.v = c;
                self.count = 4;
            }
            *byte ^= self.v as u8;
            self.v >>= 8;
            self.count -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_stream_is_returned_unchanged() {
        let data = b"PSB\0............".to_vec();
        assert_eq!(open_decompressed_stream(&data).unwrap(), data);
    }

    #[test]
    fn short_stream_is_rejected() {
        assert!(matches!(
            open_decompressed_stream(b"short"),
            Err(PsbError::Truncated(_))
        ));
    }

    #[test]
    fn mdf_stream_inflates() {
        use flate2::Compression;
        use flate2::write::ZlibEncoder;
        use std::io::Write;

        let payload = b"hello psb payload";
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(payload).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut input = Vec::new();
        input.extend_from_slice(b"MDF\0");
        input.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        input.extend_from_slice(&compressed);
        assert_eq!(open_decompressed_stream(&input).unwrap(), payload);
    }

    #[test]
    fn lz4_frame_with_stored_block_round_trips() {
        // Magic + FLG(version 1, independent) + BD(64 KiB) + HC + one stored
        // block + end mark.
        let payload = b"stored lz4 payload";
        let mut input = Vec::new();
        input.extend_from_slice(&LZ4_MAGIC);
        input.push(0x60); // version 1, independent blocks.
        input.push(0x40); // max block size code 4.
        input.push(0x00); // header checksum (ignored).
        // The high bit marks the block as stored; the low 31 bits are its
        // length (the reference `blockSize & 0x7FFFFFFF`).
        input.extend_from_slice(&(i32::MIN | payload.len() as i32).to_le_bytes());
        input.extend_from_slice(payload);
        input.extend_from_slice(&0i32.to_le_bytes());
        assert_eq!(open_decompressed_stream(&input).unwrap(), payload);
    }

    #[test]
    fn raw_lz4_block_decodes_literals() {
        // Token 0x30 => 3 literal bytes then the end marker; a block that is
        // all literals must decode to the reference "last literals" path.
        // Build: token, 3 literal bytes.
        let block = [0x30u8, b'a', b'b', b'c'];
        let mut output = [0u8; 16];
        let written = decompress_block(&block, &mut output).unwrap();
        assert_eq!(&output[..written], b"abc");
    }
}
