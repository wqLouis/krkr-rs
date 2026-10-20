# `fs` — the stateless file system and the XP3 parse

Source: `crates/platform/fs/src/` · crate `fs` · layer **L1**

`fs` turns bytes into bytes. It knows about paths, handles, stat and listing
— and **nothing** about the storage namespace, `.xp3` addressing,
case-insensitive search or TJS. The namespace sits on top of it in L2
(`engine::storage`); see [`../engine/storage.md`](../engine/storage.md).

---

## 1. Responsibility

* the stateless `FileSystem` service object;
* the explicit `FileState` value (an open handle plus a logical cursor);
* the byte IO operations: open, stat, list, read, write, seek, copy, rename,
  remove, metadata, times;
* the **XP3 container read and parse** (`fs::xp3`).

Parsing a container is a stateless transform over bytes, so it belongs in L1;
the *namespace* over the parsed index belongs in L2. The seam between the two
is spelled out in [`06-file-system.md`](../../../docs.old/migration/06-file-system.md) §2
(evidence trail: [`docs.old/migration/06-file-system.md`](../../../docs.old/migration/06-file-system.md)).

---

## 2. Layer and dependencies

| | |
|---|---|
| Layer | L1 (bottom core) |
| Depends on | `api` (L1 DTOs + error enums), `thiserror`, `flate2` (XP3 zlib), `ahash` |
| Must not depend on | `engine` (L2), `compat` (L3), `vm`/`tjs` (L4), a composition root, `std::fs` above the host backend |
| Forbidden | `unsafe` (`#![forbid(unsafe_code)]`), process-global state, `#[allow(dead_code)]` |
| Lint | `#![warn(missing_docs)]` |

---

## 3. The service object and the state value

```rust
pub struct FileSystem { /* backend kind + base directory */ }
pub struct FileState  { /* open handle + logical cursor + mode */ }

pub enum FsConfig { Host { base: PathBuf }, Memory }   // #[non_exhaustive]

impl FileSystem {
    pub fn host(base: impl Into<PathBuf>) -> Self;
    pub fn memory() -> Self;
    pub fn from_config(config: FsConfig) -> Self;
    pub fn base(&self) -> Option<&Path>;
    pub fn is_host(&self) -> bool;
    pub fn is_memory(&self) -> bool;
}
pub fn create_file_system(config: FsConfig) -> FileSystem;
```

* `FileSystem` stores **only** configuration: a backend kind and, for the host
  backend, a base directory. No mutable field, no cache, no open-file
  registry.
* `FileState` is created by `open` and **owned by L2**, which stores every
  handle and makes every position decision. It exposes `cursor()` and
  `mode()`; the handle itself is private.
* Two clones of a memory-backed `FileSystem` share one store; two separate
  `FileSystem::memory()` calls do not.

### Backends

| backend | used for |
|---|---|
| `Host { base }` | the game directory, the app-data directory, savedata — `std::fs` rooted at `base`; a relative `FsPath::Host` is joined to `base`, an absolute one is used as given |
| `Memory` | the engine's unit tests and the storage fixtures; a `BTreeMap` of files plus a `BTreeSet` of directories behind `Arc<Mutex<MemoryStore>>` |

The memory store is a **shared resource**, not per-file handle state: the
logical cursor and the mode live in the `FileState`.

---

## 4. Public API — the operations

Every operation is a free function taking `&FileSystem` (plus the `FileState`
for the handle-based ones). They live in a private `ops` module and are
re-exported at the crate root, so the `fs::…` paths are stable.

```rust
// open and metadata
pub fn open(fs: &FileSystem, path: &FsPath, mode: OpenMode) -> Result<FileState, FsError>;
pub fn stat(fs: &FileSystem, path: &FsPath) -> Result<FileStat, FsError>;
pub fn exists(fs: &FileSystem, path: &FsPath) -> bool;
pub fn metadata(fs: &FileSystem, path: &FsPath) -> Result<FileMetadata, FsError>;
pub fn list(fs: &FileSystem, path: &FsPath) -> Result<Vec<DirEntry>, FsError>;
pub fn is_dir(fs: &FileSystem, path: &FsPath) -> Result<bool, FsError>;
pub fn is_file(fs: &FileSystem, path: &FsPath) -> Result<bool, FsError>;

// byte IO
pub fn read_at(fs: &FileSystem, file: &FileState, offset: u64, buf: &mut [u8]) -> Result<usize, FsError>;
pub fn write_at(fs: &FileSystem, file: &FileState, offset: u64, buf: &[u8]) -> Result<usize, FsError>;
pub fn read_seq(fs: &FileSystem, file: &mut FileState, buf: &mut [u8]) -> Result<usize, FsError>;
pub fn append(fs: &FileSystem, file: &FileState, buf: &[u8]) -> Result<usize, FsError>;
pub fn seek(fs: &FileSystem, file: &mut FileState, from: SeekFrom) -> Result<u64, FsError>;
pub fn file_size(fs: &FileSystem, file: &FileState) -> Result<u64, FsError>;
pub fn flush(fs: &FileSystem, file: &FileState) -> Result<(), FsError>;
pub fn read(fs: &FileSystem, path: &FsPath) -> Result<Vec<u8>, FsError>;
pub fn write(fs: &FileSystem, path: &FsPath, bytes: &[u8]) -> Result<(), FsError>;

// directory and file management
pub fn create_dir_all(fs: &FileSystem, path: &FsPath) -> Result<(), FsError>;
pub fn remove_file(fs: &FileSystem, path: &FsPath) -> Result<(), FsError>;
pub fn remove_dir(fs: &FileSystem, path: &FsPath) -> Result<(), FsError>;
pub fn copy(fs: &FileSystem, from: &FsPath, to: &FsPath) -> Result<(), FsError>;
pub fn rename(fs: &FileSystem, from: &FsPath, to: &FsPath) -> Result<(), FsError>;
pub fn set_times(fs: &FileSystem, path: &FsPath, seconds: i64) -> Result<(), FsError>;
```

All byte IO is **positional**. `read_at`/`write_at` do not move the cursor;
`read_seq` reads at the cursor and advances it; `seek` sets the cursor.

`list` returns the immediate children of a directory, sorted by name, with no
`"."` or `".."` entries.

---

## 5. The XP3 container

`fs::xp3` parses the XP3 archive format and reads an entry's bytes. It is
stateless: it holds the base offset and the entry map — metadata only — and
reads through positional IO.

```rust
pub struct Xp3Archive { /* base offset + entry map + index sections */ }

impl Xp3Archive {
    pub fn open(fs: &FileSystem, file: &FileState) -> Result<Self>;
    pub fn open_reader<R: Read + Seek>(reader: &mut R) -> Result<Self>;
    pub fn base_offset(&self) -> u64;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn entries(&self) -> impl Iterator<Item = &Entry>;
    pub fn index_sections(&self) -> &[IndexSection];
    pub fn near_misses(&self, request: &str, cap: usize) -> NearMisses;
    pub fn entry(&self, name: &str) -> Option<&Entry>;

    pub fn read(&self, fs: &FileSystem, file: &FileState, name: &str) -> Result<Vec<u8>>;
    pub fn read_with_filter(&self, fs: &FileSystem, file: &FileState, name: &str,
                            archive_name: &str,
                            filter: Option<&Xp3ExtractionFilter<'_>>) -> Result<Vec<u8>>;
    pub fn read_entry(&self, fs: &FileSystem, file: &FileState, entry: &Entry) -> Result<Vec<u8>>;
    pub fn read_entry_with_filter(&self, fs: &FileSystem, file: &FileState, entry: &Entry,
                                   archive_name: &str,
                                   filter: Option<&Xp3ExtractionFilter<'_>>) -> Result<Vec<u8>>;
    pub fn read_entry_from_bytes(&self, bytes: &[u8], entry: &Entry, archive_name: &str,
                                 filter: Option<&Xp3ExtractionFilter<'_>>) -> Result<Vec<u8>>;
    pub fn read_prefix_with_filter(&self, fs: &FileSystem, file: &FileState, name: &str,
                                    archive_name: &str, max_len: usize,
                                    filter: Option<&Xp3ExtractionFilter<'_>>) -> Result<Vec<u8>>;
}

pub fn normalize_in_archive_name(name: &str) -> String;
```

| type | contents |
|---|---|
| `Entry` | `flags`, `org_size`, `arc_size`, `raw_name`, the normalized `name`, `segments`, `file_hash` |
| `Segment` | `flags`, `start` (absolute offset in the archive), `org_size`, `arc_size` |
| `IndexSection` | one index block: `offset`, `encode` (`Raw`/`Zlib`), `file_chunks`, the names it added |
| `NearMisses` | the near-miss diagnostic for a failed lookup |
| `Xp3ExtractionFilter<'a>` | the per-segment extraction hook (`xp3filter`): `(file_hash, offset, buffer, file_name, archive_name, file_size)` |

An `Entry` is **metadata plus segment descriptors, not bytes**: the bytes stay
in the archive file (or in the preloaded image L2 owns), and a read is
positional over `Segment::start`/`arc_size`.

`read_entry_from_bytes` takes the image as an explicit `&[u8]`, so the preload
path never hands L1 ownership of the buffer.

---

## 6. State ownership

| value | owner |
|---|---|
| the backend kind and the base directory | L1 (`FileSystem`, immutable) |
| the open handle, the mode and the cursor | L2 (`FileState`) |
| the mounted archives, the parsed index, the auto-path tables, the resolution cache | L2 (`engine::storage`) |
| the preloaded archive image | L2 (`ArchiveBacking::Memory`) |

`fs` keeps no table of open files and no cache. The extraction filter is
L2 state: the stateless L1 reader receives the closure as a borrow on every
read and never stores it.

---

## 7. Platform gating

None. `fs` is pure Rust + `std::fs` and compiles identically everywhere. It
contains no `#[cfg(target_os)]` and no feature gate.

`FsPath::Uri` exists in the API so an Android SAF / `AssetManager` backend
could be added without an API change; **no such backend is wired**. The
Android host reads through the host backend with a resolved path — see
[`../android/host.md`](../android/host.md) §7.

---

## 8. Failure modes

| failure | behaviour |
|---|---|
| the path does not exist | `FsError::NotFound` |
| the path is a directory where a file was required (or the reverse) | `FsError::IsADirectory` / `FsError::NotADirectory` |
| the host backend is handed a `FsPath::Uri` | `FsError::InvalidPath` |
| the underlying IO fails | `FsError::Io` |
| the handle belongs to a different backend, or the mode forbids the call | `FsError::InvalidState` |
| the offset or the resulting size is not representable on this platform | `FsError::Unsupported` with the operation named |
| access is denied / the path exists where it must not | `FsError::PermissionDenied` / `FsError::AlreadyExists` |

The XP3 parse has its own enum, `Xp3Error`:

| variant | meaning |
|---|---|
| `Io` | an underlying read failed |
| `Fs(FsError)` | a bottom file-system read failed — carried through the `Read`/`Seek` adapter so `FsError::NotFound` stays matchable instead of being erased into an opaque I/O string |
| `NotAnXp3` | the file does not start with the XP3 magic (nor an MZ header containing one) |
| `BadIndexFlag` | the index flag byte has an unknown encode method |
| `CorruptIndex` | the index data is malformed |
| `UnknownSegmentMethod` | a segment uses an unknown encode method |
| `Inflate` | a zlib segment failed to inflate |
| `NotFound` | the requested entry does not exist |
| `NoIndex` | the archive has no index block |

A structural problem is an error, never a size cap: an index chunk that
overruns the index data is `CorruptIndex`, and a segment that declares an
unallocatable stored size fails on the read. There is no arbitrary limit.

---

## 9. Tests

* `crates/platform/fs/tests/host.rs` — the host round trip, stat/list/mkdir/
  remove/copy/rename, the "parent is a file" case and the backend-mismatch
  error.
* `crates/platform/fs/tests/memory.rs` — the memory round trip, the cursor
  advance, the zero-extending positional write, the rejected overflowing and
  unrepresentable offsets, the accepted large offset, the EOF read past the
  end, `seek` and the seek past `u64::MAX`, `append`, the directory
  operations, the typed missing/directory errors, `copy` preserving the
  source, and the independence of two memory roots.
* `crates/platform/fs/tests/xp3.rs` — the name normalization, the rejected
  oversized index chunk, the rejected oversized segment, the `FsError`
  recovered through the IO adapter, the preserved `FsError` on a read
  failure, and a valid archive's raw and zlib segment reads.
* `crates/platform/fs/tests/common/mod.rs` — the isolated temporary directory
  helper the host tests use.
* `crates/platform/fs/src/lib.rs` `mod tests` — the `MemoryStore` lock
  behaviour, including the deliberate panic on a poisoned mutex.

---

## 10. Related documents

| topic | document |
|---|---|
| the L1 overview | [README.md](README.md) |
| L1 as an architecture | [`arch/l1.md`](../../arch/l1.md) §2 |
| the storage namespace built on this crate | [`../engine/storage.md`](../engine/storage.md) |
| the file-system design and the L1↔L2 seam | [`06-file-system.md`](../../../docs.old/migration/06-file-system.md) · snapshot [`docs.old/migration/06-file-system.md`](../../../docs.old/migration/06-file-system.md) |
| game loading and startup discovery | [`09-game-loading.md`](../../../docs.old/migration/09-game-loading.md) |
| the L3 ↔ L2 protocol for storage | [`14-l3-l2-protocol.md`](../../../docs.old/migration/14-l3-l2-protocol.md) §6.2 |
| the state-ownership rule | [`arch_rules.md`](../../rules/arch_rules.md) §3.1 |
| the Rust-level checklist | [`code_styles.md`](../../rules/code_styles.md) |
