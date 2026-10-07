//! XP3 archive reader — L1 stateless container parsing.
//!
//! This is a port of the standalone `xp3` crate reader onto the L1
//! [`FileState`] surface: instead of owning a live `std::fs::File`, the
//! parser reads through a small [`FileReader`] adapter that turns positional
//! [`read_at`] calls into the `Read + Seek` interface the original parser
//! logic expects. The on-disk format behaviour — magic, index chaining, raw
//! and zlib index/segment encodings, name normalization, EXE-embedded search
//! — is preserved.
//!
//! The [`Xp3Archive`] value stores only parsed index metadata. Every read
//! takes the owning [`FileSystem`] and [`FileState`] explicitly, so no file
//! handle is hidden inside the archive and the caller (L2) keeps all state.
//! Parsing holds no state of its own: given the bytes through a `FileState`
//! it returns a parsed index, per `docs/migration/02-target-architecture.md`
//! §5 and `docs/migration/09-game-loading.md`.
//!
//! ## Format summary
//!
//! * 11-byte magic: `"XP3\r\n \n\x1a\x8b\x67\x01"`.
//! * At offset 11: 8-byte little-endian *index offset* relative to the
//!   archive start (for EXE-embedded archives the magic is searched on
//!   16-byte alignment and the offset is relative to the found magic).
//! * The index block: 1 flag byte + (raw or zlib-compressed) index data.
//!   Flag bits: low 3 = encode method (`0` raw, `1` zlib), `0x80` = continue.
//! * Index data is a sequence of chunks `[4-byte tag][u64 LE size][data]`:
//!   `File` chunks hold `info` / `segm` / `adlr` sub-chunks.
//! * In-archive names are normalized: lowercased, `\` → `/`, duplicate
//!   slashes collapsed, leading slashes dropped.
//! * Reading a file concatenates its segments; zlib segments are inflated to
//!   `org_size`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Seek, SeekFrom};

use api::error::FsError;
use flate2::read::ZlibDecoder;

use crate::{FileState, FileSystem, file_size, read_at};

/// 11-byte XP3 magic: `"XP3\r\n \n\x1a\x8b\x67\x01"`.
pub const XP3_MAGIC: [u8; 11] = [
    0x58, 0x50, 0x33, // 'X','P','3'
    0x0d, 0x0a, 0x20, 0x0a, 0x1a, // \r \n ' ' \n EOF
    0x8b, 0x67, 0x01, // KANJI-code + version/coding
];

/// Index encode method mask (low 3 bits of the index flag byte).
pub const INDEX_ENCODE_METHOD_MASK: u8 = 0x07;
/// Index stored raw.
pub const INDEX_ENCODE_RAW: u8 = 0;
/// Index stored zlib-compressed.
pub const INDEX_ENCODE_ZLIB: u8 = 1;
/// Index continue bit — another index block follows.
pub const INDEX_CONTINUE: u8 = 0x80;

/// Segment encode method mask.
pub const SEGM_ENCODE_METHOD_MASK: u32 = 0x07;
/// Segment stored raw.
pub const SEGM_ENCODE_RAW: u32 = 0;
/// Segment stored zlib-compressed.
pub const SEGM_ENCODE_ZLIB: u32 = 1;

/// Errors produced while opening or reading an XP3 archive.
#[derive(Debug, thiserror::Error)]
pub enum Xp3Error {
    /// An underlying I/O read failed.
    #[error("I/O error: {0}")]
    Io(std::io::Error),

    /// A bottom file-system read failed. Carried through the `Read`/`Seek`
    /// adapter so `FsError::NotFound` and friends stay matchable instead of
    /// being erased into an opaque I/O string.
    #[error(transparent)]
    Fs(#[from] FsError),

    /// The file does not start with the XP3 magic (nor an MZ header
    /// containing one).
    #[error("not an XP3 archive")]
    NotAnXp3,

    /// The index flag byte has an unknown encode method.
    #[error("bad index flag 0x{0:02x}")]
    BadIndexFlag(u8),

    /// The index data is malformed.
    #[error("corrupt index data: {0}")]
    CorruptIndex(&'static str),

    /// A segment uses an unknown encode method.
    #[error("unknown segment encode method {0} (flags 0x{1:08x})")]
    UnknownSegmentMethod(u32, u32),

    /// A zlib segment failed to inflate.
    #[error("zlib decompression failed for segment of `{0}`")]
    Inflate(String),

    /// The requested entry does not exist.
    #[error("entry not found in archive: {0}")]
    NotFound(String),

    /// The archive has no index block.
    #[error("archive has no index (empty?)")]
    NoIndex,
}

/// Convenience alias for XP3 results.
pub type Result<T> = std::result::Result<T, Xp3Error>;

impl From<std::io::Error> for Xp3Error {
    /// Recover a [`FsError`] wrapped by [`FileReader`] before falling back to
    /// the opaque I/O variant.
    fn from(error: std::io::Error) -> Self {
        match error.get_ref().and_then(|e| e.downcast_ref::<FsError>()) {
            Some(fs) => Xp3Error::Fs(fs.clone()),
            None => Xp3Error::Io(error),
        }
    }
}

/// One 28-byte segment of a file inside the archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    /// Encode method: [`SEGM_ENCODE_RAW`] or [`SEGM_ENCODE_ZLIB`].
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
    /// Adler32 hash of the name from the `adlr` chunk.
    pub file_hash: u32,
}

/// An opened XP3 archive index.
///
/// The value stores the archive base offset and the parsed entry map; it does
/// **not** hold a file handle. Reads receive the [`FileSystem`] and
/// [`FileState`] that own the underlying bytes.
pub struct Xp3Archive {
    /// Base offset of the archive inside the file (non-zero for
    /// EXE-embedded archives).
    base: u64,
    /// Entries keyed by normalized name.
    entries: BTreeMap<String, Entry>,
}

impl Xp3Archive {
    /// Parse an XP3 index from an already-open [`FileState`].
    pub fn open(fs: &FileSystem, file: &FileState) -> Result<Self> {
        let mut reader = FileReader { fs, file, pos: 0 };
        Self::open_reader(&mut reader)
    }

    /// Parse an XP3 index from any byte stream.
    ///
    /// This is the reusable entry point behind [`open`](Self::open). The
    /// stream is read from its current position; for an EXE-embedded archive
    /// the 16-byte-aligned magic is located within it and the returned base
    /// offset is relative to that magic. It is generic over [`Read`] +
    /// [`Seek`] so callers other than the L1 [`FileState`] surface (for
    /// example an EXE/APK wrapper or a positional adapter) can parse an
    /// archive without materializing a full [`FileState`].
    pub fn open_reader<R: Read + Seek>(reader: &mut R) -> Result<Self> {
        // Read the 11-byte magic. A file shorter than the magic is simply not
        // an XP3 archive; a genuine read failure is preserved as its typed
        // error instead of being erased into `NotAnXp3`.
        let mut magic = [0u8; 11];
        reader.read_exact(&mut magic).map_err(|error| {
            if error.kind() == io::ErrorKind::UnexpectedEof {
                Xp3Error::NotAnXp3
            } else {
                Xp3Error::from(error)
            }
        })?;

        let base = if magic[..2] == *b"MZ" {
            // EXE-embedded: search for the XP3 mark on 16-byte alignment.
            find_xp3_offset(reader)?
        } else if magic == XP3_MAGIC {
            0
        } else {
            return Err(Xp3Error::NotAnXp3);
        };

        let mut archive = Xp3Archive {
            base,
            entries: BTreeMap::new(),
        };
        archive.read_index(reader)?;
        Ok(archive)
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

    /// Look up an entry by name (normalized before lookup).
    pub fn entry(&self, name: &str) -> Option<&Entry> {
        self.entries.get(&normalize_in_archive_name(name))
    }

    /// Read the full contents of an entry by name.
    pub fn read(&self, fs: &FileSystem, file: &FileState, name: &str) -> Result<Vec<u8>> {
        let normalized = normalize_in_archive_name(name);
        let entry = self
            .entries
            .get(&normalized)
            .ok_or_else(|| Xp3Error::NotFound(name.to_string()))?;
        self.read_entry(fs, file, entry)
    }

    /// Read the full contents of an entry.
    pub fn read_entry(&self, fs: &FileSystem, file: &FileState, entry: &Entry) -> Result<Vec<u8>> {
        // Note: `protected` (DRM bit) files are read normally, matching the
        // reference emulator, which allows extracting protected storage.
        let mut reader = FileReader { fs, file, pos: 0 };
        // Size the output from the segments' own declared sizes rather than
        // the entry header. The sum is checked for overflow and the
        // allocation is fallible, so a structurally invalid size is an error
        // rather than an unbounded allocation (no arbitrary maximum).
        let expected = entry.segments.iter().try_fold(0u64, |total, seg| {
            let size = if seg.flags & SEGM_ENCODE_METHOD_MASK == SEGM_ENCODE_ZLIB {
                seg.org_size
            } else {
                seg.arc_size
            };
            total
                .checked_add(size)
                .ok_or(Xp3Error::CorruptIndex("entry size overflow"))
        })?;
        let expected = usize::try_from(expected)
            .map_err(|_| Xp3Error::CorruptIndex("entry size does not fit usize"))?;
        let mut out = Vec::new();
        out.try_reserve_exact(expected)
            .map_err(|_| Xp3Error::CorruptIndex("entry size cannot be allocated"))?;
        for seg in &entry.segments {
            let arc_len = usize::try_from(seg.arc_size)
                .map_err(|_| Xp3Error::CorruptIndex("segment too large"))?;
            let mut buf = alloc_zeroed(arc_len, "segment too large")?;
            let start = self
                .base
                .checked_add(seg.start)
                .ok_or(Xp3Error::CorruptIndex("segment offset overflow"))?;
            reader.seek(SeekFrom::Start(start))?;
            reader.read_exact(&mut buf)?;

            let method = seg.flags & SEGM_ENCODE_METHOD_MASK;
            match method {
                SEGM_ENCODE_RAW => out.extend_from_slice(&buf),
                SEGM_ENCODE_ZLIB => {
                    let org_len = usize::try_from(seg.org_size)
                        .map_err(|_| Xp3Error::CorruptIndex("segment too large"))?;
                    let mut dec = ZlibDecoder::new(&buf[..]);
                    let mut chunk = alloc_zeroed(org_len, "segment too large")?;
                    dec.read_exact(&mut chunk)
                        .map_err(|_| Xp3Error::Inflate(entry.raw_name.clone()))?;
                    out.extend_from_slice(&chunk);
                }
                other => return Err(Xp3Error::UnknownSegmentMethod(other, seg.flags)),
            }
        }
        // The entry header's `org_size` must agree with what the segments
        // actually compose; otherwise `stat` would report one size and `read`
        // return another. This is a real check, not a debug assertion.
        if out.len() as u64 != entry.org_size {
            return Err(Xp3Error::CorruptIndex(
                "segment sizes disagree with info.org_size",
            ));
        }
        Ok(out)
    }

    // -- internals ----------------------------------------------------------

    /// Read the (possibly chained) index blocks, replicating the reference
    /// engine's loop: the first index offset is read from offset 11, and after
    /// a block with `INDEX_CONTINUE` the next offset is read from the current
    /// position (immediately after that block's data).
    fn read_index<R: Read + Seek>(&mut self, reader: &mut R) -> Result<()> {
        let start = self
            .base
            .checked_add(11)
            .ok_or(Xp3Error::CorruptIndex("index offset overflow"))?;
        reader.seek(SeekFrom::Start(start))?;
        // No arbitrary chain bound (D11): a real chain is finite. Termination
        // and corruption detection come from remembering every resolved index
        // offset and rejecting a repeat as a cycle.
        let mut visited: BTreeSet<u64> = BTreeSet::new();
        loop {
            let index_ofs = read_u64_le(reader)?;
            if index_ofs == 0 {
                if self.entries.is_empty() {
                    return Err(Xp3Error::NoIndex);
                }
                break;
            }

            let block = self
                .base
                .checked_add(index_ofs)
                .ok_or(Xp3Error::CorruptIndex("index offset overflow"))?;
            if !visited.insert(block) {
                return Err(Xp3Error::CorruptIndex("index chain cycle"));
            }
            reader.seek(SeekFrom::Start(block))?;
            let flag = read_u8(reader)?;
            let data = match flag & INDEX_ENCODE_METHOD_MASK {
                INDEX_ENCODE_RAW => {
                    let size = read_u64_le(reader)?;
                    read_exact_vec(reader, size)?
                }
                INDEX_ENCODE_ZLIB => {
                    let compressed_size = read_u64_le(reader)?;
                    let real_size = read_u64_le(reader)?;
                    let compressed = read_exact_vec(reader, compressed_size)?;
                    let mut dec = ZlibDecoder::new(&compressed[..]);
                    let real_len = usize::try_from(real_size)
                        .map_err(|_| Xp3Error::CorruptIndex("index too large"))?;
                    let mut data = alloc_zeroed(real_len, "index too large")?;
                    dec.read_exact(&mut data)
                        .map_err(|_| Xp3Error::CorruptIndex("zlib decompression failed"))?;
                    data
                }
                other => return Err(Xp3Error::BadIndexFlag(flag | other)),
            };

            self.parse_index(&data)?;

            // On CONTINUE the stream is left right after this block's data,
            // where the next index offset is stored.
            if flag & INDEX_CONTINUE == 0 {
                break;
            }
        }
        Ok(())
    }

    /// Parse one index block: chunks of `[4-byte tag][u64 LE size][data]`.
    fn parse_index(&mut self, data: &[u8]) -> Result<()> {
        let mut pos = 0usize;
        while data.len().saturating_sub(pos) >= 12 {
            let tag = &data[pos..pos + 4];
            let size = u64::from_le_bytes(data[pos + 4..pos + 12].try_into().unwrap());
            let size =
                usize::try_from(size).map_err(|_| Xp3Error::CorruptIndex("chunk too large"))?;
            pos += 12;
            let end = pos
                .checked_add(size)
                .ok_or(Xp3Error::CorruptIndex("chunk size overflow"))?;
            if end > data.len() {
                return Err(Xp3Error::CorruptIndex("chunk overruns index data"));
            }
            let payload = &data[pos..end];
            pos = end;

            if tag == b"File" {
                self.parse_file_chunk(payload)?;
            }
        }
        // A truncated chunk header or payload leaves trailing bytes; a
        // structurally complete index consumes every byte exactly.
        if pos != data.len() {
            return Err(Xp3Error::CorruptIndex("trailing bytes in index data"));
        }
        Ok(())
    }

    /// Parse one `File` chunk: `info` / `segm` / `adlr` sub-chunks.
    fn parse_file_chunk(&mut self, data: &[u8]) -> Result<()> {
        let mut pos = 0usize;
        let mut flags = 0u32;
        let mut org_size = 0u64;
        let mut arc_size = 0u64;
        let mut raw_name = String::new();
        let mut segments = Vec::new();
        let mut file_hash = 0u32;
        let mut seen_info = false;
        let mut seen_segm = false;
        let mut seen_adlr = false;

        while data.len().saturating_sub(pos) >= 12 {
            let tag = &data[pos..pos + 4];
            let size = u64::from_le_bytes(data[pos + 4..pos + 12].try_into().unwrap());
            let size =
                usize::try_from(size).map_err(|_| Xp3Error::CorruptIndex("sub-chunk too large"))?;
            pos += 12;
            let end = pos
                .checked_add(size)
                .ok_or(Xp3Error::CorruptIndex("sub-chunk size overflow"))?;
            if end > data.len() {
                return Err(Xp3Error::CorruptIndex("sub-chunk overruns File chunk"));
            }
            let payload = &data[pos..end];
            pos = end;

            match tag {
                b"info" => {
                    if seen_info {
                        return Err(Xp3Error::CorruptIndex("duplicate info sub-chunk"));
                    }
                    seen_info = true;
                    if payload.len() < 22 {
                        return Err(Xp3Error::CorruptIndex("info sub-chunk too short"));
                    }
                    flags = u32::from_le_bytes(payload[0..4].try_into().unwrap());
                    org_size = u64::from_le_bytes(payload[4..12].try_into().unwrap());
                    arc_size = u64::from_le_bytes(payload[12..20].try_into().unwrap());
                    let name_len = i16::from_le_bytes(payload[20..22].try_into().unwrap());
                    if name_len < 0 {
                        return Err(Xp3Error::CorruptIndex("negative name length"));
                    }
                    let name_len = name_len as usize;
                    let name_bytes = payload
                        .get(22..22 + name_len * 2)
                        .ok_or(Xp3Error::CorruptIndex("name overruns info sub-chunk"))?;
                    let mut units = Vec::with_capacity(name_len);
                    for c in name_bytes.chunks_exact(2) {
                        units.push(u16::from_le_bytes([c[0], c[1]]));
                    }
                    raw_name = String::from_utf16_lossy(&units);
                }
                b"segm" => {
                    if seen_segm {
                        return Err(Xp3Error::CorruptIndex("duplicate segm sub-chunk"));
                    }
                    seen_segm = true;
                    if payload.is_empty() {
                        return Err(Xp3Error::CorruptIndex("empty segm sub-chunk"));
                    }
                    if !payload.len().is_multiple_of(28) {
                        return Err(Xp3Error::CorruptIndex(
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
                b"adlr" => {
                    if seen_adlr {
                        return Err(Xp3Error::CorruptIndex("duplicate adlr sub-chunk"));
                    }
                    seen_adlr = true;
                    if let Some(h) = payload.get(0..4) {
                        file_hash = u32::from_le_bytes(h.try_into().unwrap());
                    }
                }
                _ => { /* unknown sub-chunk: ignore */ }
            }
        }

        // A `File` chunk is only a valid entry when all three required
        // sub-chunks are present exactly once; silently defaulting them would
        // create an addressable entry with a bogus size or no data.
        if pos != data.len() {
            return Err(Xp3Error::CorruptIndex("trailing bytes in File chunk"));
        }
        if !seen_info || !seen_segm || !seen_adlr {
            return Err(Xp3Error::CorruptIndex("File chunk missing info/segm/adlr"));
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

/// Normalize an in-archive storage name the way the reference engine does:
/// lowercase, `\` → `/`, collapse runs of `/`, and **drop leading slashes**.
///
/// This mirrors `tTVPArchive::NormalizeInArchiveStorageName`
/// (`reference/cpp/core/base/StorageIntf.cpp`): the very first `/` is skipped
/// together with any following ones.
pub fn normalize_in_archive_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut started = false;
    let mut prev_slash = false;
    for c in name.chars() {
        let c = match c {
            'A'..='Z' => c.to_ascii_lowercase(),
            '\\' => '/',
            _ => c,
        };
        if c == '/' {
            // Leading slashes are dropped; interior runs collapse to one.
            if started && !prev_slash {
                out.push('/');
                prev_slash = true;
            }
        } else {
            out.push(c);
            started = true;
            prev_slash = false;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Positional reader adapter
// ---------------------------------------------------------------------------

/// Adapts a `(FileSystem, FileState)` pair to `std::io::Read + Seek`.
///
/// The original XP3 parser was written against a live `std::fs::File`; this
/// adapter presents the same interface over positional [`read_at`] calls,
/// keeping the parser logic a direct port. The private cursor is local to the
/// adapter and is never stored in the bottom [`FileSystem`].
struct FileReader<'a> {
    /// Bottom file-system object owning the handle.
    fs: &'a FileSystem,
    /// Caller-owned open handle.
    file: &'a FileState,
    /// Local cursor for sequential `Read`.
    pos: u64,
}

impl Read for FileReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = read_at(self.fs, self.file, self.pos, buf).map_err(io::Error::other)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for FileReader<'_> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let target: i128 = match pos {
            SeekFrom::Start(off) => off as i128,
            SeekFrom::End(off) => {
                file_size(self.fs, self.file).map_err(io::Error::other)? as i128 + off as i128
            }
            SeekFrom::Current(off) => self.pos as i128 + off as i128,
        };
        self.pos = target.max(0) as u64;
        Ok(self.pos)
    }
}

// ---------------------------------------------------------------------------
// Format helpers
// ---------------------------------------------------------------------------

/// Search for the XP3 magic in an EXE (MZ) file on 16-byte alignment,
/// starting at offset 16. Returns the archive base offset.
///
/// The scan is robust to short reads: it carries the trailing bytes of each
/// window into the next one, so an 11-byte magic that straddles a read
/// boundary is still found regardless of how the backend chops the stream.
fn find_xp3_offset<R: Read + Seek>(reader: &mut R) -> Result<u64> {
    const CHUNK: usize = 256 * 1024;
    const OVERLAP: usize = XP3_MAGIC.len() - 1;
    // Candidate absolute offsets are multiples of 16 from 16 onward.
    let mut abs = 16u64;
    reader.seek(SeekFrom::Start(abs))?;
    let mut carry: Vec<u8> = Vec::new();
    loop {
        let mut fresh = vec![0u8; CHUNK];
        let n = reader.read(&mut fresh)?;
        if n == 0 {
            break;
        }
        fresh.truncate(n);

        // `carry` holds the bytes immediately before `abs`, so the window
        // starts at `start_abs`.
        let start_abs = abs - carry.len() as u64;
        let mut window = std::mem::take(&mut carry);
        window.extend_from_slice(&fresh);

        // First candidate offset in the window aligned to a multiple of 16.
        let rem = (start_abs % 16) as usize;
        let mut p = if rem == 0 { 0 } else { 16 - rem };
        while p + XP3_MAGIC.len() <= window.len() {
            if window[p..p + XP3_MAGIC.len()] == XP3_MAGIC {
                return Ok(start_abs + p as u64);
            }
            p += 16;
        }

        // Carry the tail so a magic across the boundary is not lost.
        let keep = OVERLAP.min(window.len());
        carry.extend_from_slice(&window[window.len() - keep..]);
        abs += n as u64;
    }
    Err(Xp3Error::NotAnXp3)
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
    let len = usize::try_from(len).map_err(|_| Xp3Error::CorruptIndex("index too large"))?;
    let mut v = alloc_zeroed(len, "index too large")?;
    r.read_exact(&mut v)?;
    Ok(v)
}

/// Allocate a zeroed `Vec` of exactly `len` bytes without imposing a size
/// limit: an unrepresentable or unallocatable size is reported as a corrupt
/// index rather than panicking.
fn alloc_zeroed(len: usize, what: &'static str) -> Result<Vec<u8>> {
    let mut v = Vec::new();
    v.try_reserve_exact(len)
        .map_err(|_| Xp3Error::CorruptIndex(what))?;
    v.resize(len, 0);
    Ok(v)
}
