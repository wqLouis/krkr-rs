# 06 — File system

Status: design sketch. Extracts raw byte IO into the bottom core and moves
the TVP storage namespace on top of it.

Goal: a bottom-layer `FileSystem` that knows about paths, URIs, streams,
stat and listing — and nothing about `.xp3`, `arc.xp3>path` addressing,
case-insensitive search or TJS. The middle layer keeps TVP's storage
semantics; the natives keep their current surface.

---

## 1. Inventory being replaced

| Concern | Today | Target |
|---------|-------|--------|
| Game mount + namespace (`arc.xp3>path`, disk-over-archive, case rules, auto-path) | `engine/src/storage.rs` (concrete `PathBuf`/`fs`) | L2 `krkr-engine::storage` over `FileSystem` |
| XP3 container parsing + segment read | `crates/xp3/src/archive.rs` (owns `std::fs::File`, seek/read) | L2, reading a `Stream` from `FileSystem` |
| Raw host path IO | `std::fs` scattered in `engine`, `tvp-storages`, `tvp-visual`, `tvp-natives`, `engine::logging`, `engine::state` | L1 `krkr-fs` |
| Stream abstraction | `tvp-streams::binary::BinaryStream` (exists, unused by storage) | promoted to the shared `Stream` definition |
| TJS `Array.save/load/saveStruct` | C++ `std::fopen` in `tjs2-sys/cpp/streams.cpp` | unresolved; see §6 |
| Android SAF / APK assets | SAF→path + `MANAGE_EXTERNAL_STORAGE` (option A, `docs/android.md §4`) | optional `Uri` backend |

The read path today has **no trait** (`docs/portability.md`); this is the
largest structural change of the three bottom components.

## 2. Boundaries

### Bottom (`krkr-fs`) knows

* open a path or URI in read/write/append mode;
* read/write/seek/flush/size on the returned stream;
* stat, exists, list a directory, create dirs, remove, copy, rename;
* the host path dialect (Linux `PathBuf`, Android `PathBuf` for app-private
  dirs and resolved external paths, Android content/asset `Uri`).

### Middle (`krkr-engine`) knows

* storage namespaces: game dir + mounted archives, disk-wins ordering;
* `arc.xp3>path` addressing and the specific-archive prefix;
* normalization (lowercase, `/` separators) and the recursive
  case-insensitive walk that makes Windows-authored games work on Linux;
* the auto-search-path list (`engine/src/storage.rs` global);
* archive containers (`xp3`, and optionally the unused `tvp-archive`
  zip/7z/tar readers), built over `FileSystem` streams;
* `Storages` native semantics: `stat`, `getFileList`, `open`, `fstat`,
  `deleteFile`, `copyFile`, save-data writes.

### Natives (`krkr-tjs`) keep

The exact same method surface and error strings; only their implementation
calls the middle layer instead of touching `std::fs`.

## 3. Streams

Use one stream trait across the workspace. The existing
`tvp-streams::binary::BinaryStream` (`binary.rs:57`) is the candidate: it
already has `File` and `MemoryStream` impls and mirrors the reference
`tTJSBinaryStream`. Promote it (or a `krkr-api::Stream`) to the shared
definition and make `FileSystem::open` return `Box<dyn Stream>`.

Important: `Xp3Archive` must be reworked to parse from a `Read + Seek`
source (or a buffered `Vec<u8>`) instead of a live `std::fs::File`. This is
the prerequisite that also unlocks:
* concurrent reads (`pread`/per-worker handles instead of one shared
  cursor);
* Android `AssetManager`/SAF sources;
* in-memory test fixtures.

## 4. Backends

### 4.1 `StdFileSystem` (Linux + Android app-private dirs)

`std::fs`-based. Used for:
* the game directory (desktop, and Android's resolved external path under
  option A);
* the app data dir (`filesDir` on Android, `temp_dir()` elsewhere);
* savedata (`savedata/` under the game dir or app data dir).

### 4.2 `MemoryFileSystem` (tests + wasm-style future)

An in-memory map. This is what L2 unit tests use and what makes the storage
namespace testable without a game tree. It is also the natural base if a
web target is ever revisited (`docs/portability.md`).

### 4.3 `AndroidAssetFileSystem` / `SafFileSystem` (optional, later)

* `AssetManager` for APK-bundled game data (`AAssetManager_open`), if the
  project ever ships data in the APK (currently it does not).
* SAF/`ContentResolver` streaming for permission-free external storage
  (option B in `docs/android.md §4`, explicitly deferred). The `FsPath::Uri`
  variant in the API exists so this can be added without an API change.

The plan keeps the current Android decision (SAF for picking, resolved path
for reading, `MANAGE_EXTERNAL_STORAGE`) as the default, because it already
works and `option B` is a large JNI-per-read undertaking. The FS trait makes
option B *possible* without a second rewrite.

## 5. Save data, logging and state files

These are not game-storage reads but still OS IO:

* logging sink (`engine/src/logging.rs`, rotating `krkr.log`) → `FileSystem`
  or a dedicated `LogSink` writer;
* state file (`engine/src/state.rs`, `krkr_state.json`) → `FileSystem`;
* savedata (`savedata/`) → `FileSystem`;
* `Debug.startLogToFile` → `FileSystem`.

All of them become testable with `MemoryFileSystem`.

## 6. The C++ `fopen` bypass

`tjs2-sys/cpp/streams.cpp` implements TJS `Array.save/load/saveStruct` with
`std::fopen` directly, bypassing Rust storage. This is the one IO path the
FS trait cannot reach. Options:

| Option | Description | Cost |
|--------|-------------|------|
| A | Add an IO hook in `cpp/streams.cpp` that calls back into Rust (function-pointer table set at init). | C ABI addition + threading care; keeps TJS file semantics. |
| B | Reimplement the save/load natives in Rust (`krkr-tjs`) and stop using the C++ stream. | Touches the TJS ABI; must match serialization exactly. |
| C | Leave as-is; document `std::fopen` as a platform escape hatch. | Degrades on Android/scoped storage; keeps `std::fs` reachable outside L1. |

Plan: **A** if save files must go through the trait; otherwise **C** is
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
* support a `MemoryFileSystem`-backed pre-loader for tests.

None of this changes the `FileSystem` trait; it is L2 optimization.

## 8. Platform notes

* **Linux**: `StdFileSystem`, `std::fs`. `xdg-open` etc. stay in `System`.
* **Android**: `StdFileSystem` for app-private dirs and the resolved game
  path; state/log paths come from the JNI `pendingStateDir`; `System.appDataPath`
  falls back to the state dir (`runner.rs app_data_dir`).
* **wasm** (not planned): a `fetch`-backed in-memory mount plus OPFS saves
  would implement the same trait; the `MemoryFileSystem` design is the
  stepping stone.

## 9. Migration steps specific to FS

1. Define/promote `Stream` + `FileSystem` in `krkr-api` and
   `tvp-streams`.
2. Implement `StdFileSystem` and `MemoryFileSystem`.
3. Rework `xp3::Xp3Archive` to read from a `Stream`/`Vec<u8>`; add
   `read_at`-style positional reads.
4. Move `engine::Storage` into L2 `krkr-engine` on top of `FileSystem`;
   keep the current public API as a shim for natives.
5. Route logging/state/savedata/native file IO through the trait.
6. Add Android backends only if/when APK assets or SAF streaming are needed.
7. Decide the `cpp/streams.cpp` hook.

Each step is independently testable with `MemoryFileSystem` and preserves the
existing `Storages`/`Scripts` behavior.
