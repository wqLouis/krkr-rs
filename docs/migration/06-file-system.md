# 06 — File system

Status: design sketch. Extracts raw byte IO into the bottom core and moves
the TVP storage namespace on top of it.

Goal: a **stateless `FileSystem` object** plus an explicit **`FileState`**
value. L1 knows about paths, URIs, handles, stat and listing — and nothing
about `.xp3`, `arc.xp3>path` addressing, case-insensitive search or TJS. The
middle layer stores the `FileState`s and the namespace index; the natives
keep their current surface.

---

## 1. Inventory being replaced

| Concern | Today | Target |
|---------|-------|--------|
| Game mount + namespace (`arc.xp3>path`, disk-over-archive, case rules, auto-path) | `engine/src/storage.rs` (concrete `PathBuf`/`fs`) | L2 `krkr-engine::storage` over the `FileSystem` object |
| XP3 container parsing + segment read | `crates/xp3/src/archive.rs` (owns `std::fs::File`, seek/read) | L1 `krkr-fs::xp3`, a stateless parse + positional read over a `FileState`; L2 registers the parsed index |
| Raw host path IO | `std::fs` scattered in `engine`, `tvp-storages`, `tvp-visual`, `tvp-natives`, `engine::logging`, `engine::state` | L1 `krkr-fs` (`FileSystem` + `FileState`) |
| Stream abstraction | `tvp-streams::binary::BinaryStream` (exists, unused by storage) | L2 wrapper over a `FileState` + cursor |
| TJS `Array.save/load/saveStruct` | C++ `std::fopen` in `tjs2-sys/cpp/streams.cpp` | unresolved; see §6 |
| Android SAF / APK assets | SAF→path + `MANAGE_EXTERNAL_STORAGE` (option A, `docs/android.md §4`) | optional `Uri` root |

The read path today has **no abstraction** (`docs/portability.md`); this is
the largest structural change of the three bottom components.

## 2. Boundaries

### Bottom (`krkr-fs`) owns

* the stateless `FileSystem` object (backend kind + base);
* stateless functions: open, stat, exists, list, create dirs, remove, copy,
  rename;
* the `FileState` value: the open handle **and** the logical cursor;
* positional `read_at`/`write_at`, `size`, `flush` over a `FileState`;
* the XP3 container read + parse (`krkr-fs::xp3`): given the bytes through a
  `FileState`, it returns only parsed index metadata (base offset + entry
  map) and reads segments through positional `read_at`. It holds no file
  handle and no cursor of its own;
* the host path dialect (Linux `PathBuf`, Android `PathBuf` for app-private
  dirs and resolved external paths, Android content/asset `Uri`).

It knows **nothing** about namespace resolution, caches or which archive a
name belongs to. It never keeps a table of open files; L2 stores each
`FileState`.

### Middle (`krkr-engine`) owns

* storage namespaces: game dir + mounted archives, disk-wins ordering;
* `arc.xp3>path` addressing and the specific-archive prefix;
* normalization (lowercase, `/` separators) and the recursive
  case-insensitive walk that makes Windows-authored games work on Linux;
* the auto-search-path list and the mount index (name → source);
* the parsed archive index returned by L1, plus the `FileState`s that own
  each archive's bytes — the namespace state. L2 never reads or parses the
  XP3 container itself; that is L1 (see the D6 note below);
* `Storages` native semantics: `stat`, `getFileList`, `open`, `fstat`,
  `deleteFile`, `copyFile`, save-data writes.

> **D6 note.** The XP3 container is read and parsed in L1
> (`krkr-fs::xp3`), because parsing is a stateless transform over bytes.
> L2 only owns the resulting parsed index and the open handles. The middle
> layer therefore does **not** own the `xp3` container; this corrects the
> earlier sketch in this document and is the contract implemented by
> `krkr-engine::storage` and `docs/migration/09-game-loading.md`.

### Natives (`krkr-tjs`) keep

The exact same method surface and error strings; only their implementation
calls the middle layer instead of touching `std::fs`.

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

The existing `tvp-streams::binary::BinaryStream` (`binary.rs:57`) remains the
**sequential** stream the scripts see; it becomes an L2 value wrapping a
`FileState` + offset. There is only one byte-IO mechanism in L1, and no
open-file registry.

`Xp3Archive` is parsed in L1 over positional reads (or a buffered `Vec<u8>`)
instead of a live `std::fs::File`; it stores only the base offset and the
entry map, and every read takes the owning `FileState` explicitly. L2 owns
the `FileState`s and the parsed index. This is the prerequisite that also
unlocks:
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

* logging sink (`engine/src/logging.rs`, rotating `krkr.log`) → the
  `FileSystem` object or a dedicated `LogSink` writer;
* state file (`engine/src/state.rs`, `krkr_state.json`) → `FileSystem`;
* savedata (`savedata/`) → `FileSystem`;
* `Debug.startLogToFile` → `FileSystem`.

All of them become testable with the in-memory root.

## 6. The C++ `fopen` bypass

`tjs2-sys/cpp/streams.cpp` implements TJS `Array.save/load/saveStruct` with
`std::fopen` directly, bypassing Rust storage. This is the one IO path the
stateless fs surface cannot reach. Options:

| Option | Description | Cost |
|--------|-------------|------|
| A | Add an IO hook in `cpp/streams.cpp` that calls back into Rust (function-pointer table set at init). | C ABI addition + threading care; keeps TJS file semantics. |
| B | Reimplement the save/load natives in Rust (`krkr-tjs`) and stop using the C++ stream. | Touches the TJS ABI; must match serialization exactly. |
| C | Leave as-is; document `std::fopen` as a platform escape hatch. | Degrades on Android/scoped storage; keeps `std::fs` reachable outside L1. |

Plan: **A** if save files must go through the fs surface; otherwise **C** is
acceptable for the first migration milestones and revisited before Android
savedata hardening. This is listed as an open question in
`07-roadmap.md §7`.

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

## 8. Platform notes

* **Linux**: host path root, `std::fs`. `xdg-open` etc. stay in `System`.
* **Android**: host path root for app-private dirs and the resolved game
  path; state/log paths come from the JNI `pendingStateDir`; `System.appDataPath`
  falls back to the state dir (`runner.rs app_data_dir`).
* **wasm** (not planned): a `fetch`-backed in-memory root plus OPFS saves
  would implement the same functions; the in-memory root is the stepping
  stone.

## 9. Migration steps specific to FS

1. Define the stateless `FileSystem`/`FileState` API in `krkr-api`
   (`03-middle-layer-api.md §1`).
2. Implement the host path root + the in-memory root.
3. Read + parse XP3 in L1 `krkr-fs::xp3` over positional reads, returning
   only parsed index metadata; L2 owns the `FileState`s and any buffering.
4. Move `engine::Storage` into L2 `krkr-engine` on top of the fs functions;
   keep the current public API as a shim for natives.
5. Route logging/state/savedata/native file IO through the fs functions.
6. Add Android/SAF roots only if/when APK assets or SAF streaming are needed.
7. Decide the `cpp/streams.cpp` hook.

Each step is independently testable with the in-memory root and preserves
the existing `Storages`/`Scripts` behavior.
