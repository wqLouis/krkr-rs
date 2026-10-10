//! Compact PSB container reader used by [`super::resource`].
//!
//! The reference plugin parses `.psb`/`.mtn` assets with `PSB::PSBFile`
//! (`reference/cpp/plugins/psbfile/`), then walks the resulting dictionary
//! tree to build motion clips, node layers, timelines and variables. The
//! compat crate already ships a full `PSBFile` native, but its parser lives in
//! a private sibling module; editing it is out of scope for this port, so this
//! module carries the minimal reader the motion runtime needs.
//!
//! It implements the same container format (header, name table, typed values,
//! resource chunks) plus the two stream wrappers the assets use (Emote LZ4
//! frames and MDF zlib archives). The parsed tree is intentionally generic:
//! the motion-specific schema mapping lives in [`super::model`].

use std::collections::HashMap;

use flate2::read::ZlibDecoder;
use std::io::Read as _;

/// The `PSB\0` magic.
const PSB_MAGIC: [u8; 4] = *b"PSB\0";
/// The `MDF\0` / `MFL\0` archive magics.
const MDF_MAGIC: [u8; 4] = *b"MDF\0";
const MFL_MAGIC: [u8; 4] = *b"MFL\0";
/// Emote LZ4 frame magic.
const LZ4_MAGIC: [u8; 4] = [0x04, 0x22, 0x4d, 0x18];

/// A parsed PSB value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PsbValue {
    /// Explicit null.
    Null,
    /// Boolean.
    Bool(bool),
    /// Signed integer.
    Int(i64),
    /// Single-precision real.
    Float(f32),
    /// Double-precision real.
    Double(f64),
    /// A raw integer array.
    Array(Vec<u32>),
    /// A string.
    String(String),
    /// A resource chunk reference (`index`, `extra`).
    Resource(u32, bool),
    /// An ordered list of values.
    List(Vec<PsbValue>),
    /// An ordered dictionary.
    Dict(Vec<(String, PsbValue)>),
}

impl PsbValue {
    /// The entries of an integer-array value.
    pub(crate) fn as_array(&self) -> Option<&[u32]> {
        match self {
            Self::Array(values) => Some(values),
            _ => None,
        }
    }

    /// The entry for `key` in a dictionary value.
    pub(crate) fn get(&self, key: &str) -> Option<&PsbValue> {
        match self {
            Self::Dict(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// A numeric value coerced to `f64`.
    pub(crate) fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Int(value) => Some(*value as f64),
            Self::Float(value) => Some(f64::from(*value)),
            Self::Double(value) => Some(*value),
            Self::Bool(value) => Some(if *value { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    /// A boolean value (numbers coerce to `!= 0`).
    pub(crate) fn as_bool(&self) -> bool {
        match self {
            Self::Bool(value) => *value,
            Self::Int(value) => *value != 0,
            Self::Float(value) => *value != 0.0,
            Self::Double(value) => *value != 0.0,
            _ => false,
        }
    }

    /// A list value.
    pub(crate) fn as_list(&self) -> Option<&[PsbValue]> {
        match self {
            Self::List(items) => Some(items),
            _ => None,
        }
    }

    /// A dictionary value.
    pub(crate) fn as_dict(&self) -> Option<&[(String, PsbValue)]> {
        match self {
            Self::Dict(entries) => Some(entries),
            _ => None,
        }
    }
}

/// A parsed PSB document.
pub(crate) struct Psb {
    /// The root value.
    pub(crate) root: PsbValue,
}

/// A parse failure. The message is surfaced through the TJS error slot.
#[derive(Debug)]
pub(crate) struct PsbError(pub(crate) String);

type Result<T> = std::result::Result<T, PsbError>;

fn err<T>(message: impl Into<String>) -> Result<T> {
    Err(PsbError(message.into()))
}

/// A little-endian cursor.
struct Reader<'a> {
    data: &'a [u8],
    pub(crate) pos: usize,
}

impl<'a> Reader<'a> {
    fn at(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    fn set_pos(&mut self, pos: usize) -> Result<()> {
        if pos > self.data.len() {
            return err("truncated PSB: stream position out of range");
        }
        self.pos = pos;
        Ok(())
    }

    fn bytes(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(count)
            .ok_or_else(|| PsbError("truncated PSB: length overflow".into()))?;
        let slice = self
            .data
            .get(self.pos..end)
            .ok_or_else(|| PsbError("truncated PSB".into()))?;
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        let b = self.bytes(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn uint(&mut self, count: usize) -> Result<u64> {
        if count == 0 {
            return Ok(0);
        }
        if count > 8 {
            return err(format!("bad integer width {count}"));
        }
        let mut value = 0u64;
        for (index, byte) in self.bytes(count)?.iter().enumerate() {
            value |= u64::from(*byte) << (index * 8);
        }
        Ok(value)
    }

    fn null_terminated_string(&mut self) -> Result<String> {
        let start = self.pos;
        let end = self.data[start..]
            .iter()
            .position(|byte| *byte == 0)
            .map_or(self.data.len(), |offset| start + offset);
        let bytes = &self.data[start..end];
        self.pos = if end < self.data.len() { end + 1 } else { end };
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Header {
    signature: [u8; 4],
    version: u16,
    encrypt: u16,
    offset_encrypt: u32,
    offset_names: u32,
    offset_strings: u32,
    offset_strings_data: u32,
    offset_chunk_offsets: u32,
    offset_chunk_lengths: u32,
    offset_chunk_data: u32,
    offset_entries: u32,
}

impl Header {
    const MAX_LENGTH: u32 = 56;

    fn parse(reader: &mut Reader) -> Result<Self> {
        let mut header = Self::default();
        header.signature.copy_from_slice(reader.bytes(4)?);
        header.version = reader.u16()?;
        header.encrypt = reader.u16()?;
        header.offset_encrypt = reader.u32()?;
        header.offset_names = reader.u32()?;
        if header.signature == MDF_MAGIC || header.signature == MFL_MAGIC {
            return err("this looks like an MDF/MFL archive, not a PSB file");
        }
        if header.signature != PSB_MAGIC {
            return err(format!(
                "not a valid PSB file (signature {:?})",
                header.signature
            ));
        }
        if (header.offset_names as usize) < reader.data.len() {
            header.offset_strings = reader.u32()?;
            header.offset_strings_data = reader.u32()?;
            header.offset_chunk_offsets = reader.u32()?;
            header.offset_chunk_lengths = reader.u32()?;
            header.offset_chunk_data = reader.u32()?;
            header.offset_entries = reader.u32()?;
        }
        Ok(header)
    }

    fn is_encrypted(&self) -> bool {
        self.offset_encrypt > Self::MAX_LENGTH + 16
            || self.offset_names == 0
            || (self.version > 1
                && self.offset_encrypt != self.offset_names
                && self.offset_encrypt != 0)
    }
}

fn read_count_size(byte: u8) -> Result<usize> {
    if (0x0D..=0x14).contains(&byte) {
        Ok((byte - 0x0D + 1) as usize)
    } else {
        err(format!("bad array count size {byte:#x}"))
    }
}

fn read_list_body(reader: &mut Reader, count_size: usize) -> Result<Vec<u32>> {
    let count = reader.uint(count_size)? as usize;
    if count > i32::MAX as usize {
        return err(format!("array count {count} too large"));
    }
    let entry_byte = reader.u8()?;
    if entry_byte < 0x0C {
        return err(format!("bad array entry width {entry_byte:#x}"));
    }
    let entry_width = (entry_byte - 0x0C) as usize;
    if entry_width > 8 {
        return err(format!("array entry width {entry_width} too large"));
    }
    let mut values = Vec::with_capacity(count.min(1 << 16));
    for _ in 0..count {
        values.push(if entry_width == 0 {
            0
        } else {
            reader.uint(entry_width)? as u32
        });
    }
    Ok(values)
}

fn decode_names(charset: &[u32], names_data: &[u32], name_indexes: &[u32]) -> Vec<String> {
    let mut names = Vec::with_capacity(name_indexes.len());
    for &index in name_indexes {
        let mut bytes = Vec::new();
        let mut chr = names_data.get(index as usize).copied().unwrap_or(0);
        let mut guard = 0usize;
        while chr != 0 {
            let code = names_data.get(chr as usize).copied().unwrap_or(0);
            let delta = charset.get(code as usize).copied().unwrap_or(0);
            bytes.push(chr.wrapping_sub(delta) as u8);
            chr = code;
            guard += 1;
            if guard > names_data.len() {
                break;
            }
        }
        bytes.reverse();
        names.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    names
}

struct Parser<'a> {
    data: &'a [u8],
    header: Header,
    names: Vec<String>,
    string_offsets: Vec<u32>,
    strings: HashMap<u32, String>,
}

impl Parser<'_> {
    fn read_signed(&self, reader: &mut Reader, count: usize) -> Result<i64> {
        if count == 0 {
            return Ok(0);
        }
        let raw = reader.uint(count)?;
        if count >= 8 {
            return Ok(raw as i64);
        }
        let bits = count * 8;
        let sign_bit = 1u64 << (bits - 1);
        if raw & sign_bit != 0 {
            Ok((raw | (!0u64 << bits)) as i64)
        } else {
            Ok(raw as i64)
        }
    }

    fn load_string(&mut self, index: u32) -> Result<String> {
        if let Some(value) = self.strings.get(&index) {
            return Ok(value.clone());
        }
        let offset = self
            .string_offsets
            .get(index as usize)
            .copied()
            .ok_or_else(|| PsbError(format!("string index {index} out of range")))?;
        let start = self.header.offset_strings_data as usize + offset as usize;
        let mut reader = Reader::at(self.data, start);
        let value = reader.null_terminated_string()?;
        self.strings.insert(index, value.clone());
        Ok(value)
    }

    fn unpack(&mut self, reader: &mut Reader) -> Result<Option<PsbValue>> {
        let type_byte = reader.u8()?;
        match type_byte {
            0x00 => Ok(None),
            0x01 => Ok(Some(PsbValue::Null)),
            0x02 => Ok(Some(PsbValue::Bool(false))),
            0x03 => Ok(Some(PsbValue::Bool(true))),
            0x04..=0x0C => {
                let count = (type_byte - 0x04) as usize;
                Ok(Some(PsbValue::Int(self.read_signed(reader, count)?)))
            }
            0x0D..=0x14 => {
                let count_size = (type_byte - 0x0D + 1) as usize;
                Ok(Some(PsbValue::Array(read_list_body(reader, count_size)?)))
            }
            0x15..=0x18 => {
                let width = (type_byte - 0x15 + 1) as usize;
                let index = reader.uint(width)? as u32;
                Ok(Some(PsbValue::String(self.load_string(index)?)))
            }
            0x19..=0x1C => {
                let width = (type_byte - 0x19 + 1) as usize;
                let index = reader.uint(width)? as u32;
                Ok(Some(PsbValue::Resource(index, false)))
            }
            0x1D => Ok(Some(PsbValue::Float(0.0))),
            0x1E => {
                let bytes = reader.bytes(4)?;
                Ok(Some(PsbValue::Float(f32::from_le_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3],
                ]))))
            }
            0x1F => {
                let bytes = reader.bytes(8)?;
                Ok(Some(PsbValue::Double(f64::from_le_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                ]))))
            }
            0x20 => self.load_list(reader),
            0x21 => self.load_objects(reader),
            0x22..=0x25 => {
                let width = (type_byte - 0x22 + 1) as usize;
                let index = reader.uint(width)? as u32;
                Ok(Some(PsbValue::Resource(index, true)))
            }
            other => err(format!("unknown PSB object type {other:#x}")),
        }
    }

    fn load_list(&mut self, reader: &mut Reader) -> Result<Option<PsbValue>> {
        let count_size = read_count_size(reader.u8()?)?;
        let offsets = read_list_body(reader, count_size)?;
        let base = reader.pos;
        let mut items = Vec::with_capacity(offsets.len().min(1 << 16));
        for offset in offsets {
            reader.set_pos(base + offset as usize)?;
            if let Some(value) = self.unpack(reader)? {
                items.push(value);
            }
        }
        Ok(Some(PsbValue::List(items)))
    }

    fn load_objects(&mut self, reader: &mut Reader) -> Result<Option<PsbValue>> {
        let name_count_size = read_count_size(reader.u8()?)?;
        let name_indexes = read_list_body(reader, name_count_size)?;
        let offset_count_size = read_count_size(reader.u8()?)?;
        let offsets = read_list_body(reader, offset_count_size)?;
        let base = reader.pos;
        let mut entries = Vec::with_capacity(name_indexes.len().min(1 << 16));
        for (position, name_index) in name_indexes.iter().enumerate() {
            let Some(name) = self.names.get(*name_index as usize).cloned() else {
                continue;
            };
            let Some(offset) = offsets.get(position) else {
                continue;
            };
            reader.set_pos(base + *offset as usize)?;
            if let Some(value) = self.unpack(reader)? {
                entries.push((name, value));
            }
        }
        Ok(Some(PsbValue::Dict(entries)))
    }
}

/// Parse a decompressed PSB byte stream.
fn parse(mut data: Vec<u8>, seed: i64) -> Result<Psb> {
    let mut header = Header::parse(&mut Reader::at(&data, 0))?;
    if seed > 0 {
        apply_seed_decrypt(&mut header, &mut data, seed as u32);
    }
    if header.is_encrypted() && (header.offset_encrypt as usize) > data.len() && seed == 0 {
        return err("encrypted PSB file");
    }
    if header.version > 3 {
        return err(format!("unsupported PSB version {}", header.version));
    }

    let string_offsets = {
        let mut reader = Reader::at(&data, header.offset_strings as usize);
        let count_size = read_count_size(reader.u8()?)?;
        read_list_body(&mut reader, count_size)?
    };

    let mut parser = Parser {
        data: &data,
        header,
        names: Vec::new(),
        string_offsets,
        strings: HashMap::new(),
    };

    let mut reader = Reader::at(&data, header.offset_names as usize);
    let charset_count_size = read_count_size(reader.u8()?)?;
    let charset = read_list_body(&mut reader, charset_count_size)?;
    let names_count_size = read_count_size(reader.u8()?)?;
    let names_data = read_list_body(&mut reader, names_count_size)?;
    let indexes_count_size = read_count_size(reader.u8()?)?;
    let name_indexes = read_list_body(&mut reader, indexes_count_size)?;
    parser.names = decode_names(&charset, &names_data, &name_indexes);

    let root = {
        let mut reader = Reader::at(&data, header.offset_entries as usize);
        parser
            .unpack(&mut reader)?
            .ok_or_else(|| PsbError("root object is empty".into()))?
    };

    Ok(Psb { root })
}

/// Decompress (or pass through) an on-disk PSB/MDF stream.
fn open_stream(input: &[u8]) -> Result<Vec<u8>> {
    if input.len() < 9 {
        return err("truncated PSB stream header");
    }
    if input[..4] == LZ4_MAGIC {
        return decompress_lz4(input);
    }
    if &input[..3] == b"MDF" {
        return decompress_mdf(input);
    }
    Ok(input.to_vec())
}

fn decompress_mdf(input: &[u8]) -> Result<Vec<u8>> {
    let size = u32::from_le_bytes([input[4], input[5], input[6], input[7]]);
    let mut decoder = ZlibDecoder::new(&input[8..]);
    let mut output = Vec::with_capacity(size as usize);
    decoder
        .read_to_end(&mut output)
        .map_err(|error| PsbError(format!("stream decompression failed: {error}")))?;
    Ok(output)
}

fn decompress_lz4(input: &[u8]) -> Result<Vec<u8>> {
    if input.len() < 6 {
        return err("truncated LZ4 frame header");
    }
    let flags = input[4];
    if flags >> 6 != 1 {
        return err("invalid LZ4 frame version");
    }
    let block_checksum = flags & 0x10 != 0;
    let content_length = flags & 0x08 != 0;
    let content_checksum = flags & 0x04 != 0;
    let dictionary = flags & 0x01 != 0;
    let block_size = match (input[5] >> 4) & 7 {
        4 => 0x10000,
        5 => 0x40000,
        6 => 0x100000,
        7 => 0x400000,
        _ => return err("invalid LZ4 max block size"),
    };
    let mut offset = 6;
    if content_length {
        if input.len() < offset + 8 {
            return err("truncated LZ4 content length");
        }
        offset += 8;
    }
    if dictionary {
        if input.len() < offset + 4 {
            return err("truncated LZ4 dictionary id");
        }
        offset += 4;
    }
    if input.len() <= offset {
        return err("truncated LZ4 header checksum");
    }
    offset += 1;

    let mut output = Vec::new();
    let mut buffer = vec![0u8; block_size];
    loop {
        if input.len() < offset + 4 {
            return err("truncated LZ4 block header");
        }
        let size = i32::from_le_bytes([
            input[offset],
            input[offset + 1],
            input[offset + 2],
            input[offset + 3],
        ]);
        offset += 4;
        if size == 0 {
            break;
        }
        if size < 0 {
            let size = (size & 0x7fff_ffff) as usize;
            if input.len() < offset + size {
                return err("truncated LZ4 stored block");
            }
            output.extend_from_slice(&input[offset..offset + size]);
            offset += size;
        } else {
            let size = size as usize;
            if input.len() < offset + size {
                return err("truncated LZ4 compressed block");
            }
            let written = decompress_block(&input[offset..offset + size], &mut buffer)?;
            output.extend_from_slice(&buffer[..written]);
            offset += size;
        }
        if block_checksum {
            if input.len() < offset + 4 {
                return err("truncated LZ4 block checksum");
            }
            offset += 4;
        }
    }
    if content_checksum && offset > input.len() {
        return err("truncated LZ4 content checksum");
    }
    Ok(output)
}

fn decompress_block(block: &[u8], output: &mut [u8]) -> Result<usize> {
    let iend = block.len();
    let oend = output.len();
    let mut src = 0usize;
    let mut dst = 0usize;
    loop {
        let token = *block
            .get(src)
            .ok_or_else(|| PsbError("truncated LZ4 block".into()))?;
        src += 1;
        let mut length = (token >> 4) as usize;
        if length == 0xF {
            loop {
                let byte = *block
                    .get(src)
                    .ok_or_else(|| PsbError("truncated LZ4 literal length".into()))?;
                src += 1;
                length += byte as usize;
                if byte != 0xFF {
                    break;
                }
            }
        }
        let copy_end = dst + length;
        if copy_end > oend.saturating_sub(12) || src + length > iend.saturating_sub(8) {
            if src + length != iend || copy_end > oend {
                return err("invalid final LZ4 literal run");
            }
            output[dst..copy_end].copy_from_slice(&block[src..src + length]);
            dst = copy_end;
            break;
        }
        output[dst..copy_end].copy_from_slice(&block[src..src + length]);
        src += length;
        dst = copy_end;
        if block.len() < src + 2 {
            return err("truncated LZ4 match offset");
        }
        let offset = u16::from_le_bytes([block[src], block[src + 1]]) as usize;
        src += 2;
        let match_pos = dst
            .checked_sub(offset)
            .ok_or_else(|| PsbError("LZ4 match offset before output".into()))?;
        let mut match_length = (token & 0xF) as usize;
        if match_length == 0xF {
            loop {
                let byte = *block
                    .get(src)
                    .ok_or_else(|| PsbError("truncated LZ4 match length".into()))?;
                src += 1;
                if src > iend.saturating_sub(5) {
                    return err("invalid LZ4 match length run");
                }
                match_length += byte as usize;
                if byte != 0xFF {
                    break;
                }
            }
        }
        match_length += 4;
        let match_end = match_pos + match_length;
        let write_end = dst + match_length;
        if match_end > oend || write_end > oend {
            return err("LZ4 match overruns output");
        }
        output.copy_within(match_pos..match_end, dst);
        dst = write_end;
    }
    Ok(dst)
}

struct EmoteCtx {
    key: [u32; 4],
    v: u32,
}

impl EmoteCtx {
    fn new(seed: u32) -> Self {
        Self {
            key: [0x075B_CD15, 0x159A_55E5, 0x1F12_3BB5, seed],
            v: 0,
        }
    }

    fn decrypt(&mut self, data: &mut [u8]) {
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
            }
            *byte ^= self.v as u8;
            self.v >>= 8;
        }
    }
}

fn decrypt_u32(ctx: &mut EmoteCtx, value: &mut u32) {
    let mut bytes = value.to_le_bytes();
    ctx.decrypt(&mut bytes);
    *value = u32::from_le_bytes(bytes);
}

fn apply_seed_decrypt(header: &mut Header, data: &mut [u8], seed: u32) {
    let mut ctx = EmoteCtx::new(seed);
    if header.is_encrypted() && header.offset_encrypt > Header::MAX_LENGTH {
        for field in [
            &mut header.offset_encrypt,
            &mut header.offset_names,
            &mut header.offset_strings,
            &mut header.offset_strings_data,
            &mut header.offset_chunk_offsets,
            &mut header.offset_chunk_lengths,
            &mut header.offset_chunk_data,
            &mut header.offset_entries,
        ] {
            decrypt_u32(&mut ctx, field);
        }
    }
    if header.version == 2 {
        let start = header.offset_encrypt as usize;
        let end = header.offset_chunk_offsets as usize;
        if start < end && end <= data.len() {
            ctx.decrypt(&mut data[start..end]);
        }
    }
}

/// Parse a raw on-disk PSB/MDF stream.
pub(crate) fn parse_stream(raw: &[u8], seed: i64) -> Result<Psb> {
    parse(open_stream(raw)?, seed)
}
