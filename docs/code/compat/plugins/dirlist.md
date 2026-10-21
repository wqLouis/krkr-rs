# dirlist (`dirlist.dll`)

**DLL name:** `dirlist.dll` (aliases: none) — linked with `Plugins.link("dirlist.dll")`
**Rust module:** `crates/compat/src/natives/plugins/dirlist/`
**Reference:** `reference/cpp/plugins/dirlist.cpp`
**Registers:** one **global** function — `getDirList(dir)`

The function appears when the script links `dirlist.dll`: a ported plugin
installs its members on the first `Plugins.link` that names it, not at engine
startup. See the [plugin index](../plugins.md) for the linking model.

## Surface

| member | kind | behaviour |
|---|---|---|
| `getDirList(dir)` | global function | the immediate child names of `dir`, lowercased |

The plugin registers **no** `Storages` members. The `Storages.dirlist` /
`dirlistEx` / `dirtree` / `createDirectory` family comes from the `fstat`
plugin and is owned by [`fstat`](fstat.md).

## Behaviour

### The listing

The reference's `tGetDirListFunction` (`reference/cpp/plugins/dirlist.cpp:13-57`)
creates an `Array` (`reference/cpp/plugins/dirlist.cpp:33,46`) and fills it
from `TVPGetLocalFileListAt` (`reference/cpp/plugins/dirlist.cpp:41-45`), keeping
every entry whose mode is a regular file **or** a directory. The names come
back lowercased: `TVPGetLocalFileListAt` lowercases each entry before the
callback sees it
(`reference/cpp/core/base/impl/StorageImpl.cpp:232-238`). A directory is
listed without a trailing `/`.

The port installs the function as a small TJS wrapper — the VM ABI has no
native global-function registration seam — that delegates to the `fstat`-owned
`Storages.dirlist` member and lowercases every entry afterwards, because the
delegated member does not lowercase
(`reference/cpp/plugins/fstat/main.cpp:526`).

### The trailing slash

The directory name must end in `/`. The reference raises `"'/' must be
specified at the end of given directory name."`
(`reference/cpp/plugins/dirlist.cpp:25-27`), and so does the delegated member
(`reference/cpp/plugins/fstat/main.cpp:509-512`), so the port raises it too.

### A directory that cannot be listed

`TVPGetLocalFileListAt` does nothing when `opendir` fails
(`reference/cpp/core/base/impl/StorageImpl.cpp:222-249`), so the reference's
listing for a missing directory is empty. The delegated `Storages.dirlist`
raises `"Directory not found."` instead
(`reference/cpp/plugins/fstat/main.cpp:520-522`); the wrapper catches exactly
that message and returns an empty array, and rethrows every other error
(`reference/cpp/plugins/dirlist.cpp:44-51`).

### The global is an ordinary member

`getDirList` is an ordinary member on the global object, installed with
`PropSet(TJS_MEMBERENSURE, "getDirList", ...)`
(`reference/cpp/plugins/dirlist.cpp:80-87`), so a script may reassign it and
the new binding wins.

## State and lifetime

The function holds no state. Every call goes to the attached storage through
the delegated member.

## Errors

| condition | outcome |
|---|---|
| no argument | typed error: `Storages.dirlist: '/' must be specified at the end of given directory name.` — the missing argument becomes an empty name |
| the directory name does not end in `/` | the same trailing-slash error (`reference/cpp/plugins/dirlist.cpp:25-27`) |
| the name has no locally accessible form (an absolute POSIX path) | typed error: `Storages.dirlist: Cannot get local name from <name>` (`reference/cpp/core/base/StorageIntf.cpp:546-550`, `reference/cpp/core/base/MsgIntfInc.h:35`) |
| the directory does not exist | an **empty array**, not an error |
| the delegated member raises any other error | rethrown unchanged |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/misc_plugins.rs` | that the global exists, is callable, returns the immediate child names (files and directories) in an `Array`, lowercases mixed-case names, enforces the trailing slash, returns an empty array for a missing directory, and is an ordinary reassignable global |
| `crates/compat/tests/storages_io.rs` | that the global is driven end to end against a mounted storage, and that `Storages.getDirList` is **not** registered |

## Known gaps

* The wrapper delegates to `Storages.dirlist`, which the `fstat` plugin
  registers. A script that links `dirlist.dll` but not `fstat.dll` and then
  calls `getDirList` gets the VM's missing-member error (`Member "dirlist"
  does not exist`). The reference's `getDirList` is self-contained and works
  on its own.
* The reference's `getDirList` and `Storages.dirlist` are two *independent*
  C++ implementations (`reference/cpp/plugins/dirlist.cpp:13-57` and
  `reference/cpp/plugins/fstat/main.cpp:506-551`), so a script that replaces
  one name does not affect the other. The port routes both through the single
  native member, so reassigning `Storages.dirlist` changes what `getDirList`
  delegates to.
* The reference's `getDirList` filters the listing to `S_IFREG | S_IFDIR`
  (`reference/cpp/plugins/dirlist.cpp:42`); the `fstat` member keeps every
  entry and exposes no file-type field, so the port keeps every entry.
* The reference raises `TJS_E_BADPARAMCOUNT` for a call with no argument
  (`reference/cpp/plugins/dirlist.cpp:20-21`); the port raises the
  trailing-slash error instead. Both are typed, catchable exceptions.
