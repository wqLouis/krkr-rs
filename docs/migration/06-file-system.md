# 06 — File system

Status: implemented. Raw byte IO lives in the bottom `fs` crate and the TVP
storage namespace sits on top of it in L2 `engine::storage`.

Goal: a **stateless `FileSystem` object** plus an explicit **`FileState`**
value. L1 knows about paths, URIs, handles, stat and listing — and nothing
about `.xp3`, `arc.xp3>path` addressing, case-insensitive search or TJS. The
middle layer stores the `FileState`s and the namespace index; the natives
keep their current surface.

---

## 1. Inventory

| Concern | Owner |
|---------|-------|
| Game mount + namespace (`arc.xp3>path`, disk-over-archive, case rules, auto-path) | L2 `engine::storage` over the `FileSystem` object |
| XP3 container parsing + segment read | L1 `fs::xp3`, a stateless parse + positional read over a `FileState`; L2 registers the parsed index |
| Raw host path IO | L1 `fs` (`FileSystem` + `FileState`); `std::fs` is platform code and stays there |
| Stream abstraction | an L2 wrapper over a `FileState` + cursor |
| TJS `Array.save/load/saveStruct` | C++ `std::fopen` in `tjs/cpp/streams.cpp`; the one IO path the fs surface does not reach |
| Android SAF / APK assets | `FsPath::Uri` exists in the API; no SAF/asset backend is wired yet |

## 2. Boundaries

### Bottom (`fs`) owns

* the stateless `FileSystem` object (backend kind + base);
* stateless functions: open, stat, exists, list, create dirs, remove, copy,
  rename;
* the `FileState` value: the open handle **and** the logical cursor;
* positional `read_at`/`write_at`, `size`, `flush` over a `FileState`;
* the XP3 container read + parse (`fs::xp3`): given the bytes through a
  `FileState`, it returns only parsed index metadata (base offset + entry
  map) and reads segments through positional `read_at`. It holds no file
  handle and no cursor of its own;
* the host path dialect (Linux `PathBuf`, Android `PathBuf` for app-private
  dirs and resolved external paths, Android content/asset `Uri`).

It knows **nothing** about namespace resolution, caches or which archive a
name belongs to. It never keeps a table of open files; L2 stores each
`FileState`.

### Middle (`engine`) owns

* storage namespaces: game dir + mounted archives, disk-wins ordering;
* `arc.xp3>path` addressing and the specific-archive prefix;
* normalization (lowercase, `/` separators) and the recursive
  case-insensitive walk that makes Windows-authored games work on Linux;
* the auto-search-path list and the mount index (name → source);
* the parsed archive index returned by L1, plus the `FileState`s that own
  each archive's bytes — the namespace state. L2 never reads or parses the
  XP3 container itself; that is L1 (see the D6 note below);
* `Storages` native semantics: `getPlacedPath`, `isExistentStorage`,
  `getLocalName`, `getFullPath`, `addAutoPath`/`removeAutoPath`, the
  `extractStorage*`/`chopStorageExt` string helpers, and the fstat-plugin file
  operations (`stat`, `deleteFile`, `copyFile`, …); save-data writes.
  Directory listing is **not** a `Storages` core member in the reference: it
  lists through the `dirlist.dll` global `getDirList` and the fstat plugin's
  `Storages.dirlist`/`dirlistEx`/`dirtree`, all of which are **disk-only**
  (`reference/cpp/plugins/fstat/main.cpp:486-539`). There is no
  `Storages.getFileList` in `reference/cpp`. The L2 `list`/`file_list` that
  merge disk and archive entries are a non-reference enumeration helper used
  by tests, not a registered script member.

> **D6 note.** The XP3 container is read and parsed in L1
> (`fs::xp3`), because parsing is a stateless transform over bytes.
> L2 only owns the resulting parsed index and the open handles. The middle
> layer therefore does **not** own the `xp3` container; this corrects the
> earlier sketch in this document and is the contract implemented by
> `engine::storage` and `docs/migration/09-game-loading.md`.

### L3 (`compat`) uses

The exact same method surface and error strings; its implementation calls the
middle layer (and, where it needs file IO, the L1 `fs` abstraction) instead of
touching `std::fs`.

### The seam in code

| Layer | Type / function | Role |
|---|---|---|
| L1 | `fs::FileSystem` (`Backend::Host { base }` \| `Backend::Memory { store }`); `FileSystem::{host,memory,from_config}`, `base()/is_host()/is_memory()` | the stateless service object — configuration only, no mutable field, no open-file registry |
| L1 | `fs::open(fs, path, mode) -> Result<FileState, FsError>` | creates the state value |
| L1 | `fs::FileState` (open handle + cursor); `read_at`/`write_at`/`file_size`/`flush`/`seek`/`read_seq` | positional IO; the cursor is the only evolving thing and it lives in the value, not the object |
| L1 | `fs` ops: `stat`/`exists`/`list`/`create_dir_all`/`remove_file`/`remove_dir`/`copy`/`rename`/`metadata`/`set_times` | namespace-free filesystem actions |
| L1 | `fs::xp3::Xp3Archive`, `read_entry_from_bytes` | stateless XP3 parse (index) + segment read over the bytes |
| L2 | `engine::storage::Storage` | owns the namespace: game dir + mounted archives, precedence, auto-path list, resolution cache, the parsed index and each archive's `FileState`/backing image |
| L2 | `mount_archives`/`mount_archive`/`mount_discovered_archives` (+ `_with(MountOptions)`), `unmount_archive`, `discover_archives` | mounting (D35 scan) and the `preload_archives` choice (`ArchiveBacking::Streaming \| Image`) |
| L2 | `find(name) -> Result<Option<Location>, StorageReadError>`, `read`/`read_prefix`, `stat -> Result<Option<StorageMetadata>>`, `exists -> Result<bool>`, `list`/`file_list` | resolution and reads; `NotFound` and a real error stay distinguishable (D11) |
| L2 | `add_auto_path`, `register_media`, `startup_plan`/`discover_startup`, `clear_resolution_cache`, `probe_counts` | TVP auto-path search, in-memory registered media, startup discovery |
| L3 | `compat::natives::storages` | parses the TVP storage name / `arc.xp3>path` grammar and maps it onto `Storage`; it owns no bytes |

The rule of thumb: **L1 turns bytes into bytes; L2 turns names into sources.**
`Storage` never parses an XP3 container, and `fs` never resolves a name.

### How the entries are held

L2 does **not** build one merged name→source table. It holds **one parsed index
per mounted archive** and probes them in order:

* `Storage::archives: Vec<MountedArchive>`, in mount order. `find` walks it
  `.rev()`, so a later mount wins; `find_in_archive` (`arc.xp3>path`) matches
  by `MountedArchive::file_name` case-insensitively.
* `MountedArchive { path, file_name, backing: ArchiveBacking, archive: Xp3Archive }`.
* `Xp3Archive` — the **type is L1's, the value is L2's** (parsing is stateless,
  so it lives in `fs::xp3`; the owned index is `Storage` state) — holds
  `entries: BTreeMap<String, Entry>` keyed by the **normalized** in-archive
  name, plus `base` (the offset, non-zero for EXE-embedded) and `sections`.
  `Xp3Archive::entry(name)` normalizes and does a `BTreeMap` lookup;
  `read`/`read_entry` decode the entry's segments over the backing.
* An `Entry` is **metadata + segment descriptors, not bytes**: `raw_name`,
  `name`, `flags`, `org_size`, `arc_size`, `file_hash`, and
  `segments: Vec<Segment>` where `Segment { flags, start, org_size, arc_size }`.
  The bytes stay in the archive file (or the preload image); a read is
  positional over `Segment::start`/`arc_size`.
* `ArchiveBacking` is `Stream(FileState)` (the archive's `FileState` is
  L2-owned) or `Memory(Vec<u8>)` under `preload_archives`. Either way L1 holds
  no handle: L2 owns every `FileState` and the preloaded image, and hands L1 a
  borrow on each read.

**Disk entries are not held.** A disk name is probed live (`stat`/`list`) each
time resolution reaches it, and only a **hit** is cached — in
`ResolutionCache::placed: HashMap<String, Location>`, keyed by the requested
name, never a miss, matching `TVPAutoPathCache`
(`reference/cpp/core/base/StorageIntf.cpp:1101`). The auto-path candidate
tables are cached separately
(`ResolutionCache::auto_paths: HashMap<String, Rc<AutoPathTable>>`; a
`Folder` table of real children, or an `Archive` prefix table of base name →
in-archive name). Both are dropped by `clear_resolution_cache` on any
mount/unmount/auto-path change.

`find` returns `Location::Archive(path, normalized_name)` — a lightweight
reference *into* the held index, not an entry copy — so the read goes back
through the archive. An archive that was not mounted but exists on disk is
opened and parsed **on demand** inside `find_in_archive`; that parse is
transient and dropped after the call. L3-registered virtual entries live in a
third place, `media: RefCell<HashMap<String, Vec<u8>>>`, keyed by the full
storage name (e.g. `psb://…`), holding bytes only.

### Looking up entries

Everything the index holds is reachable through the `Storage` surface — the
archive list (`archives`) and each archive's file map (`Xp3Archive::entries`)
are resident, so a lookup is in-memory, not a scan of the container bytes.

| Want | Call | Notes |
|---|---|---|
| one name | `find(name) -> Result<Option<Location>, StorageReadError>` | full resolution; `Location::Disk(PathBuf)` or `Location::Archive(PathBuf, normalized)` |
| one name in a named archive | `find("arc.xp3>path")` | bypasses the mount scan; matches `file_name` case-insensitively |
| the bytes | `read(name)` / `read_prefix(name, max_len)` | re-resolves through `find` |
| existence (file or directory) | `exists(name) -> Result<bool>` | `TVPIsExistentStorage` semantics, so directories report true (`reference/cpp/core/base/StorageIntf.cpp:1120-1123`) |
| metadata | `stat(name) -> Result<Option<StorageMetadata>>` | archive: `size = entry.org_size`, all times `None` (XP3 has no timestamps); disk: `size` + modification time, `accessed`/`created` `None` |
| a directory's children | `list(dir) -> Result<Vec<String>>` | merges the disk dir and every mounted archive's children under the prefix; directories carry a trailing `/` |
| a wildcard | `file_list(mask, attr)` | disk + archive names; `arcmask>inarcmask` addressing; recursive under the literal prefix |
| the raw index | `for_each_archive(\|path, archive\| …)` | then `archive.entries()` / `archive.entry(name)` / `archive.len()`; `archive.near_misses(request, cap)` for a miss diagnostic |

`find`/`read`/`stat`/`exists` and the `getLocalName`/`getPlacedPath`/auto-path
family are reference members. `list`/`file_list` are L2 enumeration helpers with
**no** registered script member — there is no `Storages.getFileList` in
`reference/cpp`; the reference lists directories through the disk-only
`getDirList` (dirlist plugin) and `Storages.dirlist`/`dirlistEx`/`dirtree`
(fstat plugin), see §2.

The only per-entry fields the surface exposes are `name` and `size` (+ disk
times). The stored/compressed size, the protected flag, the hash and the
segment layout live in the L1 `Entry` and have no route to a script.

## 3. Object + state, not streams

The bottom layer is positional by design (D9). `open` returns an explicit
`FileState`; L2 stores it and owns the sequential cursor.

```rust
/// Stateless root object; carries only configuration (backend + base).
#[derive(Clone)]
pub struct FileSystem { /* host root | in-memory root | URI root */ }

pub fn create_file_system(config: FsConfig) -> FileSystem;

pub fn open(fs: &FileSystem, path: &FsPath, mode: OpenMode) -> Result<FileState, FsError>;

/// Open handle + logical cursor. Bottom-defined, L2-owned.
pub struct FileState { /* fd/asset handle + cursor */ }

pub fn read_at(fs: &FileSystem, file: &FileState, offset: u64, buf: &mut [u8])
    -> Result<usize, FsError>;
pub fn write_at(fs: &FileSystem, file: &FileState, offset: u64, buf: &[u8])
    -> Result<usize, FsError>;
pub fn file_size(fs: &FileSystem, file: &FileState) -> Result<u64, FsError>;
pub fn flush(fs: &FileSystem, file: &FileState) -> Result<(), FsError>;
/// Sequential convenience: reads at the cursor and advances L2's state.
pub fn read_seq(fs: &FileSystem, file: &mut FileState, buf: &mut [u8])
    -> Result<usize, FsError>;
```

The **sequential** stream the scripts see is an L2 value wrapping a
`FileState` + offset. There is only one byte-IO mechanism in L1, and no
open-file registry.

`Xp3Archive` is parsed in L1 over positional reads (or a buffered `Vec<u8>`)
instead of a live `std::fs::File`; it stores only the base offset and the
entry map, and every read takes the owning `FileState` (or byte image)
explicitly. L2 owns the `FileState`s and the parsed index. This is the
prerequisite that also unlocks:
* concurrent reads (no shared cursor) for parallel media/image workers;
* Android `AssetManager`/SAF sources;
* in-memory test fixtures.

## 4. Backends

### 4.1 Host path root (Linux + Android app-private dirs)

`std::fs`-based. Used for:
* the game directory (desktop, and Android's resolved external path under
  option A);
* the app data dir (`filesDir` on Android, `temp_dir()` elsewhere);
* savedata (`savedata/` under the game dir or app data dir).

### 4.2 In-memory root (tests + wasm-style future)

An in-memory map. This is what L2 unit tests use and what makes the storage
namespace testable without a game tree. It is also the natural base if a
web target is ever revisited (`docs/portability.md`).

### 4.3 Android asset / SAF roots (optional, later)

* `AssetManager` for APK-bundled game data (`AAssetManager_open`), if the
  project ever ships data in the APK (currently it does not).
* SAF/`ContentResolver` streaming for permission-free external storage
  (option B in `docs/android.md §4`, explicitly deferred). The `FsPath::Uri`
  variant in the API exists so this can be added without an API change.

The plan keeps the current Android decision (SAF for picking, resolved path
for reading, `MANAGE_EXTERNAL_STORAGE`) as the default, because it already
works and option B is a large JNI-per-read undertaking. The stateless fs
surface makes option B *possible* without a second rewrite.

## 5. Save data, logging and state files

These are not game-storage reads but still OS IO:

* on Android the rotating `krkr.log` (`crates/android/src/log_file.rs`) and the
  `krkr_state.json` startup-state file (`crates/android/src/state.rs`) are the
  launcher's contract; both are plain `std` writers and are host-tested;
* savedata (`savedata/`) is routed through the storage namespace;
* `Debug.startLogToFile` is a native that writes through the host seam.

`System.appDataPath` is not a host fact: the reference's `TVPGetAppDataPath()`
returns `TVPGetAppPath()` unconditionally
(`reference/cpp/core/base/impl/SystemImpl.cpp:366-380`), so the getter reuses
the application-path helper alongside `System.exePath`.

## 6. The C++ `fopen` bypass

`tjs/cpp/streams.cpp` implements TJS `Array.save/load/saveStruct` with
`std::fopen` directly, bypassing Rust storage. This is the one IO path the
stateless fs surface cannot reach. Options:

| Option | Description | Cost |
|--------|-------------|------|
| A | Add an IO hook in `cpp/streams.cpp` that calls back into Rust (function-pointer table set at init). | C ABI addition + threading care; keeps TJS file semantics. |
| B | Reimplement the save/load natives in Rust and stop using the C++ stream. | Touches the TJS ABI; must match serialization exactly. |
| C | Leave as-is; document `std::fopen` as a platform escape hatch. | Degrades on Android/scoped storage; keeps `std::fs` reachable outside L1. |

The port currently keeps option C: `streams.cpp` is the only port-authored
C++ besides the ABI shim, and it still uses `std::fopen`. Replacing it with a
thin C++ adapter over a Rust implementation is deferred (`TODO.md` §16).

## 7. Case-insensitivity and archives

The recursive case-insensitive disk walk (`engine/src/storage.rs:47-70`) and
the archive entry map are performance-sensitive on large games (23,572
entries measured in `docs/optimization.md`). The middle layer should:

* index the storage namespace once at mount (name → source) instead of
  doing `Path::is_file` per lookup;
* use positional reads (`pread`/`read_at`) so archive reads can be
  parallelized across media/image workers without a shared cursor;
* support an in-memory-root-backed pre-loader for tests.

None of this changes the stateless fs surface; it is L2 optimization and
L2-owned index state.

### 7.1 Whole-archive preload (`preload_archives`)

`RuntimeConfig::preload_archives` (`--preload-archives` on the CLI,
`preload_archives=true` in the desktop config file) makes
`engine::storage::Storage` read each mounted archive into memory in full at
mount. This is the answer to a slow backing store: with the game's 633 MB
`data.xp3` on an NTFS-3G/FUSE mount, per-entry random reads dominate wall
time (a headless run measured `real 2m00s` against `user 5.3s`), and reading
the archive once, sequentially, removes them.

* The cost is **the archive's own size** in resident memory — 633 MB for a
  633 MB archive, summed over every mounted archive. There is no cap and no
  heuristic; the user opts in.
* `Storage::mount_archives_with` and `mount_discovered_archives_with` take
  the choice through `MountOptions::preload_archives`; the plain
  `mount_archives` / `mount_discovered_archives` constructors keep the
  streaming backing.
* With the flag off the path is unchanged: the archive file stays open and
  every entry read is positional. With it on, an internal `ArchiveBacking`
  enum holds the image (`Vec<u8>`) instead of the open handle.
* Mount order and precedence do not change. A preload failure on a scanned
  archive is skipped and warned about exactly like a parse failure, and on
  an explicitly requested archive it is a typed `MountError`
  (`TruncatedArchive`, `ArchiveTooLarge`, `ArchiveAllocation`); it is never
  collapsed into a silent fall back to the streaming path.
* The index is still parsed eagerly at mount — it is small, and `find`,
  `stat` and listing need it. Preloading only materializes the entry data.

**Layering.** `fs::xp3` stays stateless: `Xp3Archive` stores only the base
offset and the entry map, and `read_entry_from_bytes` takes the image as an
explicit `&[u8]` argument. L2 owns the `Vec<u8>` and hands a view of it to L1
on every entry read; L1 never caches it, keeps it, or returns it.

## 8. Platform notes

* **Linux**: host path root, `std::fs`. `xdg-open` etc. stay in `System`.
* **Android**: host path root for app-private dirs and the resolved game
  path; state/log paths come from the JNI `pendingStateDir`; `System.appDataPath`
  falls back to the state dir (`runner.rs app_data_dir`).
* **wasm** (not planned): a `fetch`-backed in-memory root plus OPFS saves
  would implement the same functions; the in-memory root is the stepping
  stone.

## 9. Implementation

* The stateless `FileSystem`/`FileState` API lives in `api::fs` + `fs`
  (`03-middle-layer-api.md §1`), with a host root and an in-memory root.
* XP3 is read and parsed in L1 `fs::xp3` over positional reads, returning
  parsed index metadata; L2 owns the `FileState`s and any buffering.
* `engine::Storage` sits on top of the fs functions and owns the namespace,
  the mount index and the auto-path table.
* L3 file IO calls the L1 `fs` abstraction.
* Android/SAF roots are not wired; the `FsPath::Uri` variant is reserved for
  them.
* The `cpp/streams.cpp` hook is not wired (option C, §6).
