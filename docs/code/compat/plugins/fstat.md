# fstat (`fstat.dll`)

**DLL name:** `fstat.dll` — linked with `Plugins.link("fstat.dll")`
**Rust module:** `crates/compat/src/natives/plugins/fstat/`
**Reference:** `reference/cpp/plugins/fstat/main.cpp`
**Registers:** 30 static members on the core `Storages` class, the `Storages.currentPath` property, and the `TemporaryFiles` class

## Surface

### `Storages` members

| member | kind | behaviour |
|---|---|---|
| `clearStorageCaches()` | static method | drops the storage namespace's cached lookups |
| `fstat(filename)` | static method | a dictionary with the size and the `Date` timestamps |
| `getTime(target)` | static method | the modification time of a storage as a `Date` |
| `setTime(target, times)` | static method | applies the dictionary's `mtime` `Date` |
| `getLastModifiedFileTime(target)` | static method | always `0` (the reference's value) |
| `setLastModifiedFileTime(target, time)` | static method | sets the modification time |
| `exportFile(filename, storename)` | static method | copies a storage entry to a disk file |
| `deleteFile(file)` | static method | deletes an existing disk file |
| `truncateFile(file, size)` | static method | a reference no-op: warns and returns `true` |
| `moveFile(from, to)` | static method | copies then deletes, as the reference does |
| `dirlist(dir)` | static method | the immediate child names of `dir` |
| `dirlistEx(dir)` | static method | the immediate children as name/size/isDirectory records |
| `dirtree(dir, dironly=false)` | static method | every descendant below `dir` |
| `removeDirectory(dir)` | static method | removes an empty directory |
| `createDirectory(dir)` | static method | creates a directory (normalized name) |
| `createDirectoryNoNormalize(dir)` | static method | creates a directory without normalizing the name |
| `changeDirectory(dir)` | static method | a reference no-op: validates the trailing `/`, warns, returns `true` |
| `setFileAttributes(filename, attr)` | static method | a reference no-op: warns and returns `true` |
| `resetFileAttributes(filename, attr)` | static method | a reference no-op: warns and returns `true` |
| `getFileAttributes(filename)` | static method | a reference no-op: warns and returns `0` |
| `selectDirectory(params)` | static method | a reference no-op: validates one argument, warns, returns `true` |
| `isExistentDirectory(dir)` | static method | whether `dir` is an existing directory |
| `copyFile(from, to)` | static method | copies through the placed/resolved paths |
| `copyFileNoNormalize(from, to, failIfExist)` | static method | copies without normalizing the names |
| `isExistentStorageNoSearchNoNormalize(filename)` | static method | existence check without search or normalization |
| `getDisplayName(filename)` | static method | the base file name of an existing storage |
| `getMD5HashString(filename)` | static method | the lowercase hex MD5 of the storage's bytes |
| `searchPath(filename, searchpath)` | static method | searches a `:`/`;`-separated path list |
| `getTemporaryName()` | static method | a unique name under the game directory |
| `currentPath` | property (read/write) | the normalized game/storage path |

### `TemporaryFiles` members

| member | kind | behaviour |
|---|---|---|
| `TemporaryFiles()` | constructor | creates the validator object |
| `entry(name)` | method | `true` when the temporary file exists |
| `entryFolder(name)` | method | `true` when the temporary folder exists |

## Behaviour

### Storage resolution

All storage namespace policy — normalization, the `arc.xp3>path` form, case
folding, auto paths — lives in the engine's `Storage` attached to the
compatibility services. This plugin never reimplements it: it strips the L3
media prefix and hands the bare name to the namespace.

A name that resolves to a disk file uses the real on-disk path. A name that
resolves inside an archive is reported by `fstat` with its **uncompressed size
only**, exactly like the reference — no timestamps, because an archive entry
has none.

### `fstat`

`fstat` returns a dictionary. For a disk file it carries the size and the
`mtime` / `ctime` / `atime` timestamps, each a TJS `Date`. For an archive
entry it carries the size and an empty timestamp set. A directory has no
size, so the size entry is absent.

### Timestamps and `Date`

The reference stores file times in TJS `Date` objects by calling
`Date.setTime(filetime)` with the raw `tTVP_stat` seconds
(`reference/cpp/plugins/fstat/main.cpp:112-133`) and reads them back by
handing `Date.getTime()` straight to `TVP_utime`
(`reference/cpp/plugins/fstat/main.cpp:135-148,215-218`). `Date.setTime`
divides by 1000 and `Date.getTime` multiplies by 1000
(`reference/cpp/core/tjs2/tjsDate.cpp:209-219,300-306`), so the observable
`Date.getTime()` value equals the file time the reference round-trips — raw
epoch seconds truncated to a multiple of 1000 — while the `Date` *displays* as
epoch + filetime/1000.

The port reproduces the shipped behaviour verbatim: no unit conversion at
either boundary.

### Directory listings

`dirlist` returns the immediate child names. The directory name must end in
`/`, otherwise the member raises
`"'/' must be specified at the end of given directory name."`
(`reference/cpp/plugins/fstat/main.cpp:509-512`). A directory that cannot be
listed raises `Storages.dirlist: Directory not found.`

`dirlistEx` returns the immediate children as records carrying the name, the
size and whether the entry is a directory. `dirtree` returns every descendant
below `dir`, and with `dironly` set only the directories.

Note that `dirlist` does **not** lowercase the names; the reference's
separate `getDirList` global does, because it reads through
`TVPGetLocalFileListAt`. See [`dirlist`](dirlist.md).

### Reference no-ops

A handful of the reference members are explicitly unimplemented on
non-Windows platforms and only log a warning: `truncateFile`,
`changeDirectory`, `setFileAttributes`, `resetFileAttributes`,
`getFileAttributes` and `selectDirectory`. They keep the reference's
documented return values (a warning plus `true`, or `0` for the getter).
They are not silent stubs — each one warns — and they are listed here.

`changeDirectory` and `selectDirectory` still validate their arguments before
warning, so a malformed call raises the reference's argument error rather than
returning the no-op value.

### `TemporaryFiles`

`TemporaryFiles` follows the reference's implementation, which validates but
does not record entries. Its native payload is a unit value; the reference's
`~TemporaryFiles` has nothing to release either.

## State and lifetime

* The plugin keeps **no** state of its own. Every member goes to the attached
  engine `Storage` or to the file system.
* `Storages.currentPath` reads and writes the namespace's current path.
* `TemporaryFiles` holds a unit payload.

## Errors

| condition | outcome |
|---|---|
| a member is called with the wrong argument count | typed error: `<member> requires N argument(s)` |
| no storage is attached | typed error: `<member>: no storage attached` |
| the storage raises | the storage's error, prefixed with the member name |
| a directory name does not end in `/` | typed error: `'/' must be specified at the end of given directory name.` |
| a directory cannot be listed | typed error: `Storages.dirlist: Directory not found.` |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/fstat.rs` | the `fstat` dictionary for a disk file and for an archive entry, the `Date` round trip, `getTime` / `setTime`, the copy / move / delete members, the directory helpers, `getMD5HashString`, `getDisplayName`, `searchPath` and `TemporaryFiles` |
| `crates/compat/tests/storages_io.rs` | that the file-manipulation members belong to `fstat` and are not registered twice |
| `crates/compat/src/natives/plugins/fstat/mod.rs` (unit tests) | the reference semantics of *this* implementation in isolation from the sibling plugins that register later: the `Date`-valued timestamps, `TemporaryFiles`, and the directory helpers |

## Known gaps

* The reference's Windows-only members (`truncateFile`, `changeDirectory`,
  `setFileAttributes`, `resetFileAttributes`, `getFileAttributes`,
  `selectDirectory`) are unimplemented in the reference on non-Windows
  platforms too, and the port reproduces that: a warning plus the reference's
  return value.
* The reference's `fstat` on an archive entry reports the uncompressed size
  only. The port does the same, so an archive entry has no timestamps.
* The reference's dictionary walk for `TemporaryFiles` records nothing, and
  neither does the port — `entry` and `entryFolder` only validate.
