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
//! it returns a parsed index.
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

use std::collections::BTreeSet;
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
pub(crate) const INDEX_ENCODE_METHOD_MASK: u8 = 0x07;
/// Index stored raw.
pub const INDEX_ENCODE_RAW: u8 = 0;
/// Index stored zlib-compressed.
pub const INDEX_ENCODE_ZLIB: u8 = 1;
/// Index continue bit — another index block follows.
pub const INDEX_CONTINUE: u8 = 0x80;

/// Segment encode method mask.
pub(crate) const SEGM_ENCODE_METHOD_MASK: u32 = 0x07;
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

/// The result of an XP3 parse or read.
///
/// The error is always [`Xp3Error`]; the alias exists so the archive's
/// signatures name the failure mode instead of spelling it out.
pub type Result<T> = std::result::Result<T, Xp3Error>;

/// Stateless per-segment extraction filter (the `xp3filter` hook).
///
/// The L1 reader owns no filter: the caller passes one in and it is invoked
/// once per segment on the bytes that are about to be appended to the entry
/// output — the same point the reference engine calls
/// `TVPXP3ArchiveExtractionFilter` (`XP3Archive.cpp:998`), i.e. after a
/// compressed segment has been inflated and before the bytes are returned to
/// the reader. The filter may rewrite `buffer` in place.
///
/// Arguments: `(file_hash, offset, buffer, file_name, archive_name,
/// file_size)`.
///
/// * `file_hash` — the Adler32 name hash from the entry's `adlr` chunk;
/// * `offset` — uncompressed offset of `buffer[0]` within the entry;
/// * `buffer` — the segment's decoded bytes, rewritten in place;
/// * `file_name` — the entry's normalized in-archive name (the reference
///   passes `Owner->GetName`, which is normalized because gameplay archives
///   are opened with `normalizeName=true`);
/// * `archive_name` — the logical archive name the caller mounted
///   (empty when the caller does not know it);
/// * `file_size` — the entry's total uncompressed size.
pub type Xp3ExtractionFilter<'a> = dyn Fn(u32, u64, &mut [u8], &str, &str, u64) + 'a;

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

/// How one index block stores its payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexEncode {
    /// `TVP_XP3_INDEX_ENCODE_RAW` — the block's bytes are the index data.
    Raw,
    /// `TVP_XP3_INDEX_ENCODE_ZLIB` — the block is a zlib stream.
    Zlib,
}

/// What one index block contributed while the archive was parsed.
///
/// The reference reads every block an `INDEX_CONTINUE` chain links together
/// and appends each `File` chunk to one item vector
/// (`reference/cpp/core/archive/xp3/XP3Archive.cpp:348-505`); this records the
/// same blocks so a lookup miss can say how many were read and how many names
/// each supplied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexSection {
    /// Absolute offset of the block in the archive file.
    pub offset: u64,
    /// How the block is encoded.
    pub encode: IndexEncode,
    /// `File` chunks read from this block.
    pub file_chunks: usize,
    /// Names this block added that no earlier block had already supplied.
    pub new_names: usize,
}

/// A bounded set of archive names that resemble a request that missed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NearMisses {
    /// Up to the caller's cap, the closest names in archive order.
    pub names: Vec<String>,
    /// How many entries matched one of the near-miss rules in total.
    pub total: usize,
}

/// The archive index hasher. Fixed seeds keep iteration order stable across
/// runs; `ahash`'s default `RandomState` is randomized per process.
fn entry_hasher() -> ahash::RandomState {
    ahash::RandomState::with_seeds(
        0x2f5a_1c3d_9e7b_0481,
        0x8c4d_6e2f_1a90_7531,
        0x0b7e_3a5c_9d18_6240,
        0xf31c_9042_6d7b_e825,
    )
}

/// Archive entries keyed by normalized name.
type EntryMap = hashbrown::HashMap<String, Entry, ahash::RandomState>;

fn entry_map() -> EntryMap {
    EntryMap::with_hasher(entry_hasher())
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
    entries: EntryMap,
    /// Index blocks read while parsing, in read order.
    sections: Vec<IndexSection>,
}

impl Xp3Archive {
    /// Parse an XP3 index from an already-open [`FileState`].
    ///
    /// `fs` and `file` locate the archive's bytes; the returned value holds
    /// the parsed index only, and every later read takes the same pair again.
    ///
    /// # Errors
    ///
    /// [`Xp3Error::NotAnXp3`] when the bytes do not start with the XP3 magic,
    /// [`Xp3Error::NoIndex`] when the archive has no index block,
    /// [`Xp3Error::BadIndexFlag`] or [`Xp3Error::CorruptIndex`] when the index
    /// is malformed, and [`Xp3Error::Io`] or [`Xp3Error::Fs`] when the read
    /// itself fails.
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
    ///
    /// # Errors
    ///
    /// The same errors as [`open`](Self::open).
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
            entries: entry_map(),
            sections: Vec::new(),
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

    /// Iterate over every entry.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }

    /// The index blocks read while parsing, in read order.
    pub fn index_sections(&self) -> &[IndexSection] {
        &self.sections
    }

    /// Names that resemble `request` under the reference's in-archive lookup
    /// rules, bounded to `cap` names plus the total match count.
    ///
    /// A name is a near miss when, after the same normalization the reference
    /// applies to a stored name, it either equals the request modulo a leading
    /// path, shares the request's base name, or contains the request's stem.
    /// The strongest matches (path and base name) come first.
    pub fn near_misses(&self, request: &str, cap: usize) -> NearMisses {
        let request = normalize_in_archive_name(request);
        if request.is_empty() {
            return NearMisses {
                names: Vec::new(),
                total: 0,
            };
        }
        let request_base = request.rsplit('/').next().unwrap_or(request.as_str());
        let request_stem = request_base
            .split_once('.')
            .map_or(request_base, |(stem, _)| stem);
        let mut strong = Vec::new();
        let mut weak = Vec::new();
        let mut total = 0usize;
        for entry in self.entries.values() {
            let path_match = entry.name == request || ends_with_dir(&entry.name, &request);
            let base_match = entry.name.rsplit('/').next() == Some(request_base);
            if path_match || base_match {
                total += 1;
                strong.push(entry.name.clone());
            } else if !request_stem.is_empty() && entry.name.contains(request_stem) {
                total += 1;
                weak.push(entry.name.clone());
            }
        }
        strong.sort();
        weak.sort();
        let names = strong.into_iter().chain(weak).take(cap).collect();
        NearMisses { names, total }
    }

    /// Look up an entry by name (normalized before lookup).
    pub fn entry(&self, name: &str) -> Option<&Entry> {
        self.entries.get(&normalize_in_archive_name(name))
    }

    /// Read the full contents of an entry by name.
    ///
    /// # Errors
    ///
    /// [`Xp3Error::NotFound`] when the archive has no entry with that name,
    /// [`Xp3Error::Inflate`] when a compressed segment cannot be decoded, and
    /// [`Xp3Error::Io`] or [`Xp3Error::Fs`] when the read fails.
    pub fn read(&self, fs: &FileSystem, file: &FileState, name: &str) -> Result<Vec<u8>> {
        self.read_with_filter(fs, file, name, "", None)
    }

    /// Read the full contents of an entry by name, applying an extraction
    /// filter to each decoded segment before it is appended.
    ///
    /// `archive_name` is passed through to the filter for diagnostics and
    /// content-filter context; `filter` is never stored (L1 is stateless).
    ///
    /// # Errors
    ///
    /// The same errors as [`read`](Self::read).
    pub fn read_with_filter(
        &self,
        fs: &FileSystem,
        file: &FileState,
        name: &str,
        archive_name: &str,
        filter: Option<&Xp3ExtractionFilter<'_>>,
    ) -> Result<Vec<u8>> {
        let normalized = normalize_in_archive_name(name);
        let entry = self
            .entries
            .get(&normalized)
            .ok_or_else(|| Xp3Error::NotFound(name.to_string()))?;
        self.read_entry_with_filter(fs, file, entry, archive_name, filter)
    }

    /// Read the full contents of an entry.
    ///
    /// # Errors
    ///
    /// The same errors as [`read`](Self::read), except that a missing name is
    /// not one of them: the caller supplies the entry.
    pub fn read_entry(&self, fs: &FileSystem, file: &FileState, entry: &Entry) -> Result<Vec<u8>> {
        self.read_entry_with_filter(fs, file, entry, "", None)
    }

    /// Read the full contents of an entry, applying `filter` to each decoded
    /// segment in place before it is appended.
    ///
    /// The filter is invoked with the running uncompressed offset, matching
    /// the reference's `tTVPXP3ArchiveStream::Read`, so a whole-entry read
    /// sees the same `(offset, buffer)` sequence a streaming read would.
    ///
    /// # Errors
    ///
    /// The same errors as [`read_entry`](Self::read_entry).
    pub fn read_entry_with_filter(
        &self,
        fs: &FileSystem,
        file: &FileState,
        entry: &Entry,
        archive_name: &str,
        filter: Option<&Xp3ExtractionFilter<'_>>,
    ) -> Result<Vec<u8>> {
        let mut reader = FileReader { fs, file, pos: 0 };
        self.read_entry_from(&mut reader, entry, archive_name, filter)
    }

    /// Read the full contents of an entry from an in-memory archive image.
    ///
    /// `bytes` is the whole archive file, including any EXE wrapper, exactly
    /// as it was read from disk; the parser's base offset and each segment's
    /// absolute offset address into it. Every segment is bounds-checked
    /// against `bytes` before it is read, so a corrupt index or a truncated
    /// image is a typed error rather than a short read.
    ///
    /// Like [`Self::read_entry_with_filter`], `filter` is applied to each
    /// decoded segment and is never stored.
    ///
    /// # Errors
    ///
    /// [`Xp3Error::CorruptIndex`] when a segment offset overflows or a
    /// segment runs past the end of the image, [`Xp3Error::Inflate`] when a
    /// compressed segment cannot be decoded, and [`Xp3Error::Io`] when the
    /// read fails.
    pub fn read_entry_from_bytes(
        &self,
        bytes: &[u8],
        entry: &Entry,
        archive_name: &str,
        filter: Option<&Xp3ExtractionFilter<'_>>,
    ) -> Result<Vec<u8>> {
        let len = u64::try_from(bytes.len())
            .map_err(|_| Xp3Error::CorruptIndex("archive image is too large to address"))?;
        for seg in &entry.segments {
            let start = self
                .base
                .checked_add(seg.start)
                .ok_or(Xp3Error::CorruptIndex("segment offset overflow"))?;
            let end = start
                .checked_add(seg.arc_size)
                .ok_or(Xp3Error::CorruptIndex("segment size overflow"))?;
            if end > len {
                return Err(Xp3Error::CorruptIndex("segment overruns the archive image"));
            }
        }
        let mut reader = io::Cursor::new(bytes);
        self.read_entry_from(&mut reader, entry, archive_name, filter)
    }

    /// Read at most `max_len` bytes from the start of an entry by name,
    /// applying an extraction filter to each decoded segment.
    ///
    /// This is the lazy counterpart of [`Self::read_with_filter`]: a raw
    /// segment is read positionally up to the limit, and a zlib segment is
    /// inflated only as far as the requested bytes need, so a header probe
    /// never inflates the whole entry. It concatenates across segments until
    /// `max_len` is reached; trailing segments are not read or decoded. This
    /// is our own API, not a reference-parity claim: the reference exposes a
    /// seekable per-member stream (`tTVPXP3ArchiveStream`) but on this path
    /// reads whole files, so the reference does not do partial reads here.
    /// Segment order and concatenation are the same as [`Self::read`], and a
    /// genuine decode or I/O failure is the same typed error. A shorter entry
    /// returns short; the caller decides whether a short result is an error
    /// (the middle-layer wrapper treats a short header as the reference's own
    /// short-read case).
    ///
    /// # Errors
    ///
    /// The same errors as [`read_with_filter`](Self::read_with_filter).
    pub fn read_prefix_with_filter(
        &self,
        fs: &FileSystem,
        file: &FileState,
        name: &str,
        archive_name: &str,
        max_len: usize,
        filter: Option<&Xp3ExtractionFilter<'_>>,
    ) -> Result<Vec<u8>> {
        let normalized = normalize_in_archive_name(name);
        let entry = self
            .entries
            .get(&normalized)
            .ok_or_else(|| Xp3Error::NotFound(name.to_string()))?;
        let mut reader = FileReader { fs, file, pos: 0 };
        self.read_entry_prefix_from(&mut reader, entry, archive_name, max_len, filter)
    }

    /// Read at most `max_len` bytes of an entry from an in-memory archive
    /// image, with the same bounds checks as [`Self::read_entry_from_bytes`].
    ///
    /// # Errors
    ///
    /// The same errors as [`read_entry_from_bytes`](Self::read_entry_from_bytes).
    pub fn read_entry_prefix_from_bytes(
        &self,
        bytes: &[u8],
        entry: &Entry,
        archive_name: &str,
        max_len: usize,
        filter: Option<&Xp3ExtractionFilter<'_>>,
    ) -> Result<Vec<u8>> {
        let len = u64::try_from(bytes.len())
            .map_err(|_| Xp3Error::CorruptIndex("archive image is too large to address"))?;
        for seg in &entry.segments {
            let start = self
                .base
                .checked_add(seg.start)
                .ok_or(Xp3Error::CorruptIndex("segment offset overflow"))?;
            let end = start
                .checked_add(seg.arc_size)
                .ok_or(Xp3Error::CorruptIndex("segment size overflow"))?;
            if end > len {
                return Err(Xp3Error::CorruptIndex("segment overruns the archive image"));
            }
        }
        let mut reader = io::Cursor::new(bytes);
        self.read_entry_prefix_from(&mut reader, entry, archive_name, max_len, filter)
    }

    /// Read a bounded prefix of an entry through any `Read + Seek` source.
    fn read_entry_prefix_from<R: Read + Seek>(
        &self,
        reader: &mut R,
        entry: &Entry,
        archive_name: &str,
        max_len: usize,
        filter: Option<&Xp3ExtractionFilter<'_>>,
    ) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut offset: u64 = 0;
        for seg in &entry.segments {
            if out.len() >= max_len {
                break;
            }
            let remaining = max_len - out.len();
            let method = seg.flags & SEGM_ENCODE_METHOD_MASK;
            // The decoded length of a segment: the uncompressed size for a
            // zlib segment, the stored size for a raw one.
            let decoded_len = if method == SEGM_ENCODE_ZLIB {
                seg.org_size
            } else {
                seg.arc_size
            };
            let want = u64::try_from(remaining)
                .map_err(|_| Xp3Error::CorruptIndex("segment too large"))?
                .min(decoded_len);
            let want =
                usize::try_from(want).map_err(|_| Xp3Error::CorruptIndex("segment too large"))?;
            let start = self
                .base
                .checked_add(seg.start)
                .ok_or(Xp3Error::CorruptIndex("segment offset overflow"))?;
            reader.seek(SeekFrom::Start(start))?;
            let mut decoded = match method {
                SEGM_ENCODE_RAW => {
                    let mut buf = alloc_zeroed(want, "segment too large")?;
                    reader.read_exact(&mut buf)?;
                    buf
                }
                SEGM_ENCODE_ZLIB => {
                    let mut buf = alloc_zeroed(want, "segment too large")?;
                    // The decoder pulls compressed bytes from the bounded
                    // reader on demand, so only as much of the segment as the
                    // requested output needs is read and inflated.
                    let mut dec = ZlibDecoder::new((&mut *reader).take(seg.arc_size));
                    dec.read_exact(&mut buf)
                        .map_err(|_| Xp3Error::Inflate(entry.name.clone()))?;
                    buf
                }
                other => return Err(Xp3Error::UnknownSegmentMethod(other, seg.flags)),
            };
            // The reference applies the extraction filter to the decoded
            // segment with the uncompressed offset and the normalized
            // in-archive name; a prefix is the same transform on fewer bytes.
            if let Some(filter) = filter {
                filter(
                    entry.file_hash,
                    offset,
                    &mut decoded,
                    &entry.name,
                    archive_name,
                    entry.org_size,
                );
            }
            offset = offset
                .checked_add(decoded.len() as u64)
                .ok_or(Xp3Error::CorruptIndex("entry size overflow"))?;
            out.extend_from_slice(&decoded);
        }
        Ok(out)
    }

    /// Read an entry through any `Read + Seek` source.
    fn read_entry_from<R: Read + Seek>(
        &self,
        reader: &mut R,
        entry: &Entry,
        archive_name: &str,
        filter: Option<&Xp3ExtractionFilter<'_>>,
    ) -> Result<Vec<u8>> {
        // Note: `protected` (DRM bit) files are read normally, matching the
        // reference emulator, which allows extracting protected storage.
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
        let mut offset: u64 = 0;
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
            let mut decoded = match method {
                SEGM_ENCODE_RAW => buf,
                SEGM_ENCODE_ZLIB => {
                    let org_len = usize::try_from(seg.org_size)
                        .map_err(|_| Xp3Error::CorruptIndex("segment too large"))?;
                    let mut dec = ZlibDecoder::new(&buf[..]);
                    let mut chunk = alloc_zeroed(org_len, "segment too large")?;
                    dec.read_exact(&mut chunk)
                        .map_err(|_| Xp3Error::Inflate(entry.name.clone()))?;
                    chunk
                }
                other => return Err(Xp3Error::UnknownSegmentMethod(other, seg.flags)),
            };
            // The reference applies the extraction filter to the decoded
            // segment (after any inflation) with the uncompressed offset and
            // the normalized in-archive name (`Owner->GetName` returns the
            // name `Init` normalized under `normalizeName=true`).
            if let Some(filter) = filter {
                filter(
                    entry.file_hash,
                    offset,
                    &mut decoded,
                    &entry.name,
                    archive_name,
                    entry.org_size,
                );
            }
            offset = offset
                .checked_add(decoded.len() as u64)
                .ok_or(Xp3Error::CorruptIndex("entry size overflow"))?;
            out.extend_from_slice(&decoded);
        }
        // The entry header's `org_size` must agree with what the segments
        // actually compose; otherwise `stat` would report one size and `read`
        // return another. This is a real check, not a debug assertion. (The
        // filter runs before this check but must preserve the byte count.)
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
        // No arbitrary chain bound: a real chain is finite. Termination
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
            let (encode, data) = match flag & INDEX_ENCODE_METHOD_MASK {
                INDEX_ENCODE_RAW => {
                    let size = read_u64_le(reader)?;
                    (IndexEncode::Raw, read_exact_vec(reader, size)?)
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
                    (IndexEncode::Zlib, data)
                }
                other => return Err(Xp3Error::BadIndexFlag(flag | other)),
            };

            let before = self.entries.len();
            let file_chunks = self.parse_index(&data)?;
            self.sections.push(IndexSection {
                offset: block,
                encode,
                file_chunks,
                new_names: self.entries.len() - before,
            });

            // On CONTINUE the stream is left right after this block's data,
            // where the next index offset is stored.
            if flag & INDEX_CONTINUE == 0 {
                break;
            }
        }
        Ok(())
    }

    /// Parse one index block: chunks of `[4-byte tag][u64 LE size][data]`.
    ///
    /// Returns how many `File` chunks the block held.
    fn parse_index(&mut self, data: &[u8]) -> Result<usize> {
        let mut rest = data;
        let mut file_chunks = 0usize;
        while let Some((header, tail)) = split_chunk_header(rest) {
            let (tag, size) = header.split_at(4);
            let size = u64::from_le_bytes(prefix(size)?);
            let size =
                usize::try_from(size).map_err(|_| Xp3Error::CorruptIndex("chunk too large"))?;
            let (payload, next) = tail
                .split_at_checked(size)
                .ok_or(Xp3Error::CorruptIndex("chunk overruns index data"))?;
            rest = next;

            if tag == b"File" {
                self.parse_file_chunk(payload)?;
                file_chunks += 1;
            }
        }
        // A truncated chunk header or payload leaves trailing bytes; a
        // structurally complete index consumes every byte exactly.
        if !rest.is_empty() {
            return Err(Xp3Error::CorruptIndex("trailing bytes in index data"));
        }
        Ok(file_chunks)
    }

    /// Parse one `File` chunk: `info` / `segm` / `adlr` sub-chunks.
    fn parse_file_chunk(&mut self, data: &[u8]) -> Result<()> {
        let mut rest = data;
        let mut flags = 0u32;
        let mut org_size = 0u64;
        let mut arc_size = 0u64;
        let mut raw_name = String::new();
        let mut segments = Vec::new();
        let mut file_hash = 0u32;
        let mut seen_info = false;
        let mut seen_segm = false;
        let mut seen_adlr = false;

        while let Some((header, tail)) = split_chunk_header(rest) {
            let (tag, size) = header.split_at(4);
            let size = u64::from_le_bytes(prefix(size)?);
            let size =
                usize::try_from(size).map_err(|_| Xp3Error::CorruptIndex("sub-chunk too large"))?;
            let (payload, next) = tail
                .split_at_checked(size)
                .ok_or(Xp3Error::CorruptIndex("sub-chunk overruns File chunk"))?;
            rest = next;

            match tag {
                b"info" => {
                    if seen_info {
                        return Err(Xp3Error::CorruptIndex("duplicate info sub-chunk"));
                    }
                    seen_info = true;
                    if payload.len() < 22 {
                        return Err(Xp3Error::CorruptIndex("info sub-chunk too short"));
                    }
                    flags = u32::from_le_bytes(prefix(&payload[0..4])?);
                    org_size = u64::from_le_bytes(prefix(&payload[4..12])?);
                    arc_size = u64::from_le_bytes(prefix(&payload[12..20])?);
                    let name_len = i16::from_le_bytes(prefix(&payload[20..22])?);
                    let name_len = usize::try_from(name_len)
                        .map_err(|_| Xp3Error::CorruptIndex("negative name length"))?;
                    let name_bytes = payload
                        .get(22..22 + name_len * 2)
                        .ok_or(Xp3Error::CorruptIndex("name overruns info sub-chunk"))?;
                    let mut units = Vec::with_capacity(name_len);
                    for c in name_bytes.as_chunks::<2>().0 {
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
                    for s in payload.as_chunks::<28>().0 {
                        let (flags, s) = s.split_at(4);
                        let (start, s) = s.split_at(8);
                        let (org_size, arc_size) = s.split_at(8);
                        segments.push(Segment {
                            flags: u32::from_le_bytes(prefix(flags)?),
                            start: u64::from_le_bytes(prefix(start)?),
                            org_size: u64::from_le_bytes(prefix(org_size)?),
                            arc_size: u64::from_le_bytes(prefix(arc_size)?),
                        });
                    }
                }
                b"adlr" => {
                    if seen_adlr {
                        return Err(Xp3Error::CorruptIndex("duplicate adlr sub-chunk"));
                    }
                    seen_adlr = true;
                    if let Some(h) = payload.get(0..4) {
                        file_hash = u32::from_le_bytes(prefix(h)?);
                    }
                }
                _ => { /* unknown sub-chunk: ignore */ }
            }
        }

        // A `File` chunk is only a valid entry when all three required
        // sub-chunks are present exactly once; silently defaulting them would
        // create an addressable entry with a bogus size or no data.
        if !rest.is_empty() {
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

/// Whether `name` ends with `suffix` at a path-component boundary.
fn ends_with_dir(name: &str, suffix: &str) -> bool {
    name.len() > suffix.len()
        && name.ends_with(suffix)
        && name.as_bytes()[name.len() - suffix.len() - 1] == b'/'
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
        let target = u64::try_from(target.max(0)).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "seek target is out of range")
        })?;
        self.pos = target;
        Ok(self.pos)
    }
}

// ---------------------------------------------------------------------------
// Format helpers
// ---------------------------------------------------------------------------

/// Read the first `N` bytes of `b` as an array, reporting a short slice as a
/// corrupt index rather than panicking.
fn prefix<const N: usize>(b: &[u8]) -> Result<[u8; N]> {
    b.get(..N)
        .and_then(|s| s.try_into().ok())
        .ok_or(Xp3Error::CorruptIndex("truncated chunk field"))
}

/// Split a `[4-byte tag][u64 LE size]` chunk header off the front of `data`.
fn split_chunk_header(data: &[u8]) -> Option<(&[u8], &[u8])> {
    (data.len() >= 12).then(|| data.split_at(12))
}

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

#[cfg(test)]
mod tests {
    use super::*;

    /// A field slice shorter than `N` is a typed corrupt-index error, not a
    /// panic.
    #[test]
    fn prefix_rejects_a_short_slice() {
        let err = prefix::<8>(&[0u8; 4]).unwrap_err();
        assert!(matches!(err, Xp3Error::CorruptIndex(_)));
        assert_eq!(prefix::<4>(&[1, 2, 3, 4]).unwrap(), [1, 2, 3, 4]);
    }

    /// A chunk whose declared payload overruns the index is reported as a
    /// corrupt index rather than panicking.
    #[test]
    fn chunk_overrunning_the_index_is_rejected() {
        let mut archive = Xp3Archive {
            base: 0,
            entries: entry_map(),
            sections: Vec::new(),
        };
        let mut index = Vec::new();
        index.extend_from_slice(b"File");
        index.extend_from_slice(&4u64.to_le_bytes());
        index.extend_from_slice(&[0u8; 3]);
        let err = archive.parse_index(&index).unwrap_err();
        assert!(matches!(err, Xp3Error::CorruptIndex(_)));
    }

    /// A prefix read concatenates across raw and zlib segments and stops at
    /// the requested length.
    #[test]
    fn prefix_read_concatenates_across_segments() {
        use flate2::Compression;
        use flate2::write::ZlibEncoder;
        use std::io::Write;

        let raw_a = b"AAAA".to_vec();
        let raw_b = b"BBBBBB".to_vec();
        let zlib_src = b"CCCCCCCC".to_vec();
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&zlib_src).expect("zlib compress");
        let zlib = enc.finish().expect("zlib finish");

        let mut image = Vec::new();
        let start_a = image.len() as u64;
        image.extend_from_slice(&raw_a);
        let start_b = image.len() as u64;
        image.extend_from_slice(&raw_b);
        let start_c = image.len() as u64;
        image.extend_from_slice(&zlib);

        let entry = Entry {
            flags: 0,
            org_size: (raw_a.len() + raw_b.len() + zlib_src.len()) as u64,
            arc_size: (raw_a.len() + raw_b.len() + zlib.len()) as u64,
            raw_name: "multi.bin".to_string(),
            name: "multi.bin".to_string(),
            segments: vec![
                Segment {
                    flags: SEGM_ENCODE_RAW,
                    start: start_a,
                    org_size: raw_a.len() as u64,
                    arc_size: raw_a.len() as u64,
                },
                Segment {
                    flags: SEGM_ENCODE_RAW,
                    start: start_b,
                    org_size: raw_b.len() as u64,
                    arc_size: raw_b.len() as u64,
                },
                Segment {
                    flags: SEGM_ENCODE_ZLIB,
                    start: start_c,
                    org_size: zlib_src.len() as u64,
                    arc_size: zlib.len() as u64,
                },
            ],
            file_hash: 0,
        };

        let archive = Xp3Archive {
            base: 0,
            entries: entry_map(),
            sections: Vec::new(),
        };
        let mut reader = io::Cursor::new(image.as_slice());
        assert_eq!(
            archive
                .read_entry_prefix_from(&mut reader, &entry, "test.xp3", 6, None)
                .unwrap(),
            b"AAAABB"
        );
        assert_eq!(
            archive
                .read_entry_prefix_from(&mut reader, &entry, "test.xp3", 12, None)
                .unwrap(),
            b"AAAABBBBBBCC"
        );
        assert_eq!(
            archive
                .read_entry_prefix_from(&mut reader, &entry, "test.xp3", 64, None)
                .unwrap(),
            b"AAAABBBBBBCCCCCCCC"
        );
    }

    /// A `Read + Seek` source that counts the bytes it hands out, so the
    /// prefix path can be shown to pull materially less input than the
    /// whole-entry path.
    struct CountingReader<'a> {
        data: &'a [u8],
        pos: usize,
        bytes_read: usize,
    }

    impl Read for CountingReader<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let remaining = self.data.len().saturating_sub(self.pos);
            let n = buf.len().min(remaining);
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            self.bytes_read += n;
            Ok(n)
        }
    }

    impl Seek for CountingReader<'_> {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            let target: i128 = match pos {
                SeekFrom::Start(off) => off as i128,
                SeekFrom::End(off) => self.data.len() as i128 + off as i128,
                SeekFrom::Current(off) => self.pos as i128 + off as i128,
            };
            let target = u64::try_from(target.max(0)).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "seek target is out of range")
            })?;
            self.pos = usize::try_from(target).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "seek target is out of range")
            })?;
            Ok(target)
        }
    }

    /// Build a one-segment entry whose stored bytes live at archive offset
    /// `start` in an image the caller supplies.
    fn single_segment_entry(
        name: &str,
        method: u32,
        start: u64,
        stored: &[u8],
        org_size: usize,
    ) -> Entry {
        Entry {
            flags: 0,
            org_size: org_size as u64,
            arc_size: stored.len() as u64,
            raw_name: name.to_string(),
            name: name.to_string(),
            segments: vec![Segment {
                flags: method,
                start,
                org_size: org_size as u64,
                arc_size: stored.len() as u64,
            }],
            file_hash: 0,
        }
    }

    /// A zlib prefix inflates only as far as the requested bytes: 36 bytes of
    /// a 1 MiB incompressible entry decode far less than the whole-entry read
    /// and pull far less input from the source, and the decoded output stops
    /// at 36 bytes. Two things are counted: the decoded bytes the extraction
    /// filter sees (36 vs the whole entry), and the raw bytes the zlib decoder
    /// pulled through the counting `Read`. This is our own prefix capability
    /// (the reference reads whole files on this path), so it carries no
    /// reference citation.
    #[test]
    fn prefix_inflates_only_the_requested_bytes() {
        use flate2::Compression;
        use flate2::write::ZlibEncoder;
        use std::io::Write;

        let mut payload = Vec::with_capacity(1 << 20);
        let mut state: u32 = 0x1234_5678;
        for _ in 0..(1 << 20) {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            payload.push((state >> 24) as u8);
        }
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&payload).expect("zlib compress");
        let stored = enc.finish().expect("zlib finish");
        assert!(
            stored.len() > 64 * 1024,
            "fixture must exceed the decoder buffer: {}",
            stored.len()
        );

        let entry = single_segment_entry("big.bin", SEGM_ENCODE_ZLIB, 0, &stored, payload.len());
        let archive = Xp3Archive {
            base: 0,
            entries: entry_map(),
            sections: Vec::new(),
        };

        let mut prefix_reader = CountingReader {
            data: &stored,
            pos: 0,
            bytes_read: 0,
        };
        let prefix_decoded = std::cell::Cell::new(0usize);
        let prefix_filter =
            |_hash: u32,
             _offset: u64,
             buffer: &mut [u8],
             _name: &str,
             _archive: &str,
             _size: u64| { prefix_decoded.set(prefix_decoded.get() + buffer.len()) };
        let prefix = archive
            .read_entry_prefix_from(&mut prefix_reader, &entry, "", 36, Some(&prefix_filter))
            .expect("prefix read");
        assert_eq!(prefix, payload[..36]);
        let prefix_bytes = prefix_reader.bytes_read;
        let prefix_decoded = prefix_decoded.get();

        let mut whole_reader = CountingReader {
            data: &stored,
            pos: 0,
            bytes_read: 0,
        };
        let whole_decoded = std::cell::Cell::new(0usize);
        let whole_filter =
            |_hash: u32,
             _offset: u64,
             buffer: &mut [u8],
             _name: &str,
             _archive: &str,
             _size: u64| { whole_decoded.set(whole_decoded.get() + buffer.len()) };
        let whole = archive
            .read_entry_from(&mut whole_reader, &entry, "", Some(&whole_filter))
            .expect("whole read");
        assert_eq!(whole, payload);
        let whole_bytes = whole_reader.bytes_read;
        let whole_decoded = whole_decoded.get();

        assert_eq!(prefix.len(), 36, "decoded output must stop at max_len");
        assert_eq!(
            prefix_decoded, 36,
            "the filter sees exactly max_len decoded bytes"
        );
        assert!(
            prefix_decoded * 8 < whole_decoded,
            "prefix decoded {prefix_decoded} bytes, whole decoded {whole_decoded}"
        );
        assert!(
            prefix_bytes * 8 < whole_bytes,
            "prefix pulled {prefix_bytes} source bytes, whole pulled {whole_bytes}"
        );
    }

    /// A raw prefix reads positionally for exactly the requested bytes, and a
    /// zero limit reads nothing at all.
    #[test]
    fn prefix_reads_raw_positionally_and_zero_reads_nothing() {
        let stored: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
        let entry = single_segment_entry("raw.bin", SEGM_ENCODE_RAW, 0, &stored, stored.len());
        let archive = Xp3Archive {
            base: 0,
            entries: entry_map(),
            sections: Vec::new(),
        };

        let mut reader = CountingReader {
            data: &stored,
            pos: 0,
            bytes_read: 0,
        };
        let prefix = archive
            .read_entry_prefix_from(&mut reader, &entry, "", 7, None)
            .expect("prefix read");
        assert_eq!(prefix, &stored[..7]);
        assert_eq!(reader.bytes_read, 7);

        let mut reader = CountingReader {
            data: &stored,
            pos: 0,
            bytes_read: 0,
        };
        let empty = archive
            .read_entry_prefix_from(&mut reader, &entry, "", 0, None)
            .expect("zero-length prefix");
        assert!(empty.is_empty());
        assert_eq!(reader.bytes_read, 0);
    }
}
