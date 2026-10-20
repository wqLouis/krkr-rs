# dirlist (`dirlist.dll`)

**DLL name:** `dirlist.dll` — linked with `Plugins.link("dirlist.dll")`
**Rust module:** `crates/compat/src/natives/plugins/dirlist/`
**Reference:** `reference/cpp/plugins/dirlist.cpp`
**Registers:** one **global** function — `getDirList(dir)`

## Surface

| member | kind | behaviour |
|---|---|---|
| `getDirList(dir)` | global function | the immediate child names of `dir`, lowercased |

The plugin registers **no** `Storages` members. The `Storages.dirlist` /
`dirlistEx` / `dirtree` / `createDirectory` family comes from the `fstat`
plugin and is owned by [`fstat`](fstat.md).

## Behaviour

`getDirList(dir)` returns an array with the names of the immediate children of
a directory.

The reference implementation (`reference/cpp/plugins/dirlist.cpp:13-57`) lists
the directory through `TVPGetLocalFileListAt`
(`reference/cpp/core/base/impl/StorageImpl.cpp:222-247`), which lowercases
every entry before the callback sees it
(`reference/cpp/core/base/impl/StorageImpl.cpp:234-241`). The port reproduces
that: the names come back lowercased.

A directory that cannot be listed yields an **empty array**, not an error.
`TVPGetLocalFileListAt` does nothing when `opendir` fails
(`reference/cpp/core/base/impl/StorageImpl.cpp:222-247`), and the reference
only rethrows exceptions other than the "not found" one
(`reference/cpp/plugins/dirlist.cpp:48-50`).

The directory name must end in `/`; the reference raises
`"'/' must be specified at the end of given directory name."`
(`reference/cpp/plugins/dirlist.cpp:25-27`) otherwise, and the port does too
through the delegated member.

`getDirList` is an ordinary member on the global object, installed with
`PropSet(TJS_MEMBERENSURE, "getDirList", ...)`
(`reference/cpp/plugins/dirlist.cpp:80-87`), so a script may reassign it and
the new binding wins.

The port implements the function as a small TJS wrapper (the VM ABI has no
native global-function registration seam). The wrapper delegates to the
`fstat`-owned `Storages.dirlist` member and lowercases every entry.

## State and lifetime

The function holds no state. Every call goes to the attached storage and the
`fstat` member.

## Errors

| condition | outcome |
|---|---|
| no argument | typed error: the delegated member requires one argument |
| the directory name does not end in `/` | typed error: `'/' must be specified at the end of given directory name.` (`reference/cpp/plugins/dirlist.cpp:25-27`, `reference/cpp/plugins/fstat/main.cpp:509-512`) |
| the directory has no local accessible name (an absolute path) | typed error: `Cannot get local name from %1` (`reference/cpp/core/base/impl/StorageImpl.cpp:301-413`, `reference/cpp/core/base/MsgIntfInc.h:35`) |
| the directory does not exist | an **empty array** — `TVPGetLocalFileListAt` does nothing when `opendir` fails (`reference/cpp/core/base/impl/StorageImpl.cpp:222-247`) and the reference rethrows only *other* exceptions (`reference/cpp/plugins/dirlist.cpp:44-51`), so the wrapper catches exactly the "Directory not found." error and returns `[]` |
| the delegated `Storages.dirlist` raises any other error | the error is rethrown unchanged |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/misc_plugins.rs` | the global function exists, is callable and returns the lowercased child names |
| `crates/compat/tests/storages_io.rs` | the global function is driven end to end against a real mounted storage, and that `Storages.getDirList` is **not** registered |

## Known gaps

* The reference's `getDirList` and `Storages.dirlist` are two independent
  C++ implementations (`reference/cpp/plugins/dirlist.cpp:13-57` and
  `reference/cpp/plugins/fstat/main.cpp:506-551`), so a script that replaces
  one name does not affect the other. The port routes both through the single
  native member, so reassigning `Storages.dirlist` changes what
  `getDirList` delegates to.
* The reference's `getDirList` filters the listing to `S_IFREG | S_IFDIR`
  (`reference/cpp/plugins/dirlist.cpp:42`); the `fstat` member keeps every
  entry and exposes no file-type field, so the port keeps every entry.
