# fstat (`fstat.dll`)

**DLL name:** `fstat.dll` — linked with `Plugins.link("fstat.dll")`
**Rust module:** `crates/compat/src/natives/plugins/fstat/`
**Reference:** `reference/cpp/plugins/fstat/main.cpp`
**Registers:** `Storages` (attached members), `TemporaryFiles`

The reference plugin does two things: it attaches a class of file-statistics,
directory, copy/move/delete and path helpers to the core `Storages` class
(`reference/cpp/plugins/fstat/main.cpp:918-949`), and it registers the small
instance class `TemporaryFiles`, whose members validate a temporary file or
folder (`reference/cpp/plugins/fstat/main.cpp:952-991`).

The port attaches all 30 `Storages` members as **static** members of the class
object, so a script calls them exactly as the reference does:
`Storages.fstat("data/a.png")`. The `Storages` core class itself (its
auto-path, placement and extraction members) belongs to
`crates/compat/src/natives/storages.rs`; see the [plugin index](../plugins.md).

## Surface

### `Storages` members

| member | kind | behaviour |
|---|---|---|
| `clearStorageCaches()` | static method | a documented no-op: the reference clears the storage caches (`TVPClearStorageCaches`), this port does not clear the namespace's resolution cache |
| `fstat(filename)` | static method | a dictionary with the size and the `mtime`/`ctime`/`atime` timestamps of a disk file; an archive member reports its uncompressed `size` only |
| `getTime(target)` | static method | the same dictionary without `size`; disk names only |
| `setTime(target, times)` | static method | applies the dictionary's `mtime` `Date` to the file; always reports `true` |
| `getLastModifiedFileTime(target)` | static method | always `0` — the reference's inverted stat check |
| `setLastModifiedFileTime(target, time)` | static method | writes `time` (seconds) as the file's access and modification times |
| `exportFile(filename, storename)` | static method | copies a storage entry (disk or archive) to a local file |
| `deleteFile(file)` | static method | deletes an existing disk file |
| `truncateFile(file, size)` | static method | reference no-op: warns and returns `true` without touching the file |
| `moveFile(from, to)` | static method | copies the source and then deletes it |
| `dirlist(dir)` | static method | the immediate child names of `dir` |
| `dirlistEx(dir)` | static method | the immediate children as `name`/`size`/`attrib`/`mtime`/`ctime`/`atime` dictionaries |
| `dirtree(dir, dironly=false)` | static method | every descendant below `dir`, directories suffixed with `/` |
| `removeDirectory(dir)` | static method | removes a directory |
| `createDirectory(dir)` | static method | creates a directory and its parents from a normalized name |
| `createDirectoryNoNormalize(dir)` | static method | the same without normalizing the name |
| `changeDirectory(dir)` | static method | reference no-op: validates the trailing `/` and the local name, warns, returns `true` |
| `setFileAttributes(filename, attr)` | static method | reference no-op: validates the local name, warns, returns `true` |
| `resetFileAttributes(filename, attr)` | static method | reference no-op: validates the local name, warns, returns `true` |
| `getFileAttributes(filename)` | static method | reference no-op: validates the local name, warns, returns `0` |
| `selectDirectory(params)` | static method | reference no-op: validates one argument, warns, returns `void` |
| `isExistentDirectory(dir)` | static method | whether `dir` is an existing directory |
| `copyFile(from, to)` | static method | copies the placed source to the normalized destination |
| `copyFileNoNormalize(from, to, failIfExist)` | static method | copies without normalizing the destination; `failIfExist` is accepted and ignored |
| `isExistentStorageNoSearchNoNormalize(filename)` | static method | existence check without an auto-path search and without normalizing |
| `getDisplayName(filename)` | static method | the base name of an existing local file, `""` when there is none |
| `getMD5HashString(filename)` | static method | the lowercase 32-character hex MD5 of the storage entry's bytes |
| `searchPath(filename, searchpath)` | static method | the normalized absolute path of `filename` in the search path, `void` when nothing matched |
| `getTemporaryName()` | static method | a unique `krkr_<12 hex>_<counter>_<pid>` name under the game directory |
| `currentPath` | static property (read/write) | the normalized game directory with a trailing `/`; the write forwards to `changeDirectory` |

### `TemporaryFiles` members

| member | kind | behaviour |
|---|---|---|
| `TemporaryFiles()` | constructor | `new TemporaryFiles()`; the class-named member lets a script subclass reach the native instance through `super.TemporaryFiles()` |
| `entry(name)` | method | `true` when the name is an existing, openable regular file |
| `entryFolder(name)` | method | `true` when the name is an existing directory |

The reference registers the class with a constructor and the two validators
(`reference/cpp/plugins/fstat/main.cpp:987-991`); the parity report counts
`TemporaryFiles` at 2 of 2 members
([`docs/plugin_parity.md`](../../../plugin_parity.md)) — the class-named
constructor is the constructor, not a member.

### The `Storages` count in the parity report

The parity report lists `Storages` at 45 reference members and 32 Rust
members, all matched
([`docs/plugin_parity.md`](../../../plugin_parity.md)). The 30 members above
plus the two `xp3_filter` setters make up the 32; the 13 remaining members of
the class are core members registered by `crates/compat/src/natives/storages.rs`
and are counted separately.

### Globals

The reference's post-registration script installs seven constants
(`reference/cpp/plugins/fstat/main.cpp:43-51,1007`); the port installs the
same seven when the plugin registers.

| global | value |
|---|---|
| `FILE_ATTRIBUTE_READONLY` | `0x00000001` |
| `FILE_ATTRIBUTE_HIDDEN` | `0x00000002` |
| `FILE_ATTRIBUTE_SYSTEM` | `0x00000004` |
| `FILE_ATTRIBUTE_DIRECTORY` | `0x00000010` |
| `FILE_ATTRIBUTE_ARCHIVE` | `0x00000020` |
| `FILE_ATTRIBUTE_NORMAL` | `0x00000080` |
| `FILE_ATTRIBUTE_TEMPORARY` | `0x00000100` |

## Behaviour

### Storage resolution

All storage-namespace policy — normalization, the `arc.xp3>path` form, case
folding, auto paths — lives in the engine's `Storage` attached to the
compatibility services. This plugin never reimplements it: it strips the
compatibility layer's `file://.` media prefix (the `file://` and `file:`
spellings too) and hands the bare name to the namespace.

A name that resolves to a disk file uses the real on-disk path, case-corrected
by the namespace's walk. A name inside an archive has no locally accessible
form, so every member that needs a local path raises
`Cannot get local name from <name>` — the reference's `TVPCannotGetLocalName`
(`reference/cpp/core/base/StorageIntf.cpp:546-550`,
`reference/cpp/core/base/MsgIntfInc.h:35`).

### `fstat` and `getTime`

`fstat` resolves the name first. An archive member takes the reference's
archive branch and yields a dictionary with the **uncompressed size only**
(`reference/cpp/plugins/fstat/main.cpp:264-284`); a disk file takes the
`_getTime` branch and yields the size plus the three timestamps
(`reference/cpp/plugins/fstat/main.cpp:222-247`).

A directory has no `size` member — the reference fills `size` only for a
non-directory (`reference/cpp/plugins/fstat/main.cpp:193-195`). A missing file
is a zero-size entry with no timestamps at all: the reference ignores
`TVP_stat`'s return value and reads the zero-initialised `tTVP_stat`
(`reference/cpp/plugins/fstat/main.cpp:186-200`), and `storeDate` stores a
time only when it is greater than zero
(`reference/cpp/plugins/fstat/main.cpp:112-116`).

`getTime` is the same dictionary without `size`, and it is disk-only: an
archive name has no local name, so it raises
(`reference/cpp/plugins/fstat/main.cpp:153-154,222-231`).

### Timestamps and `Date`

The reference stores a file time in a TJS `Date` by handing `Date.setTime` the
raw `tTVP_stat` seconds (`reference/cpp/plugins/fstat/main.cpp:112-133`) and
reads it back by handing `Date.getTime()` straight to `TVP_utime`
(`reference/cpp/plugins/fstat/main.cpp:135-148,215-218`). `Date.setTime`
divides by 1000 and `Date.getTime` multiplies by 1000
(`reference/cpp/core/tjs2/tjsDate.cpp:209-219,300-306`), so the observable
`Date.getTime()` value is the file time in seconds — truncated to a multiple of
1000 by the round trip — while the `Date` *displays* as epoch + filetime/1000.
The port reproduces this verbatim: no unit conversion at either boundary.

`setTime` restores the file time from the dictionary's `mtime` member only.
When the dictionary holds no usable `Date`, the reference still calls
`TVP_utime(..., 0)` (`hasC ? m : 0`,
`reference/cpp/plugins/fstat/main.cpp:215-218`), so the file's times are
clobbered to the epoch; the port does the same and still reports `true`
(`reference/cpp/plugins/fstat/main.cpp:320-322`).

### The fast timestamp members

`getLastModifiedFileTime` inverts its own stat check: `TVP_stat` returns
**true on success**
(`reference/cpp/core/environ/linux/Platform.cpp:252-264`), so
`if(TVP_stat(...)) return 0;` returns `0` for every existing file, and a
failed stat falls through to the zero-initialised struct, which is `0` as well
(`reference/cpp/plugins/fstat/main.cpp:331-338`). The shipped implementation
never returns a timestamp; the port pins the shipped behaviour. The name is
still validated first, so an in-archive name raises.

`setLastModifiedFileTime(target, time)` is the working fast setter: it writes
`time` — in seconds — as the file's access and modification times
(`reference/cpp/plugins/fstat/main.cpp:340-344`,
`reference/cpp/core/environ/linux/Platform.cpp:58-65`) and returns whether the
write succeeded.

### Directory listings

`dirlist` returns the immediate child names. The directory name must end in
`/`, otherwise the member raises
`"'/' must be specified at the end of given directory name."`
(`reference/cpp/plugins/fstat/main.cpp:506-513`), and a directory that cannot
be listed raises `Directory not found.`
(`reference/cpp/plugins/fstat/main.cpp:520-522`).

`dirlistEx` returns the immediate children as dictionaries
(`reference/cpp/plugins/fstat/main.cpp:551-610`):

* `name` — the entry name, without a trailing `/` for a directory;
* `size` — the file size, `0` for anything that is not a regular file
  (`reference/cpp/plugins/fstat/main.cpp:565`);
* `attrib` — the permission bits
  (`reference/cpp/plugins/fstat/main.cpp:571`);
* `mtime`, `ctime`, `atime` — **raw epoch-second integers**, not `Date`
  objects, unlike `fstat`; the reference's `ctime` and `atime` mirror its
  `mtime` (`reference/cpp/plugins/fstat/main.cpp:577-599`).

`dirtree` walks the tree recursively and suffixes every directory name with
`/` (`reference/cpp/plugins/fstat/main.cpp:436-483`). With `dironly` set, only
the directories are listed
(`reference/cpp/plugins/fstat/main.cpp:440,473`). A directory that does not
exist yields an empty array — the reference's `_dirtree` returns quietly
(`reference/cpp/plugins/fstat/main.cpp:457-461`) — while `dirlist` and
`dirlistEx` raise `Directory not found.`.

The port does **not** lowercase the names; the reference's separate
`getDirList` global does, because it reads through `TVPGetLocalFileListAt`.
That global is owned by the [`dirlist`](dirlist.md) plugin, which delegates to
`Storages.dirlist`.

### Directories

`createDirectory` normalizes the name and requires the trailing `/`;
`createDirectoryNoNormalize` skips the normalization but keeps the trailing-`/`
rule (`reference/cpp/plugins/fstat/main.cpp:620-676`). Both create the
directory *and its parents* (`fs::create_directories`,
`reference/cpp/plugins/fstat/main.cpp:651`) and report whether something was
created: an existing directory reports "not created", as the reference's
`create_directories` does
(`reference/cpp/plugins/fstat/main.cpp:651-656`).

`removeDirectory` deletes the directory
(`reference/cpp/plugins/fstat/main.cpp:620-637`) and reports whether it was
removed. `isExistentDirectory` is `exists && is_directory`
(`reference/cpp/plugins/fstat/main.cpp:757-766`).

### Copying, moving and deleting

`copyFile(from, to)` copies the *placed* source to the *normalized*
destination (`reference/cpp/plugins/fstat/main.cpp:774-789`).
`copyFileNoNormalize(from, to, failIfExist)` copies without normalizing the
destination and resolves that destination through the local-name path, so an
in-archive destination raises `Cannot get local name`
(`reference/cpp/plugins/fstat/main.cpp:800-810`). The reference accepts
`failIfExist` and ignores it; so does the port.

`moveFile(from, to)` copies and then deletes the source. The reference deletes
the source **unconditionally** after the copy attempt, so a failed copy still
removes it (`reference/cpp/plugins/fstat/main.cpp:417-434`); the port does the
same and returns the copy result.

`deleteFile(file)` deletes an existing disk file only — the reference resolves
`TVPGetLocallyAccessibleName(TVPGetPlacedPath(file))` first
(`reference/cpp/plugins/fstat/main.cpp:382-396`) — and reports whether the
file was removed.

### Exporting and hashing

`exportFile(filename, storename)` reads the storage entry — a disk file or an
archive member — and writes it to a local file. The reference raises
`cannot open readfile: <name>` and `cannot open storefile: <name>`
(`reference/cpp/plugins/fstat/main.cpp:351-374`), and so does the port.

`getMD5HashString(filename)` hashes the storage entry's bytes and returns the
lowercase 32-character hex digest
(`reference/cpp/plugins/fstat/main.cpp:840-874`). The reference raises
`cannot open : <name>` for an entry it cannot read
(`reference/cpp/plugins/fstat/main.cpp:845-851`).

### Searching the path

`searchPath(filename, searchpath)` splits the search path on `;` (Windows) or
`:` (POSIX) and falls back to the system `PATH` when the argument is empty
(`reference/cpp/plugins/fstat/main.cpp:54-100`). It returns the first match as
a normalized absolute path, or `void` when nothing matched
(`reference/cpp/plugins/fstat/main.cpp:884-904`).

### The `currentPath` property

The getter returns the normalized game directory with a trailing `/` — the
reference's `TVPGetAppStoragePath` normalized with a separator appended
(`reference/cpp/plugins/fstat/main.cpp:909-913`). The setter forwards to
`changeDirectory` (`reference/cpp/plugins/fstat/main.cpp:915`), so a name
without the trailing `/` raises and an in-archive name raises `Cannot get
local name`; the write itself changes nothing, because `changeDirectory` is a
reference no-op.

### The reference no-ops

Six members are left unimplemented by the reference: it logs a warning and
returns a fixed value instead of doing the work.

| member | reference behaviour | reference |
|---|---|---|
| `truncateFile(file, size)` | warns, returns `true`, the file is untouched | `reference/cpp/plugins/fstat/main.cpp:405-408` |
| `changeDirectory(dir)` | validates the trailing `/` and the local name, warns, returns `true` | `reference/cpp/plugins/fstat/main.cpp:683-692` |
| `setFileAttributes(filename, attr)` | validates the local name, warns, returns `true` | `reference/cpp/plugins/fstat/main.cpp:700-705` |
| `resetFileAttributes(filename, attr)` | validates the local name, warns, returns `true` | `reference/cpp/plugins/fstat/main.cpp:713-719` |
| `getFileAttributes(filename)` | validates the local name, warns, returns `0` | `reference/cpp/plugins/fstat/main.cpp:726-733` |
| `selectDirectory(params)` | validates one argument, warns, returns `void` | `reference/cpp/plugins/fstat/main.cpp:742-749` |

The port reproduces every one of them, warning on each call; none of them is
silent. The attribute members still validate the name first — the reference
normalizes it and requires a locally accessible form
(`reference/cpp/plugins/fstat/main.cpp:700-735`) — so an in-archive name
raises `Cannot get local name` before the warning.

### `TemporaryFiles`

`entry(name)` is `true` when the name resolves to an existing regular file
that can be opened for reading; `entryFolder(name)` is `true` when the name
resolves to an existing directory
(`reference/cpp/plugins/fstat/main.cpp:952-985`). The reference validates but
records nothing — the class keeps no entry list — and neither does the port:
the native payload is a unit value. The members ignore their receiver and
work from their argument plus the attached storage.

### Related plugins

* [`dirlist`](dirlist.md) — the `getDirList` global, which delegates to
  `Storages.dirlist` and lowercases the names.
* [`var_file`](var_file.md) — registers no `Storages` members of its own; the
  file-manipulation members on the class come from this plugin.

## State and lifetime

* The plugin keeps **one** piece of state: a per-engine counter mixed into
  `getTemporaryName`, so two calls never return the same name. It lives in the
  compatibility services' per-family dispatch state, not in a process global.
* Everything else goes to the attached engine `Storage` and to the file
  system. The plugin holds no cache of its own.
* `TemporaryFiles` instances carry a unit payload; the destructor frees the
  box and nothing else — the reference's `~TemporaryFiles` has nothing to
  release either.
* The `FILE_ATTRIBUTE_*` globals are installed when the plugin registers, and
  a second link does not register it again.

## Errors

| condition | outcome |
|---|---|
| the member is called with too few arguments | typed error: `Storages.<member> requires N argument(s)`. The reference rejects a short call too: its raw callbacks check `numparams` (`reference/cpp/plugins/fstat/main.cpp:262-263,307-308,438-439,842-844,886-887`) and its `NCB_METHOD` members reject it in the ncbind invocation (`reference/cpp/core/plugin/ncbind.hpp:1178`) |
| the compat context is not bound | typed error: `Storages.<member>: compat context is not bound` |
| no storage is attached | typed error: `Storages.<member>: no storage attached` |
| the storage raises | the storage's error, prefixed with the member name |
| the name has no locally accessible form (an in-archive name) | typed error: `Cannot get local name from <name>` (`reference/cpp/core/base/StorageIntf.cpp:546-550`) |
| a directory name does not end in `/` | typed error: `'/' must be specified at the end of given directory name.` (`reference/cpp/plugins/fstat/main.cpp:509-512`) |
| a directory cannot be listed | typed error: `Storages.dirlist: Directory not found.` (`reference/cpp/plugins/fstat/main.cpp:520-522`) |
| `exportFile` cannot read the source | typed error: `Storages.exportFile: cannot open readfile: <name> (…)` (`reference/cpp/plugins/fstat/main.cpp:371-372`) |
| `exportFile` cannot write the destination | typed error: `Storages.exportFile: cannot open storefile: <name> (…)` (`reference/cpp/plugins/fstat/main.cpp:366-369`) |
| `getMD5HashString` cannot read the entry | typed error: `Storages.getMD5HashString: cannot open : <name> (…)` (`reference/cpp/plugins/fstat/main.cpp:845-851`) |
| a file-system operation fails (delete, move, copy, create, remove, set-time) | the member reports `false` and the failure is logged |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/fstat.rs` | the whole plugin surface: the 30 registered `Storages` members, the `FILE_ATTRIBUTE_*` globals, the `fstat`/`getTime`/`setTime` dictionaries and their `Date` values, the archive branch, the copy / move / delete / export members, the directory helpers, `getMD5HashString`, `getDisplayName`, `searchPath`, `getTemporaryName`, the reference no-ops, the arity errors, the missing-storage errors and `TemporaryFiles` |
| `crates/compat/src/natives/plugins/fstat/mod.rs` (unit tests) | this implementation in isolation from the sibling plugins that register later: the `Date`-valued timestamps, the `getTime`/`setTime` round trip, the `TemporaryFiles` validators, the class-named constructor a script subclass reaches through `super.TemporaryFiles()`, and the directory helpers |
| `crates/compat/tests/dispatch_fmt.rs` | that the `TemporaryFiles` members are receiver-independent: the argument, not the receiver, selects the path |
| `crates/compat/tests/storages_io.rs` | that the other plugin surfaces do not shadow the `Storages` members this plugin owns |

## Known gaps

* **`clearStorageCaches` is a no-op.** The reference clears the storage
  caches (`TVPClearStorageCaches`,
  `reference/cpp/plugins/fstat/main.cpp:252`); the engine's namespace caches a
  resolved hit, and this member does not clear it. The call is documented, not
  silent, but a script that relies on it to see a changed archive index does
  not get that.
* **`Date` carries the reference's unit mix-up.** `Date.getTime()` returns the
  file time in seconds, but the `Date` displays as epoch + filetime/1000,
  because the reference hands `Date.setTime` the raw seconds and `Date.setTime`
  divides by 1000
  (`reference/cpp/plugins/fstat/main.cpp:112-133`,
  `reference/cpp/core/tjs2/tjsDate.cpp:209-219,300-306`). The port reproduces
  the reference exactly — a script that reads the year out of the `Date` sees
  a value 1000× too small, on the reference and here alike.
* **Six members are unimplemented in the reference and stay unimplemented
  here.** `truncateFile`, `changeDirectory`, `setFileAttributes`,
  `resetFileAttributes`, `getFileAttributes` and `selectDirectory` log a
  warning and return a fixed value in the reference
  (`reference/cpp/plugins/fstat/main.cpp:405-408,683-749`); the port does the
  same, so no file is truncated, no attribute is set, no working directory is
  changed and no folder chooser opens.
* **`getLastModifiedFileTime` always answers `0`.** The reference's stat
  check is inverted (`TVP_stat` returns true on success,
  `reference/cpp/core/environ/linux/Platform.cpp:252-264`), so it reports `0`
  for every existing file (`reference/cpp/plugins/fstat/main.cpp:331-338`).
  The port pins the shipped behaviour.
* **The listings are sorted by name.** The port lists the entries of a
  directory in name order; the reference yields the raw directory order of
  `std::filesystem::directory_iterator`
  (`reference/cpp/plugins/fstat/main.cpp:463,525`).
* **`getTemporaryName` mixes its own entropy.** The reference builds the name
  from 6 random bytes (`TVPGetRandomBits128`,
  `reference/cpp/core/base/impl/StorageImpl.cpp:468-481`); the port mixes the
  clock, its per-engine counter and the process id. The name *shape* —
  `krkr_<12 hex>_<counter>_<pid>` — is the same.
* **`dirtree` normalizes its argument's trailing slashes.** The port trims
  them and appends one; the reference appends a `/` to the argument as given
  (`reference/cpp/plugins/fstat/main.cpp:442`).
