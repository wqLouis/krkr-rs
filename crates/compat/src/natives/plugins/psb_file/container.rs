//! PSB/MDF container parsing: header, name/string tables, typed values,
//! resource chunks and type inference.
//!
//! This is a faithful port of the reference `PSBHeader.h`, `PSBValue.{h,cpp}`,
//! `PSBFile.cpp` (`loadKeys`/`loadNames`/`unpack`/`loadList`/`loadObjects`/
//! `loadResource`/`afterLoad`) and the `types/*` `isThisType` predicates. The
//! container is parsed eagerly into [`PsbValue`]; the raw decompressed bytes
//! are kept so chunk reads and string lookups stay available.

use std::collections::HashMap;

use super::decompress::{EmoteCtx, open_decompressed_stream};
use super::error::{PsbError, PsbResult};

/// The `PSB\0` magic.
const PSB_MAGIC: [u8; 4] = *b"PSB\0";
/// The `MDF\0` archive magic.
const MDF_MAGIC: [u8; 4] = *b"MDF\0";
/// The `MFL\0` archive magic.
const MFL_MAGIC: [u8; 4] = *b"MFL\0";

// ---------------------------------------------------------------------------
// Byte reader
// ---------------------------------------------------------------------------

/// A little-endian cursor over the decompressed container bytes.
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pub(crate) pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn at(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    pub(crate) fn len(&self) -> usize {
        self.data.len()
    }

    pub(crate) fn set_pos(&mut self, pos: usize) -> PsbResult<()> {
        if pos > self.data.len() {
            return Err(PsbError::Truncated("stream position"));
        }
        self.pos = pos;
        Ok(())
    }

    pub(crate) fn bytes(&mut self, count: usize) -> PsbResult<&'a [u8]> {
        let end = self
            .pos
            .checked_add(count)
            .ok_or(PsbError::Truncated("length"))?;
        let slice = self
            .data
            .get(self.pos..end)
            .ok_or(PsbError::Truncated("bytes"))?;
        self.pos = end;
        Ok(slice)
    }

    pub(crate) fn u8(&mut self) -> PsbResult<u8> {
        Ok(self.bytes(1)?[0])
    }

    pub(crate) fn u16(&mut self) -> PsbResult<u16> {
        let b = self.bytes(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub(crate) fn u32(&mut self) -> PsbResult<u32> {
        let b = self.bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Read `count` little-endian bytes into an unsigned integer (1..=8).
    pub(crate) fn uint(&mut self, count: usize) -> PsbResult<u64> {
        if count == 0 {
            return Ok(0);
        }
        if count > 8 {
            return Err(PsbError::Format(format!("bad integer width {count}")));
        }
        let bytes = self.bytes(count)?;
        let mut value = 0u64;
        for (index, byte) in bytes.iter().enumerate() {
            value |= u64::from(*byte) << (index * 8);
        }
        Ok(value)
    }

    /// Read a NUL-terminated string. A missing terminator yields the rest of
    /// the buffer (the reference `readStringZeroTrim` returns what it read).
    pub(crate) fn null_terminated_string(&mut self) -> PsbResult<String> {
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

// ---------------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------------

/// The fixed PSB header (`PSBHeader.h`).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PsbHeader {
    pub signature: [u8; 4],
    pub version: u16,
    pub encrypt: u16,
    pub offset_encrypt: u32,
    pub offset_names: u32,
    pub offset_strings: u32,
    pub offset_strings_data: u32,
    pub offset_chunk_offsets: u32,
    pub offset_chunk_lengths: u32,
    pub offset_chunk_data: u32,
    pub offset_entries: u32,
    pub checksum: u32,
    pub offset_extra_chunk_offsets: u32,
    pub offset_extra_chunk_lengths: u32,
    pub offset_extra_chunk_data: u32,
}

impl PsbHeader {
    /// The reference `PSBHeader::MAX_LENGTH`.
    pub(crate) const MAX_LENGTH: u32 = 56;

    /// Parse the header at the start of `reader`.
    pub(crate) fn parse(reader: &mut Reader) -> PsbResult<Self> {
        let mut header = Self::default();
        header.signature.copy_from_slice(reader.bytes(4)?);
        header.version = reader.u16()?;
        header.encrypt = reader.u16()?;
        header.offset_encrypt = reader.u32()?;
        header.offset_names = reader.u32()?;

        if header.signature == MDF_MAGIC || header.signature == MFL_MAGIC {
            return Err(PsbError::ArchiveNotPsb);
        }
        if header.signature != PSB_MAGIC {
            return Err(PsbError::BadSignature(header.signature));
        }

        if (header.offset_names as usize) < reader.len() {
            header.offset_strings = reader.u32()?;
            header.offset_strings_data = reader.u32()?;
            header.offset_chunk_offsets = reader.u32()?;
            header.offset_chunk_lengths = reader.u32()?;
            header.offset_chunk_data = reader.u32()?;
            header.offset_entries = reader.u32()?;
            if header.version > 2 {
                header.checksum = reader.u32()?;
            }
            if header.version > 3 {
                header.offset_extra_chunk_offsets = reader.u32()?;
                header.offset_extra_chunk_lengths = reader.u32()?;
                header.offset_extra_chunk_data = reader.u32()?;
            }
        }
        Ok(header)
    }

    /// The reference `PSBHeader::isEncrypted`.
    pub(crate) fn is_encrypted(&self) -> bool {
        self.offset_encrypt > Self::MAX_LENGTH + 16
            || self.offset_names == 0
            || (self.version > 1
                && self.offset_encrypt != self.offset_names
                && self.offset_encrypt != 0)
    }

    /// The reference `PSBHeader::GetHeaderLength`.
    pub(crate) fn header_length(&self) -> u32 {
        if self.version < 3 {
            40
        } else if self.version > 3 {
            56
        } else {
            44
        }
    }
}

// ---------------------------------------------------------------------------
// Typed values
// ---------------------------------------------------------------------------

/// A reference to a resource chunk (`PSBResource`), normal or extra.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ResourceRef {
    pub index: u32,
    pub extra: bool,
}

/// A parsed PSB value (`PSBValue.h`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PsbValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f32),
    Double(f64),
    Array(Vec<u32>),
    String(String),
    Resource(ResourceRef),
    List(Vec<PsbValue>),
    Dictionary(Vec<(String, PsbValue)>),
}

// ---------------------------------------------------------------------------
// Document
// ---------------------------------------------------------------------------

/// A fully parsed PSB/MDF container.
pub(crate) struct PsbDocument {
    pub root: PsbValue,
    pub resources: Vec<(ResourceRef, Vec<u8>)>,
}

impl PsbDocument {
    /// The bytes of a referenced resource, if present.
    pub(crate) fn resource_bytes(&self, reference: ResourceRef) -> Option<&[u8]> {
        self.resources
            .iter()
            .find(|(candidate, _)| *candidate == reference)
            .map(|(_, data)| data.as_slice())
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// The parsing state that survives across the recursive `unpack`.
struct Parser<'a> {
    data: &'a [u8],
    header: PsbHeader,
    names: Vec<String>,
    string_offsets: Vec<u32>,
    strings: HashMap<u32, String>,
    resources: Vec<ResourceRef>,
    extra_resources: Vec<ResourceRef>,
}

impl<'a> Parser<'a> {
    fn read_signed(&self, reader: &mut Reader, count: usize) -> PsbResult<i64> {
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

    /// Reference `loadString`: look the string up by index and cache it.
    fn load_string(&mut self, index: u32) -> PsbResult<String> {
        if let Some(value) = self.strings.get(&index) {
            return Ok(value.clone());
        }
        let offset = self
            .string_offsets
            .get(index as usize)
            .copied()
            .ok_or_else(|| PsbError::Format(format!("string index {index} out of range")))?;
        let start = self.header.offset_strings_data as usize + offset as usize;
        let mut reader = Reader::at(self.data, start);
        let value = reader.null_terminated_string()?;
        self.strings.insert(index, value.clone());
        Ok(value)
    }

    fn register_resource(&mut self, index: u32, extra: bool) -> ResourceRef {
        let reference = ResourceRef { index, extra };
        let list = if extra {
            &mut self.extra_resources
        } else {
            &mut self.resources
        };
        if !list.contains(&reference) {
            list.push(reference);
        }
        reference
    }

    /// Reference `unpack`.
    fn unpack(&mut self, reader: &mut Reader) -> PsbResult<Option<PsbValue>> {
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
                let reference = self.register_resource(index, false);
                Ok(Some(PsbValue::Resource(reference)))
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
                let reference = self.register_resource(index, true);
                Ok(Some(PsbValue::Resource(reference)))
            }
            other => {
                log::warn!("psb_file: unknown PSB object type {other:#x}");
                Ok(None)
            }
        }
    }

    /// Reference `loadList`.
    fn load_list(&mut self, reader: &mut Reader) -> PsbResult<Option<PsbValue>> {
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

    /// Reference `loadObjects` / `loadObjectsV1`.
    fn load_objects(&mut self, reader: &mut Reader) -> PsbResult<Option<PsbValue>> {
        if self.header.version == 1 {
            return self.load_objects_v1(reader);
        }
        let name_count_size = read_count_size(reader.u8()?)?;
        let name_indexes = read_list_body(reader, name_count_size)?;
        let offset_count_size = read_count_size(reader.u8()?)?;
        let offsets = read_list_body(reader, offset_count_size)?;
        let base = reader.pos;
        let mut entries = Vec::with_capacity(name_indexes.len().min(1 << 16));
        for (position, name_index) in name_indexes.iter().enumerate() {
            let Some(name) = self.names.get(*name_index as usize).cloned() else {
                log::warn!("psb_file: name index {name_index} out of range");
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
        Ok(Some(PsbValue::Dictionary(entries)))
    }

    /// Reference `loadObjectsV1`: each entry starts with its name index.
    fn load_objects_v1(&mut self, reader: &mut Reader) -> PsbResult<Option<PsbValue>> {
        let count_size = read_count_size(reader.u8()?)?;
        let offsets = read_list_body(reader, count_size)?;
        let base = reader.pos;
        let mut entries = Vec::with_capacity(offsets.len().min(1 << 16));
        for offset in offsets {
            reader.set_pos(base + offset as usize)?;
            let type_byte = reader.u8()?;
            let width = key_name_width(type_byte)?;
            let name_index = self.read_signed(reader, width)? as usize;
            let Some(name) = self.names.get(name_index).cloned() else {
                log::warn!("psb_file: v1 name index {name_index} out of range");
                continue;
            };
            if let Some(value) = self.unpack(reader)? {
                entries.push((name, value));
            }
        }
        Ok(Some(PsbValue::Dictionary(entries)))
    }
}

/// Decode the reference array count-size byte (`typeByte - ArrayN1 + 1`).
fn read_count_size(byte: u8) -> PsbResult<usize> {
    if (0x0D..=0x14).contains(&byte) {
        Ok((byte - 0x0D + 1) as usize)
    } else {
        Err(PsbError::Format(format!("bad array count size {byte:#x}")))
    }
}

/// Decode the name-index width of a PSB v1 object key type.
fn key_name_width(byte: u8) -> PsbResult<usize> {
    match byte {
        0x04..=0x0C => Ok((byte - 0x04) as usize),
        0x11..=0x14 => Ok((byte - 0x10) as usize),
        other => Err(PsbError::Format(format!("bad v1 key type {other:#x}"))),
    }
}

/// Read a `PSBArray`/`PSBList` body: count, entry width and entries.
fn read_list_body(reader: &mut Reader, count_size: usize) -> PsbResult<Vec<u32>> {
    let count = reader.uint(count_size)? as usize;
    if count > i32::MAX as usize {
        return Err(PsbError::Format(format!("array count {count} too large")));
    }
    let entry_byte = reader.u8()?;
    if entry_byte < 0x0C {
        return Err(PsbError::Format(format!(
            "bad array entry width {entry_byte:#x}"
        )));
    }
    let entry_width = (entry_byte - 0x0C) as usize;
    if entry_width > 8 {
        return Err(PsbError::Format(format!(
            "array entry width {entry_width} too large"
        )));
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

/// Decode the v2+ name table (`PSBFile::loadNames`).
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

/// Load the v1 name table (`PSBFile::loadKeys`).
fn load_keys(
    data: &[u8],
    header: &PsbHeader,
    name_indexes: &[u32],
    reader: &mut Reader,
) -> PsbResult<Vec<String>> {
    let mut names = Vec::with_capacity(name_indexes.len());
    for &index in name_indexes {
        reader.set_pos(header.offset_names as usize + index as usize)?;
        names.push(reader.null_terminated_string()?);
    }
    let _ = data;
    Ok(names)
}

/// Decrypt the header offset fields and the v2 payload region for a seed.
fn apply_seed_decrypt(header: &mut PsbHeader, data: &mut [u8], seed: u32) {
    let mut ctx = EmoteCtx::new(seed);
    if header.is_encrypted() && header.header_length() as usize > data.len() {
        decrypt_u32(&mut ctx, &mut header.offset_encrypt);
        decrypt_u32(&mut ctx, &mut header.offset_names);
        decrypt_u32(&mut ctx, &mut header.offset_strings);
        decrypt_u32(&mut ctx, &mut header.offset_strings_data);
        decrypt_u32(&mut ctx, &mut header.offset_chunk_offsets);
        decrypt_u32(&mut ctx, &mut header.offset_chunk_lengths);
        decrypt_u32(&mut ctx, &mut header.offset_chunk_data);
        decrypt_u32(&mut ctx, &mut header.offset_entries);
        if header.version > 2 {
            decrypt_u32(&mut ctx, &mut header.checksum);
        }
        if header.version > 3 {
            decrypt_u32(&mut ctx, &mut header.offset_extra_chunk_offsets);
            decrypt_u32(&mut ctx, &mut header.offset_extra_chunk_lengths);
            decrypt_u32(&mut ctx, &mut header.offset_extra_chunk_data);
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

fn decrypt_u32(ctx: &mut EmoteCtx, value: &mut u32) {
    let mut bytes = value.to_le_bytes();
    ctx.decrypt(&mut bytes);
    *value = u32::from_le_bytes(bytes);
}

/// Parse a decompressed PSB byte stream into a [`PsbDocument`].
///
/// `seed` is the Emote decryption seed. The reference registers no script
/// accessor for its `_seed`, so the port always passes `0`; with `0` the
/// stream is left encrypted and an encrypted container fails with
/// [`PsbError::Encrypted`].
pub(crate) fn parse(mut data: Vec<u8>, seed: i64) -> PsbResult<PsbDocument> {
    let mut header = {
        let mut reader = Reader::at(&data, 0);
        PsbHeader::parse(&mut reader)?
    };

    if seed > 0 {
        apply_seed_decrypt(&mut header, &mut data, seed as u32);
    }

    if header.signature != PSB_MAGIC {
        return Err(PsbError::BadSignature(header.signature));
    }
    if header.is_encrypted() && header.header_length() as usize > data.len() && seed == 0 {
        return Err(PsbError::Encrypted);
    }
    if header.version > 3 {
        return Err(PsbError::UnsupportedVersion(header.version));
    }

    // String offset table + names.
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
        resources: Vec::new(),
        extra_resources: Vec::new(),
    };

    if header.version == 1 {
        let mut reader = Reader::at(&data, 0);
        // v1 stores the name-index table at the "encrypt" offset.
        let mut offset = header.offset_encrypt;
        if offset as usize >= data.len() {
            offset = header.header_length();
        }
        reader.set_pos(offset as usize)?;
        let count_size = read_count_size(reader.u8()?)?;
        let name_indexes = read_list_body(&mut reader, count_size)?;
        parser.names = load_keys(&data, &header, &name_indexes, &mut reader)?;
    } else {
        let mut reader = Reader::at(&data, header.offset_names as usize);
        let charset_count_size = read_count_size(reader.u8()?)?;
        let charset = read_list_body(&mut reader, charset_count_size)?;
        let names_count_size = read_count_size(reader.u8()?)?;
        let names_data = read_list_body(&mut reader, names_count_size)?;
        let indexes_count_size = read_count_size(reader.u8()?)?;
        let name_indexes = read_list_body(&mut reader, indexes_count_size)?;
        parser.names = decode_names(&charset, &names_data, &name_indexes);
    }

    let chunk_offsets = {
        let mut reader = Reader::at(&data, header.offset_chunk_offsets as usize);
        let count_size = read_count_size(reader.u8()?)?;
        read_list_body(&mut reader, count_size)?
    };
    let chunk_lengths = {
        let mut reader = Reader::at(&data, header.offset_chunk_lengths as usize);
        let count_size = read_count_size(reader.u8()?)?;
        read_list_body(&mut reader, count_size)?
    };
    let (extra_chunk_offsets, extra_chunk_lengths) = if header.version >= 4 {
        let offsets = {
            let mut reader = Reader::at(&data, header.offset_extra_chunk_offsets as usize);
            let count_size = read_count_size(reader.u8()?)?;
            read_list_body(&mut reader, count_size)?
        };
        let lengths = {
            let mut reader = Reader::at(&data, header.offset_extra_chunk_lengths as usize);
            let count_size = read_count_size(reader.u8()?)?;
            read_list_body(&mut reader, count_size)?
        };
        (offsets, lengths)
    } else {
        (Vec::new(), Vec::new())
    };

    let root = {
        let mut reader = Reader::at(&data, header.offset_entries as usize);
        parser
            .unpack(&mut reader)?
            .ok_or_else(|| PsbError::Format("root object is empty".into()))?
    };

    let mut resources = Vec::new();
    load_chunks(
        &data,
        header.offset_chunk_data,
        &chunk_offsets,
        &chunk_lengths,
        &parser.resources,
        &mut resources,
    )?;
    let mut extra = Vec::new();
    load_chunks(
        &data,
        header.offset_extra_chunk_data,
        &extra_chunk_offsets,
        &extra_chunk_lengths,
        &parser.extra_resources,
        &mut extra,
    )?;
    resources.extend(extra);
    resources.sort_by_key(|(reference, _)| (reference.extra, reference.index));

    Ok(PsbDocument { root, resources })
}

/// Load the raw bytes of each referenced chunk (reference `loadResource` /
/// `loadExtraResource`).
fn load_chunks(
    data: &[u8],
    base: u32,
    offsets: &[u32],
    lengths: &[u32],
    references: &[ResourceRef],
    output: &mut Vec<(ResourceRef, Vec<u8>)>,
) -> PsbResult<()> {
    for reference in references {
        let offset = offsets
            .get(reference.index as usize)
            .copied()
            .ok_or_else(|| {
                PsbError::Format(format!("chunk index {} out of range", reference.index))
            })?;
        let length = lengths
            .get(reference.index as usize)
            .copied()
            .ok_or_else(|| {
                PsbError::Format(format!("chunk index {} out of range", reference.index))
            })?;
        let start = base as usize + offset as usize;
        let end = start
            .checked_add(length as usize)
            .ok_or(PsbError::Truncated("chunk data"))?;
        let bytes = data
            .get(start..end)
            .ok_or(PsbError::Truncated("chunk data"))?;
        output.push((*reference, bytes.to_vec()));
    }
    Ok(())
}

/// Decompress a raw file and parse it in one step.
pub(crate) fn parse_stream(raw: &[u8], seed: i64) -> PsbResult<PsbDocument> {
    parse(open_decompressed_stream(raw)?, seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a reference name-table encoding for a set of names.
    ///
    /// The reference stores each name as a path through a byte trie: a node's
    /// parent is `names_data[node]`, the byte on the edge is
    /// `node - charset[parent]`, and `name_indexes[i]` is a slot whose
    /// `names_data` entry is the terminal node. `decode_names` walks the chain
    /// back to the root and reverses it.
    fn encode_names(names: &[&str]) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
        type Children = std::collections::BTreeMap<u8, usize>;

        fn assign(
            node: usize,
            index: u32,
            children: &[Children],
            node_index: &mut [u32],
            charset: &mut Vec<u32>,
            names_data: &mut Vec<u32>,
            next_base: &mut u32,
        ) {
            if children[node].is_empty() {
                return;
            }
            // Reserve a 256-wide window so every child byte maps to a unique
            // node index that no other parent can reuse.
            let base = *next_base;
            *next_base += 256;
            if charset.len() <= index as usize {
                charset.resize(index as usize + 1, 0);
            }
            charset[index as usize] = base;
            let edges: Vec<(usize, u32)> = children[node]
                .iter()
                .map(|(&byte, &child)| {
                    let child_index = base + u32::from(byte);
                    if names_data.len() <= child_index as usize {
                        names_data.resize(child_index as usize + 1, 0);
                    }
                    names_data[child_index as usize] = index;
                    node_index[child] = child_index;
                    (child, child_index)
                })
                .collect();
            for (child, child_index) in edges {
                assign(
                    child,
                    child_index,
                    children,
                    node_index,
                    charset,
                    names_data,
                    next_base,
                );
            }
        }

        let mut children: Vec<Children> = vec![Children::new()];
        let mut terminals = Vec::with_capacity(names.len());
        for name in names {
            let mut node = 0usize;
            for byte in name.bytes() {
                node = if let Some(&child) = children[node].get(&byte) {
                    child
                } else {
                    children.push(Children::new());
                    let child = children.len() - 1;
                    children[node].insert(byte, child);
                    child
                };
            }
            terminals.push(node);
        }

        let mut charset = vec![0u32; 1];
        let mut names_data = vec![0u32; 1];
        let mut node_index = vec![0u32; children.len()];
        let mut next_base = 1u32;
        assign(
            0,
            0,
            &children,
            &mut node_index,
            &mut charset,
            &mut names_data,
            &mut next_base,
        );
        let indexes: Vec<u32> = terminals
            .into_iter()
            .map(|terminal| {
                let slot = next_base;
                next_base += 1;
                if names_data.len() <= slot as usize {
                    names_data.resize(slot as usize + 1, 0);
                }
                names_data[slot as usize] = node_index[terminal];
                slot
            })
            .collect();
        (charset, names_data, indexes)
    }

    #[test]
    fn decode_names_round_trips_ascii() {
        let (charset, names_data, indexes) = encode_names(&["num", "str", "arr"]);
        assert_eq!(
            decode_names(&charset, &names_data, &indexes),
            vec!["num".to_string(), "str".to_string(), "arr".to_string()]
        );
    }

    #[test]
    fn read_list_body_decodes_little_endian_entries() {
        // count size 1, count 2, entry width 2, values 0x0102 and 0x0304.
        let data = [2u8, 0x0E, 0x02, 0x01, 0x04, 0x03];
        let mut reader = Reader::at(&data, 0);
        assert_eq!(
            read_list_body(&mut reader, 1).unwrap(),
            vec![0x0102, 0x0304]
        );
    }
}
