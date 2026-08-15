//! XP3 archive reader.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use flate2::read::ZlibDecoder;

use crate::error::{Error, Result};
use crate::{
    INDEX_CONTINUE, INDEX_ENCODE_METHOD_MASK, INDEX_ENCODE_RAW, INDEX_ENCODE_ZLIB,
    SEGM_ENCODE_METHOD_MASK, SEGM_ENCODE_RAW, SEGM_ENCODE_ZLIB, XP3_MAGIC,
    normalize_in_archive_name,
};

/// One 28-byte segment of a file inside the archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    /// Encode method: [`SEGM_ENCODE_RAW`](crate::SEGM_ENCODE_RAW) or
    /// [`SEGM_ENCODE_ZLIB`](crate::SEGM_ENCODE_ZLIB).
    pub flags: u32,
    /// Absolute offset of the segment data in the archive file.
    pub start: u64,
    /// Uncompressed size.
    pub org_size: u64,
    /// Stored (compressed) size.
    pub arc_size: u64,
}

/// A single file entry inside the archive.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Raw flags from the `info` chunk (bit 31 = protected).
    pub flags: u32,
    /// Uncompressed total size.
    pub org_size: u64,
    /// Total stored size (sum of segment `arc_size`).
    pub arc_size: u64,
    /// Name as stored in the archive (not normalized).
    pub raw_name: String,
    /// Normalized in-archive name (lowercase, `/` separators).
    pub name: String,
    /// Segments composing the file, in order.
    pub segments: Vec<Segment>,
    /// Adler32 hash of the name from the `aldr` chunk.
    pub file_hash: u32,
}

/// An opened XP3 archive.
pub struct Xp3Archive {
    path: PathBuf,
    file: File,
    /// Base offset of the archive inside the file (non-zero for EXE-embedded).
    base: u64,
    entries: BTreeMap<String, Entry>,
}

impl Xp3Archive {
    /// Open an XP3 archive from a file on disk.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut file = File::open(&path)?;

        // Read the 11-byte magic.
        let mut magic = [0u8; 11];
        file.read_exact(&mut magic)
            .map_err(|_| Error::NotAnXp3(path.clone()))?;

        let base = if magic[..2] == *b"MZ" {
            // EXE-embedded: search for the XP3 mark on 16-byte alignment.
            find_xp3_offset(&mut file)?
        } else if magic == XP3_MAGIC {
            0
        } else {
            return Err(Error::NotAnXp3(path.clone()));
        };

        let mut archive = Xp3Archive {
            path,
            file,
            base,
            entries: BTreeMap::new(),
        };
        archive.read_index()?;
        Ok(archive)
    }

    /// Absolute path of the archive file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Base offset of the archive data inside the file (0 for plain files).
    pub fn base_offset(&self) -> u64 {
        self.base
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the archive has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over all entries in normalized-name order.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }

    /// Look up an entry by normalized name (lowercase, `/` separators).
    pub fn entry(&self, name: &str) -> Option<&Entry> {
        self.entries.get(&normalize_in_archive_name(name))
    }

    /// Read the full contents of an entry by normalized name.
    pub fn read(&mut self, name: &str) -> Result<Vec<u8>> {
        let normalized = normalize_in_archive_name(name);
        let entry = self
            .entries
            .get(&normalized)
            .ok_or_else(|| Error::NotFound(name.to_string()))?
            .clone();
        self.read_entry(&entry)
    }

    /// Read the full contents of an entry.
    pub fn read_entry(&mut self, entry: &Entry) -> Result<Vec<u8>> {
        // Note: `protected` (DRM bit) files are read normally — matching the
        // reference emulator, which sets TVPAllowExtractProtectedStorage=true.
        let mut out = Vec::with_capacity(entry.org_size as usize);
        for seg in &entry.segments {
            let mut buf = vec![0u8; seg.arc_size as usize];
            self.file.seek(SeekFrom::Start(self.base + seg.start))?;
            self.file.read_exact(&mut buf)?;

            let method = seg.flags & SEGM_ENCODE_METHOD_MASK;
            match method {
                SEGM_ENCODE_RAW => out.extend_from_slice(&buf),
                SEGM_ENCODE_ZLIB => {
                    let mut dec = ZlibDecoder::new(&buf[..]);
                    let mut chunk = vec![0u8; seg.org_size as usize];
                    dec.read_exact(&mut chunk)
                        .map_err(|_| Error::Inflate(entry.raw_name.clone()))?;
                    out.extend_from_slice(&chunk);
                }
                other => return Err(Error::UnknownSegmentMethod(other, seg.flags)),
            }
        }
        debug_assert_eq!(out.len() as u64, entry.org_size);
        Ok(out)
    }

    // -- internals ----------------------------------------------------------

    /// Read the (possibly chained) index blocks, replicating the reference
    /// engine's loop: the next index offset is re-read from position 11.
    fn read_index(&mut self) -> Result<()> {
        let mut chain = 0usize;
        loop {
            chain += 1;
            if chain > 256 {
                return Err(Error::IndexChainTooLong(chain));
            }

            self.file.seek(SeekFrom::Start(self.base + 11))?;
            let index_ofs = read_u64_le(&mut self.file)?;
            if index_ofs == 0 {
                if self.entries.is_empty() {
                    return Err(Error::NoIndex);
                }
                break;
            }

            self.file.seek(SeekFrom::Start(self.base + index_ofs))?;
            let flag = read_u8(&mut self.file)?;
            let data = match flag & INDEX_ENCODE_METHOD_MASK {
                INDEX_ENCODE_RAW => {
                    let size = read_u64_le(&mut self.file)?;
                    read_exact_vec(&mut self.file, size)?
                }
                INDEX_ENCODE_ZLIB => {
                    let compressed_size = read_u64_le(&mut self.file)?;
                    let real_size = read_u64_le(&mut self.file)?;
                    let compressed = read_exact_vec(&mut self.file, compressed_size)?;
                    let mut dec = ZlibDecoder::new(&compressed[..]);
                    let mut data = vec![0u8; real_size as usize];
                    dec.read_exact(&mut data)
                        .map_err(|_| Error::CorruptIndex("zlib decompression failed"))?;
                    data
                }
                other => return Err(Error::BadIndexFlag(flag | other)),
            };

            self.parse_index(&data)?;

            if flag & INDEX_CONTINUE == 0 {
                break;
            }
        }
        Ok(())
    }

    /// Parse one index block: chunks of `[4-byte tag][u64 LE size][data]`.
    fn parse_index(&mut self, data: &[u8]) -> Result<()> {
        let mut pos = 0usize;
        while pos + 12 <= data.len() {
            let tag = &data[pos..pos + 4];
            let size = u64::from_le_bytes(data[pos + 4..pos + 12].try_into().unwrap());
            let size = usize::try_from(size).map_err(|_| Error::CorruptIndex("chunk too large"))?;
            pos += 12;
            if pos + size > data.len() {
                return Err(Error::CorruptIndex("chunk overruns index data"));
            }
            let payload = &data[pos..pos + size];
            pos += size;

            if tag == b"File" {
                self.parse_file_chunk(payload)?;
            }
        }
        Ok(())
    }

    /// Parse one `File` chunk: `info` / `segm` / `aldr` sub-chunks.
    fn parse_file_chunk(&mut self, data: &[u8]) -> Result<()> {
        let mut pos = 0usize;
        let mut flags = 0u32;
        let mut org_size = 0u64;
        let mut arc_size = 0u64;
        let mut raw_name = String::new();
        let mut segments = Vec::new();
        let mut file_hash = 0u32;

        while pos + 12 <= data.len() {
            let tag = &data[pos..pos + 4];
            let size = u64::from_le_bytes(data[pos + 4..pos + 12].try_into().unwrap());
            let size =
                usize::try_from(size).map_err(|_| Error::CorruptIndex("sub-chunk too large"))?;
            pos += 12;
            if pos + size > data.len() {
                return Err(Error::CorruptIndex("sub-chunk overruns File chunk"));
            }
            let payload = &data[pos..pos + size];
            pos += size;

            match tag {
                b"info" => {
                    if payload.len() < 22 {
                        return Err(Error::CorruptIndex("info sub-chunk too short"));
                    }
                    flags = u32::from_le_bytes(payload[0..4].try_into().unwrap());
                    org_size = u64::from_le_bytes(payload[4..12].try_into().unwrap());
                    arc_size = u64::from_le_bytes(payload[12..20].try_into().unwrap());
                    let name_len = i16::from_le_bytes(payload[20..22].try_into().unwrap()) as usize;
                    let name_bytes = payload
                        .get(22..22 + name_len * 2)
                        .ok_or(Error::CorruptIndex("name overruns info sub-chunk"))?;
                    let mut units = Vec::with_capacity(name_len);
                    for c in name_bytes.chunks_exact(2) {
                        units.push(u16::from_le_bytes([c[0], c[1]]));
                    }
                    raw_name = String::from_utf16_lossy(&units);
                }
                b"segm" => {
                    if !payload.len().is_multiple_of(28) {
                        return Err(Error::CorruptIndex(
                            "segm sub-chunk size not multiple of 28",
                        ));
                    }
                    for s in payload.chunks_exact(28) {
                        segments.push(Segment {
                            flags: u32::from_le_bytes(s[0..4].try_into().unwrap()),
                            start: u64::from_le_bytes(s[4..12].try_into().unwrap()),
                            org_size: u64::from_le_bytes(s[12..20].try_into().unwrap()),
                            arc_size: u64::from_le_bytes(s[20..28].try_into().unwrap()),
                        });
                    }
                }
                b"aldr" => {
                    // SAFETY: length checked by the match guard below.
                    if let Some(h) = payload.get(0..4) {
                        file_hash = u32::from_le_bytes(h.try_into().unwrap());
                    }
                }
                _ => { /* unknown sub-chunk: ignore */ }
            }
        }

        let name = normalize_in_archive_name(&raw_name);
        self.entries.insert(
            name.clone(),
            Entry {
                flags,
                org_size,
                arc_size,
                raw_name,
                name,
                segments,
                file_hash,
            },
        );
        Ok(())
    }
}

/// Search for the XP3 magic in an EXE (MZ) file on 16-byte alignment,
/// starting at offset 16. Returns the archive base offset.
fn find_xp3_offset(file: &mut File) -> Result<u64> {
    const CHUNK: usize = 256 * 1024;
    let mut offset = 16u64;
    file.seek(SeekFrom::Start(offset))?;
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        let mut p = 0usize;
        while p + 11 <= n {
            if buffer[p..p + 11] == XP3_MAGIC {
                return Ok(offset + p as u64);
            }
            p += 16;
        }
        // The magic may straddle the boundary; rewind 16 bytes to realign.
        offset += n as u64;
        if n == CHUNK {
            offset = offset.saturating_sub(16);
            file.seek(SeekFrom::Start(offset))?;
        }
    }
    Err(Error::NotAnXp3(PathBuf::new()))
}

fn read_u8<R: Read>(r: &mut R) -> Result<u8> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b)?;
    Ok(b[0])
}

fn read_u64_le<R: Read>(r: &mut R) -> Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn read_exact_vec<R: Read>(r: &mut R, len: u64) -> Result<Vec<u8>> {
    let len = usize::try_from(len).map_err(|_| Error::CorruptIndex("index too large"))?;
    let mut v = vec![0u8; len];
    r.read_exact(&mut v)?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_names() {
        assert_eq!(normalize_in_archive_name("Startup.tjs"), "startup.tjs");
        assert_eq!(normalize_in_archive_name(r"data\bg\a.jpg"), "data/bg/a.jpg");
        assert_eq!(
            normalize_in_archive_name("data//bg///a.jpg"),
            "data/bg/a.jpg"
        );
        assert_eq!(
            normalize_in_archive_name("//leading/slash"),
            "/leading/slash"
        );
    }
}
