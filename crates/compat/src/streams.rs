//! The stream host the save and load members read and write through.
//!
//! The runtime's stream seam ([`tjs::vm::runtime::stream`]) owns the member
//! protocol and names only a host, a name and a mode. This module is the
//! host: it resolves the name through the attached L2 storage (disk files and
//! mounted archive members), writes local files through the same file system
//! the storage reads, and owns the save formats — the `FE FE <mode>` container,
//! its zlib payload, the UTF-16 byte-order mark, the `oN` offset and the write
//! access a mode selects.
//!
//! The host is installed once, by the compatibility binding, and removed with
//! it. Its only state is the address of the boxed [`CompatServices`] the
//! binding already keeps alive; storage is looked up when a stream opens, so a
//! host installed before the storage is attached sees it once it is.
#![allow(unsafe_code)]

use std::io::Write;
use std::path::{Path, PathBuf};

use api::error::FsError;
use api::fs::{FsPath, OpenMode};
use fs::{FileState, FileSystem};
use tjs::vm::runtime::stream::{
    BinaryReadStream, BinaryWriteStream, StreamError, StreamHost, TextReadStream, TextWriteStream,
};
use tjs::vm::runtime::string::TjsString;

use crate::engine_api::{Storage, StorageReadError};
use crate::storage::strip_file_media;
use crate::text::{TextEncoding, decode_units};
use crate::{CompatError, CompatServices};

/// The host over the attached storage and the local file system.
struct EngineStreams {
    /// The address of the boxed [`CompatServices`] the binding owns.
    ///
    /// The binding outlives this host: it installs the host after recording
    /// the address and removes the host before the services are dropped.
    services: usize,
}

impl EngineStreams {
    /// The services the host was installed against.
    fn services(&self) -> &CompatServices {
        // SAFETY: the binding stores the address of the boxed services it
        // keeps alive for the whole binding, and removes this host before that
        // value is dropped.
        unsafe { &*(self.services as *const CompatServices) }
    }

    /// The attached storage, when the composition root attached one.
    fn storage(&self) -> Option<&Storage> {
        self.services()
            .storage()
            // SAFETY: `attach_storage` requires the storage owner to outlive
            // the binding, and this host is removed with the binding.
            .map(|storage| unsafe { &*storage })
    }

    /// The file system the attached storage reads through, or the process
    /// file system when no storage is attached.
    fn file_system(&self) -> FileSystem {
        match self.storage() {
            Some(storage) => storage.file_system().clone(),
            None => FileSystem::host(""),
        }
    }

    /// Resolves the local path a write names.
    ///
    /// A name below the project directory is resolved by the storage's own
    /// case-correcting resolver, so a write updates the existing mixed-case
    /// file. An absolute path outside the project directory is used as named.
    fn local_path(&self, name: &str) -> Result<PathBuf, StreamError> {
        let clean = strip_file_media(name);
        if clean.is_empty() || clean.contains('>') {
            return Err(StreamError::CannotOpen {
                name: TjsString::from(name),
            });
        }
        let path = Path::new(clean);
        // A drive-letter path is absolute even on hosts where
        // `Path::is_absolute` does not treat it as one.
        if is_drive_path(clean) {
            return Ok(path.to_path_buf());
        }
        if let Some(storage) = self.storage() {
            match storage.local_name(clean) {
                Ok(Some(path)) => return Ok(path),
                Ok(None) => {}
                Err(_) => {
                    return Err(StreamError::CannotOpen {
                        name: TjsString::from(name),
                    });
                }
            }
            if path.is_absolute() {
                return Ok(path.to_path_buf());
            }
            return Ok(storage.app_dir().join(path));
        }
        Ok(path.to_path_buf())
    }

    /// Resolves the local path a write names, refusing a write into an archive
    /// or another storage media.
    fn write_path(&self, name: &TjsString) -> Result<PathBuf, StreamError> {
        let text = tjs_text(name).ok_or_else(|| StreamError::CannotOpen { name: name.clone() })?;
        if text.contains('>') || has_foreign_media_prefix(&text) {
            return Err(StreamError::CannotOpen { name: name.clone() });
        }
        self.local_path(&text)
    }

    /// Reads the whole entry a name resolves to.
    ///
    /// The storage is tried first, so an archive member and a disk file below
    /// the project directory resolve exactly as they do for every other
    /// storage read. A name the storage does not place falls back to the local
    /// file system; a name that still carries the archive delimiter never
    /// falls back to a literal file.
    fn read_entry(&self, name: &TjsString) -> Result<Vec<u8>, StreamError> {
        let text = tjs_text(name).ok_or_else(|| StreamError::CannotOpen { name: name.clone() })?;
        let lookup = strip_file_media(&text);
        if let Some(storage) = self.storage() {
            match storage.read(lookup) {
                Ok(bytes) => return Ok(bytes),
                Err(StorageReadError::NotFound(_)) => {}
                Err(_) => {
                    return Err(StreamError::CannotOpen { name: name.clone() });
                }
            }
        }
        if lookup.contains('>') {
            return Err(StreamError::CannotOpen { name: name.clone() });
        }
        let path = self.local_path(&text)?;
        let file_system = self.file_system();
        fs::read(&file_system, &FsPath::host(path))
            .map_err(|_| StreamError::CannotOpen { name: name.clone() })
    }
}

impl StreamHost for EngineStreams {
    fn open_text_read(
        &self,
        name: &TjsString,
        mode: &TjsString,
    ) -> Result<Box<dyn TextReadStream>, StreamError> {
        let mode_text = tjs_text(mode).ok_or(StreamError::Read)?;
        let raw = self.read_entry(name)?;
        let offset = mode_offset(&mode_text).map_err(|_| StreamError::Seek)?;
        let raw = offset_bytes(raw, offset)?;
        let decoded = decode_units(&raw, TextEncoding::Detect).map_err(|_| StreamError::Read)?;
        Ok(Box::new(HostTextRead {
            text: TjsString::from(decoded.units),
        }))
    }

    fn open_text_write(
        &self,
        name: &TjsString,
        mode: &TjsString,
    ) -> Result<Box<dyn TextWriteStream>, StreamError> {
        let mode_text = tjs_text(mode).ok_or(StreamError::Write)?;
        let path = self.write_path(name)?;
        let format = text_write_format(&mode_text).map_err(|_| StreamError::Write)?;
        let offset = mode_offset(&mode_text).map_err(|_| StreamError::Write)?;
        let file_system = self.file_system();
        create_parent_folders(&file_system, &path, name)?;
        let access = if offset != 0 {
            OpenMode::ReadWrite
        } else {
            OpenMode::Write
        };
        let file = open_for_write(&file_system, &path, access)
            .map_err(|_| StreamError::CannotOpen { name: name.clone() })?;
        HostTextWrite::open(file_system, file, offset, format)
    }

    fn open_binary_read(
        &self,
        name: &TjsString,
        mode: &TjsString,
    ) -> Result<Box<dyn BinaryReadStream>, StreamError> {
        let mode_text = tjs_text(mode).ok_or(StreamError::Read)?;
        let raw = self.read_entry(name)?;
        let offset = mode_offset(&mode_text).map_err(|_| StreamError::Seek)?;
        let position = usize::try_from(offset).map_err(|_| StreamError::Seek)?;
        if position > raw.len() {
            return Err(StreamError::Seek);
        }
        Ok(Box::new(HostBinaryRead {
            bytes: raw,
            position,
        }))
    }

    fn open_binary_write(
        &self,
        name: &TjsString,
        mode: &TjsString,
    ) -> Result<Box<dyn BinaryWriteStream>, StreamError> {
        let mode_text = tjs_text(mode).ok_or(StreamError::Write)?;
        let path = self.write_path(name)?;
        let offset = mode_offset(&mode_text).map_err(|_| StreamError::Write)?;
        let access = binary_write_access(&mode_text, offset);
        let file_system = self.file_system();
        create_parent_folders(&file_system, &path, name)?;
        let file = open_for_write(&file_system, &path, access)
            .map_err(|_| StreamError::CannotOpen { name: name.clone() })?;
        let position = if access == OpenMode::Append {
            fs::file_size(&file_system, &file).map_err(|_| StreamError::Seek)?
        } else {
            offset
        };
        Ok(Box::new(HostBinaryWrite {
            file_system,
            file,
            position,
            append: access == OpenMode::Append,
        }))
    }
}

/// Installs the host for a binding.
///
/// # Errors
///
/// [`CompatError::Register`] when a host is already installed: the stream seam
/// is process-wide and there is exactly one VM.
pub(crate) fn install(services: *mut CompatServices) -> Result<(), CompatError> {
    if tjs::vm::runtime::stream::host().is_some() {
        return Err(CompatError::Register(
            "the stream host is already installed".into(),
        ));
    }
    tjs::vm::runtime::stream::install(std::sync::Arc::new(EngineStreams {
        services: services as usize,
    }));
    Ok(())
}

/// Removes the host the binding installed.
pub(crate) fn uninstall() {
    tjs::vm::runtime::stream::uninstall();
}

/// The text a script string holds, or `None` when it is not valid UTF-16.
fn tjs_text(value: &TjsString) -> Option<String> {
    String::from_utf16(value.as_utf16()).ok()
}

/// Whether `name` is a Windows drive-letter path.
fn is_drive_path(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()
}

/// Whether `name` carries a storage-media name other than the local `file`
/// media.
fn has_foreign_media_prefix(name: &str) -> bool {
    let bytes = name.as_bytes();
    let len = bytes
        .iter()
        .take_while(|byte| byte.is_ascii_alphabetic())
        .count();
    if len == 0 || bytes.get(len) != Some(&b':') || len == 1 {
        return false;
    }
    !(len == 4 && &bytes[..4] == b"file")
}

/// The outcome of reading a mode number.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModeNumber {
    /// The key is absent.
    Absent,
    /// The key is present but no digit follows it.
    NoNumber,
    /// The key is followed by a digit; the value is the parsed number.
    Value(u64),
}

/// Finds `key` and, when it is directly followed by up to `max_digits`
/// decimal digits, parses them.
fn parse_mode_number(mode: &str, key: char, max_digits: usize) -> Option<ModeNumber> {
    let Some(position) = mode.find(key) else {
        return Some(ModeNumber::Absent);
    };
    let rest = &mode[position + key.len_utf8()..];
    let mut value = 0u64;
    let mut digits = 0usize;
    for character in rest.chars() {
        if digits >= max_digits || !character.is_ascii_digit() {
            break;
        }
        value = value
            .checked_mul(10)?
            .checked_add(u64::from(character as u8 - b'0'))?;
        digits += 1;
    }
    if digits == 0 {
        Some(ModeNumber::NoNumber)
    } else {
        Some(ModeNumber::Value(value))
    }
}

/// The byte offset a mode's `oN` selects.
fn mode_offset(mode: &str) -> Result<u64, ()> {
    match parse_mode_number(mode, 'o', 255).ok_or(())? {
        ModeNumber::Value(value) => Ok(value),
        ModeNumber::Absent | ModeNumber::NoNumber => Ok(0),
    }
}

/// The output form a text write mode selects.
#[derive(Clone, Copy, Debug)]
struct TextWriteFormat {
    /// The `c` cipher mode: -1 (plain), 1 (simple cipher) or 2 (zlib).
    crypt_mode: i32,
    /// The zlib level a `z` number selects.
    compression_level: u32,
}

/// Decides the output form of a text write stream from its mode string.
fn text_write_format(mode: &str) -> Result<TextWriteFormat, ()> {
    let mut crypt_mode = -1i32;
    match parse_mode_number(mode, 'c', 1).ok_or(())? {
        ModeNumber::Absent => {}
        ModeNumber::NoNumber => crypt_mode = 1,
        ModeNumber::Value(value) => {
            crypt_mode = i32::try_from(value).map_err(|_| ())?;
        }
    }
    let mut compression_level = 6u32;
    match parse_mode_number(mode, 'z', 1).ok_or(())? {
        ModeNumber::Absent => {}
        ModeNumber::NoNumber => crypt_mode = 2,
        ModeNumber::Value(value) => {
            compression_level = u32::try_from(value).map_err(|_| ())?;
            if compression_level > 9 {
                return Err(());
            }
        }
    }
    if crypt_mode != -1 && crypt_mode != 1 && crypt_mode != 2 {
        return Err(());
    }
    Ok(TextWriteFormat {
        crypt_mode,
        compression_level,
    })
}

/// The access a binary write mode selects.
fn binary_write_access(mode: &str, offset: u64) -> OpenMode {
    if mode.contains('a') {
        OpenMode::Append
    } else if offset != 0 {
        OpenMode::ReadWrite
    } else {
        OpenMode::Write
    }
}

/// Drops the first `offset` bytes, refusing an offset past the end.
fn offset_bytes(bytes: Vec<u8>, offset: u64) -> Result<Vec<u8>, StreamError> {
    let start = usize::try_from(offset).map_err(|_| StreamError::Seek)?;
    if start > bytes.len() {
        return Err(StreamError::Seek);
    }
    Ok(bytes[start..].to_vec())
}

/// Opens a write target, creating it when a read/write mode names a file that
/// does not exist yet.
fn open_for_write(
    file_system: &FileSystem,
    path: &Path,
    access: OpenMode,
) -> Result<FileState, FsError> {
    if access == OpenMode::ReadWrite {
        let exists = match fs::stat(file_system, &FsPath::host(path)) {
            Ok(meta) => !meta.is_dir,
            Err(FsError::NotFound { .. }) => false,
            Err(error) => return Err(error),
        };
        if !exists {
            return fs::open(file_system, &FsPath::host(path), OpenMode::Write);
        }
    }
    fs::open(file_system, &FsPath::host(path), access)
}

/// Creates the missing parent folders of a write target.
fn create_parent_folders(
    file_system: &FileSystem,
    path: &Path,
    name: &TjsString,
) -> Result<(), StreamError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    fs::create_dir_all(file_system, &FsPath::host(parent))
        .map_err(|_| StreamError::CannotOpen { name: name.clone() })
}

/// A text load reading the whole entry.
struct HostTextRead {
    /// The decoded text.
    text: TjsString,
}

impl TextReadStream for HostTextRead {
    fn read_all(&mut self) -> Result<TjsString, StreamError> {
        Ok(self.text.clone())
    }
}

/// A text save writing the file the mode selects.
struct HostTextWrite {
    /// The file system the file belongs to.
    file_system: FileSystem,
    /// The opened file.
    file: FileState,
    /// The first byte the save owns.
    offset: u64,
    /// Where the next plain or cipher write lands.
    position: u64,
    /// The `c` cipher mode: -1 (plain), 1 (simple cipher) or 2 (zlib).
    crypt_mode: i32,
    /// The zlib level a `z` number selected.
    compression_level: u32,
    /// The text written so far, for the zlib payload the next write emits.
    units: Vec<u16>,
}

impl HostTextWrite {
    /// Writes the header the mode selects and opens the stream.
    fn open(
        file_system: FileSystem,
        file: FileState,
        offset: u64,
        format: TextWriteFormat,
    ) -> Result<Box<dyn TextWriteStream>, StreamError> {
        let mut stream = Self {
            file_system,
            file,
            offset,
            position: offset,
            crypt_mode: format.crypt_mode,
            compression_level: format.compression_level,
            units: Vec::new(),
        };
        match stream.crypt_mode {
            2 => stream.flush_zlib()?,
            1 => {
                stream.write_raw(&[0xFE, 0xFE, 0x01, 0xFF, 0xFE])?;
            }
            _ => {
                stream.write_raw(&[0xFF, 0xFE])?;
            }
        }
        Ok(Box::new(stream))
    }

    /// Writes bytes at the current position and moves it.
    fn write_raw(&mut self, bytes: &[u8]) -> Result<(), StreamError> {
        let written = fs::write_at(&self.file_system, &self.file, self.position, bytes)
            .map_err(|_| StreamError::Write)?;
        if written != bytes.len() {
            return Err(StreamError::Write);
        }
        self.position = self
            .position
            .checked_add(u64::try_from(bytes.len()).map_err(|_| StreamError::Write)?)
            .ok_or(StreamError::Write)?;
        Ok(())
    }

    /// Emits the whole zlib container for the text written so far.
    fn flush_zlib(&mut self) -> Result<(), StreamError> {
        let mut raw = Vec::with_capacity(self.units.len().saturating_mul(2));
        for unit in &self.units {
            raw.extend_from_slice(&unit.to_le_bytes());
        }
        let mut encoder = flate2::write::ZlibEncoder::new(
            Vec::new(),
            flate2::Compression::new(self.compression_level),
        );
        encoder.write_all(&raw).map_err(|_| StreamError::Write)?;
        let compressed = encoder.finish().map_err(|_| StreamError::Write)?;
        let mut header = vec![0xFE, 0xFE, 0x02, 0xFF, 0xFE];
        header.extend_from_slice(
            &u64::try_from(compressed.len())
                .map_err(|_| StreamError::Write)?
                .to_le_bytes(),
        );
        header.extend_from_slice(
            &u64::try_from(raw.len())
                .map_err(|_| StreamError::Write)?
                .to_le_bytes(),
        );
        self.position = self.offset;
        self.write_raw(&header)?;
        self.write_raw(&compressed)
    }
}

impl TextWriteStream for HostTextWrite {
    fn write(&mut self, text: &TjsString) -> Result<(), StreamError> {
        if self.crypt_mode == 2 {
            self.units.extend_from_slice(text.as_utf16());
            return self.flush_zlib();
        }
        let mut bytes = Vec::with_capacity(text.len().saturating_mul(2));
        for &unit in text.as_utf16() {
            let unit = if self.crypt_mode == 1 {
                ((unit >> 1) & 0x5555) | ((unit << 1) & 0xAAAA)
            } else {
                unit
            };
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        self.write_raw(&bytes)
    }
}

/// A binary load over the entry's bytes.
struct HostBinaryRead {
    /// The whole entry.
    bytes: Vec<u8>,
    /// The position the next read takes its byte from.
    position: usize,
}

impl BinaryReadStream for HostBinaryRead {
    fn size(&self) -> Result<u64, StreamError> {
        u64::try_from(self.bytes.len()).map_err(|_| StreamError::Seek)
    }

    fn position(&self) -> Result<u64, StreamError> {
        u64::try_from(self.position).map_err(|_| StreamError::Seek)
    }

    fn read(&mut self, bytes: &mut [u8]) -> Result<usize, StreamError> {
        let remaining = self.bytes.len().saturating_sub(self.position);
        let count = remaining.min(bytes.len());
        let end = self.position + count;
        bytes[..count].copy_from_slice(&self.bytes[self.position..end]);
        self.position = end;
        Ok(count)
    }
}

/// A binary save writing the file the mode selects.
struct HostBinaryWrite {
    /// The file system the file belongs to.
    file_system: FileSystem,
    /// The opened file.
    file: FileState,
    /// Where the next write lands.
    position: u64,
    /// Whether the stream was opened in append mode.
    append: bool,
}

impl BinaryWriteStream for HostBinaryWrite {
    fn write(&mut self, bytes: &[u8]) -> Result<(), StreamError> {
        if self.append {
            let written =
                fs::append(&self.file_system, &self.file, bytes).map_err(|_| StreamError::Write)?;
            if written != bytes.len() {
                return Err(StreamError::Write);
            }
            return Ok(());
        }
        let written = fs::write_at(&self.file_system, &self.file, self.position, bytes)
            .map_err(|_| StreamError::Write)?;
        if written != bytes.len() {
            return Err(StreamError::Write);
        }
        self.position = self
            .position
            .checked_add(u64::try_from(bytes.len()).map_err(|_| StreamError::Write)?)
            .ok_or(StreamError::Write)?;
        Ok(())
    }
}
