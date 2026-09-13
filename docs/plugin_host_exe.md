# Design study: a dedicated Windows "plugin host" `.exe` under Wine, bridged to native krkr-rs

Scope: **DEV tooling only.** This document designs and evaluates the user's idea:

> Write our own Windows `.exe` whose sole job is to `LoadLibrary` the game's
> plugin DLLs, call `V2Link`, and invoke them under Wine (native x86_64, or
> Wine+box64 on ARM), then bridge the results back to the native Rust engine.

It is a design/feasibility study, not an implementation. **No packages or
toolchains are installed by this study.** The one executable artefact below was
*compiled* with the already-present `zig` (cross-compile only); it was **not run**
because `wine` is absent on this machine.

Repo: `/mnt/DATA/Document/code/krkr-rs`
Game: `/mnt/DATA/Games/Others/test`
Reference engine in tree: `reference/` (the KrKr2 Emulator fork; see
`docs/wine_box64.md` for the earlier Wine study, whose conclusion this study
sharpens and partially revises).

---

## 0. Verdict (short)

The idea is **worth it for exactly one plugin in this package — `wuvorbis` — and
even there only as a fallback**, because `wuvorbis.dll` is the only one of the
nine that exports a real C decoder API. For the other eight, "call the plugin"
means "run the entire TVP/TJS core inside the host exe", at which point the host
exe has *become* the engine (option B1 in `docs/wine_box64.md`). Per-call IPC
forwarding of the host API is not just slow, it is **semantically impossible**:
plugins exchange raw `iTJSDispatch2*`, `ttstr` and callback pointers with the
host.

| Plugin | Kind | Exports | Host funcs | Self-contained? | Host-exe effort | Recommended? |
|---|---|---|---:|---:|---|---:|---|
| `wuvorbis.dll` | Ogg Vorbis decoder | 40 (full `wu_ov_*` C API) | 5 | **yes (C API)** | **S** | **Yes, as last resort** |
| `extrans.dll` | pixel transition providers | 2 | 11 | partial (provider I/F) | M | No |
| `csvParser.dll` | CSV → TJS array | 2 | 13 | no (returns TJS objects) | L | No |
| `fstat.dll` | filesystem / `Storages` | 4 | 21 | no (storage + TJS) | L | No |
| `KAGParserEx.dll` | KAG scenario parser | 4 | 23 | no (TJS + events) | XL | No |
| `layerExDraw.dll` | GDI+ layer drawing | 4 | 17 | no (`Layer` graph + TJS) | L/XL | No |
| `menu.dll` | Win32 menus / accelerators | 2 | 20 | no (User32 + TJS events) | XL | No |
| `win32dialog.dll` | Win32 dialogs | 4 | 18 | no (User32/GDI + TJS) | XL | No |
| `windowEx.dll` | Win32 window extension | 4 | 22 | no (User32/GDI + TJS) | XL | No |

**Recommended scope:** *self-contained C-API decoders only*. For this game even
that is redundant (`tvp-sound` already decodes Ogg/Vorbis natively), so the
practical recommendation is: **do not build the host exe for this title; add it
as an optional sidecar only if a future title ships a Windows-only, closed-source
codec/DRM plugin that exposes a plain C ABI.** Everything else stays a native
Rust reimplementation.

---

## 1. Method and reproduction

All claims are reproducible from the extracted DLLs; commands are in
§12. The archive is a Kirikiroid2 Android package (`docs/wine_box64.md` §2); the
nine `.dll` members inside `data.xp3` are the game's **original 32-bit Windows
plugins**, carried along by the platform-agnostic archive and never executed by
the Android build.

```text
$ file /tmp/dlls/*
csvParser.dll:   PE32 executable for MS Windows 4.00 (DLL), Intel i386, 5 sections
extrans.dll:     PE32 executable for MS Windows 4.00 (DLL), Intel i386, 5 sections
fstat.dll:       PE32 executable for MS Windows 4.00 (DLL), Intel i386, 5 sections
kagparserex.dll: PE32 executable for MS Windows 6.00 (DLL), Intel i386, 5 sections
layerexdraw.dll: PE32 executable for MS Windows 4.00 (DLL), Intel i386, 5 sections
menu.dll:        PE32 executable for MS Windows 5.01 (DLL), Intel i386, 5 sections
win32dialog.dll: PE32 executable for MS Windows 5.00 (DLL), Intel i386, 5 sections
windowex.dll:    PE32 executable for MS Windows 5.01 (DLL), Intel i386, 5 sections
wuvorbis.dll:    PE32 executable for MS Windows 4.00 (DLL), Intel i386, 7 sections
```

The DLLs are extracted from the XP3 index; `extract_xp3.py`'s `open_index` /
`get_file` are reused as a library (the script only supports one file at a time,
so a short loop is used — see §12).

### 1.1 Environment (verified, unchanged)

```text
$ uname -m
x86_64
$ which wine wine64            # no host-arch question here: this box is x86_64
wine        (absent)
wine64      (absent)
$ rustup target list --installed
aarch64-linux-android
armv7-linux-androideabi
i686-linux-android
wasm32-unknown-unknown
wasm32-wasip2
x86_64-linux-android
x86_64-unknown-linux-gnu
$ command -v zig cmake ninja bison
zig   /usr/bin/zig        # 0.16.0
cmake /usr/bin/cmake
ninja /usr/bin/ninja
bison /usr/bin/bison
```

* `wine`/`wine64` are **not installed**; no `i686-pc-windows-gnu` Rust target.
* **`zig 0.16.0` is installed and can cross-compile to 32-bit Windows.** This is
  the important new environment fact versus `docs/wine_box64.md`; it means a
  C/C++ host exe *can be built here* even though no mingw-w64 is installed:

```text
$ zig cc -target x86-windows-gnu -c ztest.c -o ztest.o
$ objdump -f ztest.o
file format pe-i386
architecture: i386
```

  (`x86-windows-gnu` is zig's triple for 32-bit x86 Windows; `i686-windows-gnu`
  is rejected.) Zig links full PE32 exes/DLLs too (§5.5).

---

## 2. Per-plugin evidence

### 2.1 Exports

The export tables are decisive. Eight of the nine expose **only**
`V2Link`/`V2Unlink` (some also their stdcall decorations `_V2Link@4` /
`_V2Unlink@0`). Only `wuvorbis.dll` exposes anything else.

| DLL | Export count | Exported names |
|---|---:|---|
| `csvParser.dll` | 2 | `V2Link`, `V2Unlink` |
| `extrans.dll` | 2 | `V2Link`, `V2Unlink` |
| `fstat.dll` | 4 | `V2Link`, `V2Unlink`, `_V2Link@4`, `_V2Unlink@0` |
| `KAGParserEx.dll` | 4 | `V2Link`, `V2Unlink`, `_V2Link@4`, `_V2Unlink@0` |
| `layerExDraw.dll` | 4 | `V2Link`, `V2Unlink`, `_V2Link@4`, `_V2Unlink@0` |
| `menu.dll` | 2 | `V2Link`, `V2Unlink` |
| `win32dialog.dll` | 4 | `V2Link`, `V2Unlink`, `_V2Link@4`, `_V2Unlink@0` |
| `windowEx.dll` | 4 | `V2Link`, `V2Unlink`, `_V2Link@4`, `_V2Unlink@0` |
| `wuvorbis.dll` | **40** | `V2Link`, `V2Unlink`, `GetModuleInstance`, `GetOptionDesc`, `Query_sizeof_OggVorbis_File`, `wu_DetectCPU`, `wu_SetCPUType`, `wu_ScaleOutput`, and the full `wu_ov_*` libvorbisfile API (`wu_ov_open_callbacks`, `wu_ov_read`, `wu_ov_read_float`, `wu_ov_info`, `wu_ov_clear`, `wu_ov_pcm_seek`, `wu_ov_time_total`, … 35 `wu_*` symbols total) |

`objdump -p wuvorbis.dll`:

```text
[Ordinal/Name Pointer] Table -- Ordinal Base 1
	          Ordinal   Hint Name
	[   0] +base[   1]  0000 GetModuleInstance
	[  36] +base[  37]  0001 GetOptionDesc
	[  37] +base[  38]  0002 Query_sizeof_OggVorbis_File
	[  38] +base[  39]  0003 V2Link
	[  39] +base[  40]  0004 V2Unlink
	[   1] +base[   2]  0005 wu_DetectCPU
	...
	[  12] +base[  13]  0010 wu_ov_open_callbacks
	[  18] +base[  19]  0016 wu_ov_pcm_total
	[  23] +base[  24]  001b wu_ov_read
	[  24] +base[  25]  001c wu_ov_read_float
	...
```

Consequence: **there is no exported "do the work" function for the other eight.**
The only entry point to their functionality is `V2Link`, i.e. the TVP plugin ABI.

### 2.2 Imports (native Win32 only — no TVP import library)

Every DLL imports only OS DLLs, never a `TVP*.dll`:

| DLL | Imported DLLs |
|---|---|
| `csvParser.dll` | `KERNEL32.dll` |
| `extrans.dll` | `KERNEL32.dll` |
| `fstat.dll` | `KERNEL32.dll`, `USER32.dll`, `SHELL32.dll` |
| `KAGParserEx.dll` | `KERNEL32.dll` |
| `layerExDraw.dll` | `KERNEL32.dll`, `GDI32.dll`, `ole32.dll`, **`gdiplus.dll`** |
| `menu.dll` | `KERNEL32.dll`, `USER32.dll` |
| `win32dialog.dll` | `KERNEL32.dll`, `USER32.dll`, `GDI32.dll` |
| `windowEx.dll` | `KERNEL32.dll`, `USER32.dll`, `GDI32.dll`, `SHELL32.dll` |
| `wuvorbis.dll` | `KERNEL32.dll` |

This is the core architectural fact: **all TVP/TJS calls are resolved at runtime
by name through `iTVPFunctionExporter`.** The plugins do not (and cannot) link
against the engine. The ABI declaration is the legacy one, still present but
compiled out in this fork:

`reference/cpp/core/plugin/PluginImpl.h:21` `#if 0`, `:28` `struct
iTVPFunctionExporter`, `:30` `QueryFunctions`, `:32`
`QueryFunctionsByNarrowString`, `:48` `TVPGetFunctionExporter`, `:51-52` the
`tTVPV2LinkProc`/`tTVPV2UnlinkProc` typedefs, `:66` `#endif`. (`:55` also records
the TSS-module typedef `tTVPGetModuleInstanceProc`, which is how `wuvorbis`'s
`GetModuleInstance` is consumed.)

### 2.3 Host functions requested (the exporter query surface)

Two measurement methods differ, and the difference matters:

* A **loose** `strings | grep -oE '::[A-Za-z_]\w*\(' | sort -u` yields the
  "**89 distinct**" figure quoted in `docs/wine_box64.md`. That set includes C++
  member functions/types (`tTJSString::c_str`, `tTJSVariant::AsObject`,
  `tTJSString::operator+`, …), because the DLLs statically carry RTTI/type
  strings.
* The actual ncbind query keys use the **global-scope signature form**
  `<return> ::<name>(<args>)` (leading space before `::`). Extracting
  `strings | grep -oE ' ::[A-Za-z_]\w*'` gives the true host API surface:
  **59 distinct host functions** across the nine plugins (60 strings, minus the
  false positive `operator` from `tTJSString ::operator +(...)`).

```text
$ for d in *.dll; do strings -n3 "$d" | grep -oE ' ::[A-Za-z_][A-Za-z0-9_]*' | sed 's/^ :://'; done \
  | sort -u | grep -v '^operator$' | wc -l
59
$ # names NOT preceded by a space are C++ members, e.g.:
$ comm -23 all_qualified.txt space_qualified.txt | tr '\n' ' ' | head -c 300
AllocBuffer AsInteger AsObject AsObjectNoAddRef AsStringNoAddRef ... c_str ... tTJSString tTJSVariant Type
```

Per-plugin count and full list:

| DLL | Host funcs | Names |
|---|---:|---|
| `wuvorbis.dll` | **5** | `TVPAddImportantLog`, `TVPAddLog`, `TVPGetCommandLine`, `TVPGetCPUType`, `TVPThrowPluginUnboundFunctionError` |
| `extrans.dll` | 11 | `TVPAddTransHandlerProvider`, `TVPConstAlphaBlend_SD`, `…_SD_a`, `…_SD_d`, `TVPFillARGB`, `TVPGetCPUType`, `TVPLinTransCopy`, `TVPRemoveTransHandlerProvider`, `TVPStretchCopy`, `TVPThrowExceptionMessage`, `TVPThrowPluginUnboundFunctionError` |
| `csvParser.dll` | 13 | `TJSCreateArrayObject`, `TJSCreateNativeClassConstructor`, `TJSCreateNativeClassForPlugin`, `TJSCreateNativeClassMethod`, `TJSCreateNativeClassProperty`, `TJSNativeClassRegisterNCM`, `TJSNativeClassSetClassID`, `TJSRegisterNativeClass`, `TVPCreateIStream`, `TVPExecuteExpression`, `TVPGetScriptDispatch`, `TVPThrowExceptionMessage`, `TVPThrowPluginUnboundFunctionError` |
| `fstat.dll` | 21 | `TJSCreateArrayObject`, `TJSCreateDictionaryObject`, `TJSCreateNativeClassMethod`, `TJSDoVariantOperation`, `TJS_int_to_str`, `TJSRegisterNativeClass`, `TJS_stricmp`, `TVPAddLog`, `TVPCreateIStream`, `TVPExecuteExpression`, `TVPGetApplicationWindowHandle`, `TVPGetLocalName`, `TVPGetPlacedPath`, `TVPGetScriptDispatch`, `TVPIsExistentStorageNoSearchNoNormalize`, `TVP_md5_append`, `TVP_md5_finish`, `TVP_md5_init`, `TVPNormalizeStorageName`, `TVPThrowExceptionMessage`, `TVPThrowPluginUnboundFunctionError` |
| `KAGParserEx.dll` | 23 | `TJSCreateArrayObject`, `TJSCreateDictionaryObject`, `TJSCreateNativeClassConstructor`, `TJSCreateNativeClassForPlugin`, `TJSCreateNativeClassMethod`, `TJSCreateNativeClassProperty`, `TJSGetArrayElementCount`, `TJSGetMessageMapMessage`, `TJSMapGlobalStringMap`, `TJSNativeClassRegisterNCM`, `TJSNativeClassSetClassID`, `TJSRegisterNativeClass`, `TJS_strcpy`, `TJS_strlen`, `TJSThrowNullAccess`, `TVPAddCompactEventHook`, `TVPAddLog`, `TVPCreateTextStreamForRead`, `TVPExecuteExpression`, `TVPExtractStorageName`, `TVPGetScriptDispatch`, `TVPThrowExceptionMessage`, `TVPThrowPluginUnboundFunctionError` |
| `layerExDraw.dll` | 17 | `TJSCreateArrayObject`, `TJSCreateNativeClassForPlugin`, `TJSCreateNativeClassMethod`, `TJSDoVariantOperation`, `TJS_int_to_str`, `TJSNativeClassRegisterNCM`, `TJSNativeClassSetClassID`, `TJSRegisterNativeClass`, `TJS_stricmp`, `TVPAddLog`, `TVPCreateIStream`, `TVPExecuteExpression`, `TVPGetLocalName`, `TVPGetPlacedPath`, `TVPGetScriptDispatch`, `TVPThrowExceptionMessage`, `TVPThrowPluginUnboundFunctionError` |
| `menu.dll` | 20 | `TJSCreateArrayObject`, `TJSCreateDictionaryObject`, `TJSCreateNativeClassConstructor`, `TJSCreateNativeClassForPlugin`, `TJSCreateNativeClassMethod`, `TJSCreateNativeClassProperty`, `TJSDoVariantOperation`, `TJS_int_to_str`, `TJSNativeClassRegisterNCM`, `TJSNativeClassSetClassID`, `TJSRegisterNativeClass`, `TJS_strnicmp`, `TVPCancelSourceEvents`, `TVPCreateEventObject`, `TVPDeleteAcceleratorKeyTable`, `TVPGetScriptDispatch`, `TVPPostEvent`, `TVPRegisterAcceleratorKey`, `TVPThrowExceptionMessage`, `TVPThrowPluginUnboundFunctionError` |
| `win32dialog.dll` | 18 | `TJSCreateDictionaryObject`, `TJSCreateNativeClassForPlugin`, `TJSCreateNativeClassMethod`, `TJSDoVariantOperation`, `TJS_int_to_str`, `TJSNativeClassRegisterNCM`, `TJSNativeClassSetClassID`, `TJSRegisterNativeClass`, `TJS_stricmp`, `TVPAddLog`, `TVPBreathe`, `TVPDoTryBlock`, `TVP_free`, `TVPGetApplicationWindowHandle`, `TVPGetScriptDispatch`, `TVP_malloc`, `TVPThrowExceptionMessage`, `TVPThrowPluginUnboundFunctionError` |
| `windowEx.dll` | 22 | `TJSCreateArrayObject`, `TJSCreateDictionaryObject`, `TJSCreateNativeClassMethod`, `TJSDoVariantOperation`, `TJS_int_to_str`, `TJSRegisterNativeClass`, `TJS_stricmp`, `TVPAddImportantLog`, `TVPAddLog`, `TVPBreathe`, `TVPClearGraphicCache`, `TVPDoTryBlock`, `TVPExecuteExpression`, `TVPGetAboutString`, `TVPGetApplicationWindowHandle`, `TVPGetBreathing`, `TVPGetCPUType`, `TVPGetLocalName`, `TVPGetPlacedPath`, `TVPGetScriptDispatch`, `TVPThrowExceptionMessage`, `TVPThrowPluginUnboundFunctionError` |

Sorted into three buckets:

| Bucket | Functions | Reimplementable in a thin host? |
|---|---|---|
| **Pure/util** (22) | `TVPAddLog`, `TVPAddImportantLog`, `TVPGetCPUType`, `TVPGetCommandLine`, `TVPThrowExceptionMessage`, `TVPThrowPluginUnboundFunctionError`, `TVP_malloc`, `TVP_free`, `TJS_int_to_str`, `TJS_strcpy`, `TJS_strlen`, `TJS_stricmp`, `TJS_strnicmp`, `TVPFillARGB`, `TVPConstAlphaBlend_SD{,_a,_d}`, `TVPLinTransCopy`, `TVPStretchCopy`, `TVP_md5_{init,append,finish}` | **yes** — self-contained or trivial logging |
| **TJS object model** (~16) | `TJSCreateArrayObject`, `TJSCreateDictionaryObject`, `TJSGetArrayElementCount`, `TJSDoVariantOperation`, `TJSGetMessageMapMessage`, `TJSMapGlobalStringMap`, `TJSThrowNullAccess`, `TJSCreateNativeClass{ForPlugin,Constructor,Method,Property}`, `TJSNativeClassRegisterNCM`, `TJSNativeClassSetClassID`, `TJSRegisterNativeClass`, `TVPGetScriptDispatch`, `TVPExecuteExpression` | **no** — needs a real TJS2 VM |
| **Engine/storage/windowing/events** (21) | `TVPCreateIStream`, `TVPCreateTextStreamForRead`, `TVPNormalizeStorageName`, `TVPGetLocalName`, `TVPGetPlacedPath`, `TVPIsExistentStorageNoSearchNoNormalize`, `TVPExtractStorageName`, `TVPGetApplicationWindowHandle`, `TVPAddTransHandlerProvider`, `TVPRemoveTransHandlerProvider`, `TVPAddCompactEventHook`, `TVPCreateEventObject`, `TVPCancelSourceEvents`, `TVPPostEvent`, `TVPRegisterAcceleratorKey`, `TVPDeleteAcceleratorKeyTable`, `TVPClearGraphicCache`, `TVPGetAboutString`, `TVPBreathe`, `TVPGetBreathing`, `TVPDoTryBlock` | **no** — needs engine state / Win32 message pump |

The **TJS bucket plus the engine bucket (≈38 of 59)** is what makes a generic
host exe degenerate into an engine.

### 2.4 Registered TJS classes / methods (from the DLLs' wide strings)

The plugins register their surface as TJS classes with wide-string names; these
appear verbatim in the PE `.rdata` (extracted with a UTF-16LE identifier scan):

| DLL | Registered class(es) / indicative members |
|---|---|
| `csvParser.dll` | `CSVParser` — `init`, `initStorage`, `parse`, `parseStorage`, `getNextLine`, `doLine`, `currentLineNumber`, `clear`, `finalize` |
| `extrans.dll` | `extrans` transition options — `mosaic`, `ripple`, `rotatezoom`, `rotatevanish`, `rotateswap`, `wave`, `maxdrift`, `maxomega`, `roundness`, `bgcolor{,1,2}`, `center{x,y}`, `factor`, `speed`, `twist{,accel}` |
| `fstat.dll` | `StoragesFstat` (attached to `Storages`) + `TemporaryFiles` — `copyFile`, `createDirectory`, `deleteFile`, `dirlist`, `exportFile`, `getDisplayName`, `getLastModifiedFileTime`, `isExistentDirectory`, `moveFile`, `removeDirectory`, `setFileAttributes`, `…NoNormalize`, plus `Date`/`atime`/`mtime`/`ctime` |
| `KAGParserEx.dll` | `KAGParser` — `assign`, `call`, `callLabel`, `clear`, `clearCallStack`, `getNextTag`, `goToLabel`, `jump`, `loadScenario`, `macro`/`macros`, `callStack`, `curLine`, `curLabel`, … |
| `layerExDraw.dll` | `Layer` extension + `GdiPlus`, `Matrix`, `Image`, `Font`, `PointF`, `RectF`, `Appearance` |
| `menu.dll` | `Menu`, `MenuItem`, `Window` menu surface — `insert`, `remove`, `popup`, `onClick`, `shortcut`, `checked`, `radio`, `keycodeToText`, `textToKeycode` |
| `win32dialog.dll` | `WIN32Dialog` and common controls — `BUTTON`, `EDIT`, `LISTBOX`, `COMBOBOX`, `LISTVIEW`, `TREEVIEW`, `TABCONTROL`, `STATUSBAR`, … |
| `windowEx.dll` | `Window` extension + `System`/`MenuItem`/`Debug.console` attachments — `restoreMaximize`, `maximize`, `getSystemMetrics`, `setApplicationIcon`, … |
| `wuvorbis.dll` | `OggVorbis` / `wuvorbis` metadata class + TSS decoder module (`GetModuleInstance`, `Query_sizeof_OggVorbis_File`) |

Reference sources corroborate the mechanics, e.g. `csvParser.cpp`'s `V2Link`
path grabs the global TJS object and installs `CSVParser` by hand:
`reference/cpp/plugins/csvParser.cpp:432` `TVPGetScriptDispatch()`, `:439`
`TVPExecuteExpression(TJS_W("Array"), …)`, `:445`
`addMember(global, TJS_W("CSVParser"), Create_NC_CSVParser())`. `fstat`'s method
list is at `reference/cpp/plugins/fstat/main.cpp:918-944`; `layerExDraw`'s
`GdiPlus` class at `reference/cpp/plugins/layerex_draw/windows/main.cpp:607`;
`windowEx`'s `System` attachments at `reference/cpp/plugins/windowEx.cpp:1513-1527`.
The registered names in the shipped DLLs match the reference's class/method
surface, confirming the DLLs were built from this plugin family against the
original (non-`#if 0`) exporter.

`wuvorbis`'s decoder is the same libvorbisfile API the in-tree engine uses
directly: `reference/cpp/core/sound/VorbisWaveDecoder.cpp:32` `OggVorbis_File
InputFile;`, `:217` `ov_callbacks callbacks = { read_func, seek_func, close_func,
tell_func };`, `:221` `ov_open_callbacks(this, &InputFile, nullptr, 0, callbacks)`.
The DLL just names these `wu_ov_*`.

---

## 3. Classification: only `wuvorbis` is genuinely self-contained

The naive "input bytes → output bytes" test, applied rigorously:

* **`wuvorbis` — YES.** It exports the whole decoder as C functions, so a host can
  call `wu_ov_open_callbacks` → `wu_ov_read` → `wu_ov_clear` with a plain
  `ov_callbacks` struct of C function pointers. `V2Link` is not required. The
  only exporter keys it uses are 5 logging/util functions; they are needed only
  if one insists on running `V2Link`.
* **`extrans` — PARTIAL.** Its pixel math is pure, but its output is registered
  as `iTVPTransHandlerProvider` objects via `TVPAddTransHandlerProvider`, and the
  engine drives `StartProcess`/`Process`/`EndProcess` with **layer objects**. It
  calls *back* into the host for the blend primitives
  (`TVPConstAlphaBlend_SD*`, `TVPLinTransCopy`, `TVPStretchCopy`,
  `TVPFillARGB`). A host must therefore supply the provider registry + layer
  ABI, not just a byte buffer. (And krkr-rs already implements these transitions
  natively — `crates/tvp-natives/src/extrans.rs`.)
* **`csvParser` — NO.** It returns TJS arrays via `TJSCreateArrayObject` and
  installs a `CSVParser` class through `TVPGetScriptDispatch` /
  `TVPExecuteExpression`. You cannot get its parsed rows without a TJS runtime.
  (Already native: `crates/tvp-storages/src/csv_parser.rs`.)
* **`fstat` — NO.** Storage path normalization, streams, MD5, `Date` objects,
  Win32 file APIs, and `Storages` member attachment. (Already native:
  `crates/tvp-storages/src/lib.rs`.)
* **`KAGParserEx` — NO.** Builds TJS arrays/dicts, raises TJS exceptions, adds
  compact event hooks, reads text streams. 23 host functions, half in the TJS
  bucket. (Already native: `crates/tvp-kagparser`.)
* **`layerExDraw` — NO.** GDI+ draws into the engine's `Layer` buffers; methods
  are dispatched through TJS object handles. (Already native:
  `crates/tvp-visual/src/natives/gdiplus.rs` and `window.rs`.)
* **`menu`, `win32dialog`, `windowEx` — NO.** User32/GDI32/SHELL32 UI plus the
  TVP event/accelerator/window-handle graph. These are the *opposite* of
  self-contained: their whole job is engine/window state.

So of nine, exactly one clears the bar.

---

## 4. The TVP ABI direction (why "call the plugin" is not simple)

It is worth stating precisely what `V2Link` does, because it determines what a
host must implement. In ncbind:

1. The host exports `TVPGetFunctionExporter()`; the loader passes the exporter to
   `V2Link(exporter)` (`PluginImpl.h:48-52`).
2. The plugin resolves **host factory functions by name** —
   `TJSCreateNativeClassForPlugin`, `TJSCreateNativeClassMethod`,
   `TJSCreateNativeClassProperty`, `TJSNativeClassRegisterNCM`,
   `TJSRegisterNativeClass`, `TJSCreateArrayObject`, `TVPGetScriptDispatch`, … —
   and uses them to build and register its class/method descriptors.
3. The host's **TJS VM** later dispatches script calls to the plugin's
   `tTJSNativeClassMethodCallback`/instance callbacks, passing `tTJSVariant*`
   argv and an `iTJSDispatch2*` `objthis`.

Crucially, the plugin does **not** expose a C entry point that a foreign process
can call. Calling `V2Link` from a host exe only *registers* the plugin with
*that host's* TJS VM. To actually exercise a method, the host must own a TJS VM.
That is why the eight non-C-API plugins cannot be reduced to a callable function
without re-creating the TVP core.

The registration macros are visible in the reference (this fork statically
auto-registers, but the same classes/methods):
`reference/cpp/core/plugin/ncbind.hpp:2157` `NCB_REGISTER_CLASS_COMMON`,
`:2248-2266` the method/property/ctor registration macros. The runtime exporter
path is the `#if 0` block at `PluginImpl.h:21-66` that the shipped DLLs were
built against.

---

## 5. Design of a host exe (for the self-contained case)

### 5.1 Architecture

```
┌────────────────────────── Linux native krkr-rs (Rust/Bevy, x86_64 ELF) ─────────────────────────┐
│  tvp-sound / tvp-visual / tjs2-sys …                                                             │
│  PluginSidecar (Rust):                                                                           │
│    DecoderClient ─── control: 127.0.0.1:PORT (TCP)  ───┐                                         │
│                   └─ data:   /tmp/krkr-host-<pid>.pcm (mmap ring) ──┐                            │
└────────────────────────────────────────────────────────────────────┼────────────────────────────┘
                                                                     │ (Wine maps Z:\tmp\...)
┌──────────────────────────────── Wine process: host.exe (i386 PE) ──▼────────────────────────────┐
│  LoadLibraryA("wuvorbis.dll")  →  GetProcAddress("wu_ov_open_callbacks"/"wu_ov_read"/…)          │
│  ov_callbacks { read, seek, close, tell }                                                        │
│  decode loop → PCM → shared-memory ring                                                          │
│  (NO TVP exporter needed: the C API bypasses V2Link entirely)                                    │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
```

Two bridge variants:

* **One-shot sidecar (simplest).** Start `wine host.exe in.ogg out.pcm` per
  decode; read the PCM file. No IPC, no shared memory, no daemon. Cost is one
  process spawn per track (~tens of ms under Wine). Fine for short SE/BGM where
  the engine can pre-decode; bad for streaming/seeking.
* **Persistent worker (recommended if streaming).** A worker per host process
  holding open files/decoders. Control plane carries commands; a shared-memory
  ring carries PCM so the native audio thread never blocks on a syscall per
  sample.

### 5.2 The host function table: implement natively, no forwarding

For `wuvorbis` we need none, because we bypass `V2Link` (§5.3). If a future
C-API-less self-contained plugin is encountered and we *must* run `V2Link`, the
right move is to **implement the handful of requested functions natively inside
the host exe**, not forward them over IPC. The pure/util bucket (22 functions)
is small; e.g. `TVPFillARGB`/`TVPStretchCopy`/`TVPConstAlphaBlend_SD*` are
straightforward pixel loops, `TVP_md5_*` is MD5, and the TJS factory functions
can be satisfied with a *minimal mock* only if the host never dispatches TJS.

Forwarding the host table is a non-starter: many functions take or return
`iTJSDispatch2*`, `ttstr&`, `iTVPTransHandlerProvider*`, `HWND`, and register
callbacks *back into the host* (`TVPAddTransHandlerProvider`, `TVPPostEvent`,
`TVPCreateEventObject`). Those are process-local pointers and vtables; a
copy-across-the-wire design is a distributed object system, not an RPC shim.

### 5.3 The `wuvorbis` bypass (the actual minimal design)

Because `wuvorbis` exports the libvorbisfile API:

1. `h = LoadLibraryA("wuvorbis.dll")` (the game's DLL lives in the game dir or a
   Wine prefix path passed by the caller).
2. `sz = Query_sizeof_OggVorbis_File()`; allocate `sz` bytes for the opaque
   `OggVorbis_File`.
3. Build `ov_callbacks { read, seek, close, tell }` over a C `FILE*` or a
   memory image.
4. `wu_ov_open_callbacks(datasource, vf, NULL, 0, cb)`; read format via
   `wu_ov_info`; decode with `wu_ov_read`.
5. `wu_ov_clear`, `FreeLibrary` on shutdown.

This is exactly the in-tree engine's decoder call pattern
(`VorbisWaveDecoder.cpp:217-221`), so the semantics match the game's audio.

### 5.4 IPC command protocol (text sketch)

Control plane (TCP to `127.0.0.1:PORT`, or a Wine named pipe created by the host
and connected to via a Linux proxy — TCP is the most portable across the
Wine/Linux boundary). All little-endian, fixed header:

```
CtrlMsg {
  u32 magic   = 'KRKR' (0x524B524B)
  u16 version = 1
  u16 op
  u32 seq
  u32 len          // payload bytes following
  u64 arg0         // op-specific (sample index / storage id / flags)
  u64 arg1
}
```

Ops:

| op | name | payload | reply |
|---|---|---|---|
| 1 | `HELLO` | client version, plugin name | `HELLO_ACK` + caps |
| 2 | `OPEN` | storage bytes (or path string), requested format | `OPEN_ACK {sample_rate, channels, total_samples, codec}` |
| 3 | `READ` | `arg0 = max_samples` | `READ_ACK {n}` then `n*channels*2` bytes on the data plane |
| 4 | `SEEK` | `arg0 = sample` | `SEEK_ACK {actual}` |
| 5 | `CLOSE` | — | `CLOSE_ACK` |
| 6 | `PING` | — | `PONG` |
| 0xEE | `ERR` | UTF-8 message | — |

Data plane: a file-backed shared-memory ring, mapped by the native side at
`/tmp/krkr-host-<pid>.pcm` and by Wine at `Z:\tmp\krkr-host-<pid>.pcm`
(`\\?\Z:\...` for long paths). Header:

```c
struct RingHdr {
    volatile uint32_t head;      // producer (host) write index
    volatile uint32_t tail;      // consumer (native) read index
    uint32_t capacity;           // bytes, power of two
    uint32_t flags;              // eof / error
    uint32_t channels, bits;     // 1/2, 16
};                                // PCM bytes follow, wrap-around
```

The native audio thread drains the ring; the control thread owns `seq` and
back-pressure. If PCM throughput is low (stereo 16-bit ≈ 176 KB/s), TCP alone
suffices and the ring can be omitted — shared memory is only necessary to keep
the realtime audio callback off the socket path.

Handshake and lifetime: native side binds the TCP listener *before* spawning
`wine host.exe --connect 127.0.0.1:PORT --shm Z:\tmp\krkr-host-<pid>.pcm`;
the host connects and sends `HELLO`. On engine shutdown the native side sends
`QUIT` (or closes the socket) and reaps the Wine process.

### 5.5 Minimal working example (compiles; not executed here)

The following sidecar **compiles to a real PE32 i386 exe** with the installed
zig, proving the toolchain path. It was *not run*: `wine` is absent on this
machine, and the game DLLs are proprietary.

Build (verified):

```text
$ zig cc -target x86-windows-gnu -O2 host_demo.c -o host_demo.exe
$ file host_demo.exe
host_demo.exe: PE32 executable for MS Windows 6.00 (console), Intel i386, 7 sections
$ objdump -p host_demo.exe | grep 'DLL Name' | sort -u
	DLL Name: api-ms-win-crt-*.dll      # UCRT, provided by Wine
	DLL Name: KERNEL32.dll
```

Source (`/tmp/host_demo.c` in the reproduction; abbreviated here):

```c
#include <windows.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct OggVorbis_File OggVorbis_File;   /* opaque; size queried at runtime */

typedef size_t (*ov_read_func)(void*, size_t, size_t, void*);
typedef int    (*ov_seek_func)(void*, long long, int);
typedef int    (*ov_close_func)(void*);
typedef long   (*ov_tell_func)(void*);
typedef struct { ov_read_func read; ov_seek_func seek; ov_close_func close; ov_tell_func tell; } ov_callbacks;

static size_t f_read(void *p, size_t sz, size_t n, void *ds){ return fread(p, sz, n, (FILE*)ds); }
static int    f_seek(void *ds, long long o, int w){ return fseek((FILE*)ds, (long)o, w); }
static int    f_close(void *ds){ return fclose((FILE*)ds); }
static long   f_tell(void *ds){ return ftell((FILE*)ds); }

int main(int argc, char **argv) {                 /* argv[1]=in.ogg argv[2]=out.pcm */
    HMODULE lib = LoadLibraryA("wuvorbis.dll");
    SIZE_T (*q_sizeof)(void) =
        (SIZE_T(*)(void))GetProcAddress(lib, "Query_sizeof_OggVorbis_File");
    int (*open_cb)(void*, OggVorbis_File*, const char*, long, ov_callbacks) =
        (int(*)(void*,OggVorbis_File*,const char*,long,ov_callbacks))
        GetProcAddress(lib, "wu_ov_open_callbacks");
    long (*read_pcm)(OggVorbis_File*, char*, int, int, int, int, int*) =
        (long(*)(OggVorbis_File*,char*,int,int,int,int,int*))
        GetProcAddress(lib, "wu_ov_read");
    int (*clear)(OggVorbis_File*) =
        (int(*)(OggVorbis_File*))GetProcAddress(lib, "wu_ov_clear");

    FILE *in = fopen(argv[1], "rb"), *out = fopen(argv[2], "wb");
    OggVorbis_File *vf = (OggVorbis_File*)calloc(1, q_sizeof());
    ov_callbacks cb = { f_read, f_seek, f_close, f_tell };
    open_cb(in, vf, NULL, 0, cb);
    char buf[4096]; int bs = 0; long n;
    while ((n = read_pcm(vf, buf, sizeof buf, 0, 2, 1, &bs)) > 0) fwrite(buf, 1, (size_t)n, out);
    clear(vf); free(vf); fclose(out);
    return 0;
}
```

The Rust side only needs to drive the control protocol above; it never sees a
TVP type.

### 5.6 Build toolchain — what is / is not installed here

| Need | Status |
|---|---|
| C/C++ → i386 PE (`zig cc -target x86-windows-gnu`) | **installed, verified** (`zig 0.16.0`); links full PE32 exe and DLL |
| mingw-w64 `i686-w64-mingw32-gcc` | absent (zig substitutes) |
| Rust `i686-pc-windows-gnu` target | **not installed** (`rustup target list --installed`); can be added (`rustup target add i686-pc-windows-gnu`) — out of scope here |
| `wine` / `wine64` to *run* the exe | **absent** |
| Wine prefix / DLL deployment | absent |

So the host exe is buildable but **not runnable on this machine**; any claim
about runtime behaviour is design-level, not measured.

---

## 6. The coupled eight: degeneracy analysis

If we insist on running the other eight DLLs, the host must satisfy ~13-23 host
functions each, of which the TJS factory/dispatch and engine buckets dominate.
Concretely:

* **A TJS2 VM is mandatory.** `TJSCreateNativeClassForPlugin`,
  `TJSCreateNativeClassMethod`, `TJSNativeClassRegisterNCM`,
  `TJSRegisterNativeClass`, `TJSCreateArrayObject`, `TJSCreateDictionaryObject`,
  `TJSDoVariantOperation`, `TVPGetScriptDispatch`, `TVPExecuteExpression` are
  used by **every** non-`wuvorbis` plugin. The host must be able to hold
  `iTJSDispatch2` objects, execute expressions, and invoke the plugin's
  registered callbacks. That is the krkr-rs `tjs2-sys` C++ VM
  (`crates/tjs2-sys/build.rs` compiles the whole reference `cpp/core/tjs2/*`).
* **A storage layer is mandatory** for `fstat`, `KAGParserEx`, `layerExDraw`
  (`TVPCreateIStream`, `TVPCreateTextStreamForRead`, `TVPNormalizeStorageName`,
  `TVPGetPlacedPath`, `TVPGetLocalName`, `TVPExtractStorageName`,
  `TVPIsExistentStorageNoSearchNoNormalize`) — i.e. XP3/cxr mounting and path
  resolution.
* **A window/event loop is mandatory** for `menu`, `win32dialog`, `windowEx`
  (`TVPGetApplicationWindowHandle`, `TVPRegisterAcceleratorKey`,
  `TVPPostEvent`, `TVPCreateEventObject`, `TVPBreathe`, `TVPDoTryBlock`,
  `TVPClearGraphicCache`) — i.e. the TVP message pump and Win32 window.
* **The `Layer` object graph is mandatory** for `layerExDraw`/`extrans`
  (provider registration, layer memory, clipping, appearance).

By the time all of that exists, the "host exe" contains the TJS VM, the storage
layer, the visual/event core, and a Win32 window — i.e. **it is the engine**.
That is precisely option **B1** of `docs/wine_box64.md` ("everything is a Wine
app"). There is no smaller box that runs these DLLs, because the DLLs' *only*
interface is that core.

### Why per-call IPC forwarding is not viable

1. **Type safety / ownership.** Host calls pass `iTJSDispatch2*`, `ttstr&`,
   `iTVPTransHandlerProvider*`, `HWND`, `iTVPWaveDecoder*`. These are raw
   process-local pointers that plugins dereference and pass back. A process
   boundary cannot carry them without a full distributed object system with
   identity, refcounting and marshalling for every interface.
2. **Callbacks.** Plugins *install* callbacks into the host
   (`TVPAddTransHandlerProvider`, `TVPAddCompactEventHook`, `TVPCreateEventObject`,
   `TVPRegisterAcceleratorKey`). The host must call back into the plugin (possibly
   on another thread). Round-tripping each callback across IPC inverts control
   flow and deadlocks easily (`TVPBreathe`/`TVPDoTryBlock` are synchronous
   re-entrancy points).
3. **Latency / volume.** `extrans` transitions and `layerExDraw` drawing are
   per-pixel / per-frame. A 1280×720 transition at 60 fps is ~5.5×10⁷ pixel
   operations per second; if even a fraction became IPC calls at ~2-10 µs each,
   a single frame costs seconds to minutes. `KAGParserEx` allocates a TJS object
   per tag; `fstat` per storage path. No forwarding layer survives this.
4. **Synchronisation.** The TJS VM and Win32 message pump are single-threaded
   and re-entrant; a bridge must mirror that on both sides. This is a research
   project, not an engineering task.

**Degeneracy threshold:** a plugin is host-exe-viable iff it exposes a
C-callable function over plain data, needing at most the pure/util host bucket.
In this package that is exactly `wuvorbis`. For everything else the idea stops
paying off the moment the plugin's only entry point is `V2Link`.

---

## 7. Alternative that avoids the bridge: cross-compile krkr-rs to a Windows PE

This is `docs/wine_box64.md` option E/B1, updated with what is on this machine.

### 7.1 Bitness is forced to i686

The DLLs are **i386** (`file` output above). A 64-bit PE **cannot**
`LoadLibrary` a 32-bit DLL, and Wine's WoW64 runs a 32-bit image as a 32-bit
process; it does not make 32-bit code callable from a 64-bit address space. So:

* `x86_64-pc-windows-gnu` is useless for loading the plugins (it would only help
  a plugin-free "run the engine on Wine" experiment).
* The plugin-using build must be **`i686-pc-windows-gnu`** (32-bit, 2 GB address
  space).

### 7.2 What is buildable today

* The **C++ TJS2 VM** is already built via `zig c++` in
  `crates/tjs2-sys/build.rs` (the `zig build` deps step stages oniguruma, fmt
  and spdlog). Zig 0.16 can emit COFF for `x86-windows-gnu`
  (verified), so the VM is cross-compilable in principle. In practice the
  build.rs must pass a Windows target through to *both* the `zig build` deps step
  and every `zig c++` compile, and provide Windows-target `libonig`/`fmt`/`spdlog`.
  That is real work (L).
* **Rust target** `i686-pc-windows-gnu` is not installed; it would be added with
  `rustup target add` plus a linker (zig can act as one, or mingw-w64).
* **Bevy 0.19 / wgpu**: wgpu has DX12 and Vulkan backends that nominally support
  32-bit Windows, but Bevy's dependency tree is overwhelmingly tested on 64-bit;
  `usize`/pointer assumptions, large-asset address space, and 32-bit `wgpu`/`naga`
  builds are all risk. This is the largest unknown (XL).
* **Windowing**: `winit`'s Windows backend under Wine goes through `user32` →
  Wine's `winex11.drv` or `winewayland.drv`. The process is a Wine process; you
  lose the native Linux path (Wayland/X11 directly), gain Wine's input/IME/
  focus quirks, and render through Wine's Vulkan/D3D translation.

### 7.3 Trade-offs vs the host exe

| Dimension | Host exe + IPC (self-contained only) | Whole engine as i686 PE under Wine |
|---|---|---|
| Address space | plugins stay in Wine; engine native | plugins and engine share one space |
| Host API work | none (C API bypass) / 22 pure funcs | full 59 + real semantics |
| TJS VM | native (already built) | rebuilt for i686 COFF |
| Rendering | native Bevy | Bevy/wgpu on Wine (32-bit) |
| IPC | yes (but tiny, PCM only) | **none** |
| Platforms | Linux (+Windows host if desired) | Linux/Wine; ARM needs box64/FEX |
| Effort | S (one plugin) | XL |
| Risk | low (bounded decoder) | high (32-bit Bevy, Wine graphics, build system) |

### 7.4 ARM / Android

The game also targets ARM (the APK's `libgame.so`), but:

* Native krkr-rs on Android should decode Vorbis natively (Rust), not via Wine.
* Running an **i386 PE** under Wine on ARM requires `box86` or `box64`'s
  experimental WoW64 or Hangover/FEX (`docs/wine_box64.md` §6). Pixel-heavy
  plugins (`layerExDraw`, `extrans`) are the worst case for emulation.
* There is no way to inject the i386 DLLs into the native ARM engine; the
  cross-compile alternative only makes sense as a *Wine app*, not as the Android
  product.

So the PE-cross-compile route is a desktop/Linux (x86_64 host, i686 guest)
experiment, not an Android strategy.

---

## 8. Effort estimates and per-plugin verdict

Scale: **S** < 1 day · **M** days · **L** 1-3 weeks · **XL** months.

| Plugin | Host exe effort | Verdict | Reason |
|---|---:|---|---|
| `wuvorbis` | **S** | **Worth it (sidecar), if needed** | Exports full C decoder API; no TJS, no exporter. ~1 C file + a small Rust client. |
| `extrans` | M | Not worth it here | Needs provider registry + layer ABI; krkr-rs already implements the transitions natively (`crates/tvp-natives/src/extrans.rs`). |
| `csvParser` | L | Not worth it | Returns TJS objects; native impl exists (`crates/tvp-storages/src/csv_parser.rs`). |
| `fstat` | L | Not worth it | Storage + TJS + Win32 file APIs; native impl exists (`crates/tvp-storages`). |
| `KAGParserEx` | XL | Not worth it | Full TJS object graph + events; native impl exists (`crates/tvp-kagparser`). |
| `layerExDraw` | L/XL | Not worth it | GDI+ into the `Layer` graph + TJS; native impl exists (`crates/tvp-visual`). |
| `menu` | XL | Not worth it | Win32 menu + TVP events/accelerators; script fallbacks exist. |
| `win32dialog` | XL | Not worth it | Win32 dialogs + `Win32GenericDialogEX`; script fallback exists (`crates/tvp-natives/src/plugin_stubs.rs`). |
| `windowEx` | XL | Not worth it | Win32 window + engine `Window`; native impl exists (`crates/tvp-visual/src/natives/window.rs`). |

**Recommended scope: "self-contained C-API decoders only."** Implement the host
exe as a *narrow decoder sidecar* pattern (load a codec DLL, call its C API,
stream bytes back), and only for plugins that actually export a C API. For this
game, `wuvorbis` is already covered by `tvp-sound` (symphonia + libopus) and the
plugin set is emulated by built-in Rust natives
(`crates/tvp-natives/src/plugins.rs:100-140`), so the sidecar is a
future-proofing tool, not a task for this title.

Do **not** attempt a generic "full TVP-in-a-box" host: it is option B1 and is
equivalent to porting/running the whole engine under Wine.

---

## 9. Risks

* **Proprietary DLLs.** The game's plugins are not ours; shipping them (repo,
  prefix, APK) is a distribution/legal risk. A dev sidecar must load them from
  the user's own game directory, never vendor them.
* **Wine licensing/operational cost.** Wine is LGPL-2.1+; prefixes, `winex11`
  vs `winewayland`, 32-bit WoW64, and per-distro packaging are ongoing cost.
* **Toolchain drift.** The pure C path is blessed by zig today, but the *Rust
  i686 Windows* path is unvalidated here (no target, no Wine to run).
* **ABI fragility.** The exporter ABI is `#if 0` in the vendored fork; the
  "true" interface lives only in the original SDK and the shipped DLL strings.
  A mock host factory that only approximates the ncbind classes will crash on
  registrations it mis-models — another reason to bypass `V2Link` when possible.
* **`win32dialog` coverage gap.** `crates/tvp-natives/src/plugins.rs:118-133`'s
  emulated-name list omits `win32dialog.dll` even though `system/initialize.tjs`
  (indirectly, via `k2compat/win32dialog.tjs`) links it; that is handled today by
  a script fallback, and is unrelated to the host-exe question but worth noting.

---

## 10. Summary

* The nine DLLs are i386 PE; eight export **only** `V2Link`/`V2Unlink`, so their
  entire functionality is reachable *only* through the TVP exporter + TJS.
* The real exporter query surface is **59 distinct host functions** (the earlier
  "89" included C++ member-function RTTI strings). Of those, ~16 need a TJS VM
  and 21 need engine/storage/window/event state.
* **Only `wuvorbis` is self-contained**, because it uniquely exports the full
  `wu_ov_*` libvorbisfile C API; a host exe can call it directly and skip
  `V2Link` and the host table entirely.
* A host exe for the other eight must provide a TJS VM + storage + event loop +
  `Layer` graph — i.e. it *becomes* the engine. Per-call IPC forwarding is
  blocked by raw pointer/vtable sharing, callbacks, and per-pixel/per-tag call
  volume.
* The bridge-free alternative (port krkr-rs to `i686-pc-windows-gnu` and run the
  whole thing under Wine) is coherent but XL, forced to 32-bit, and risky for
  32-bit Bevy/wgpu.
* **Recommendation:** keep the native reimplementation; if a genuinely
  Windows-only codec appears, add a **narrow C-API decoder sidecar** (S effort),
  not a general plugin host. `zig` on this machine can build the sidecar; `wine`
  would have to be installed to run it.

---

## 11. Evidence index

| Claim | Evidence |
|---|---|
| DLLs are i386 PE | `file /tmp/dlls/*.dll` output in §1 |
| 8/9 export only `V2Link`/`V2Unlink`; `wuvorbis` exports 40 | `objdump -p` export tables (§2.1) |
| No TVP import library; runtime resolution by name | §2.2 import tables; `reference/cpp/core/plugin/PluginImpl.h:21-66` |
| Exporter ABI | `reference/cpp/core/plugin/PluginImpl.h:28-52` |
| TSS module (`wuvorbis`) ABI | `reference/cpp/core/plugin/PluginImpl.h:55` |
| Host query strings + counts | §2.3 extraction; 59 distinct after excluding C++ members |
| TJS class/method registration | §2.4 wide-string scan; `reference/cpp/plugins/csvParser.cpp:432,439,445`; `reference/cpp/plugins/fstat/main.cpp:918-944`; `reference/cpp/plugins/layerex_draw/windows/main.cpp:607`; `reference/cpp/plugins/windowEx.cpp:1513-1527` |
| ncbind registration macros | `reference/cpp/core/plugin/ncbind.hpp:2157,2248-2266` |
| wuvorbis decoder call pattern | `reference/cpp/core/sound/VorbisWaveDecoder.cpp:32,217,221` |
| krkr-rs current plugin handling | `crates/tvp-natives/src/plugins.rs:62-140`; `plugin_stubs.rs:1-25`; `crates/tvp-storages/src/csv_parser.rs`; `crates/tvp-kagparser/src/lib.rs`; `crates/tvp-visual/src/natives/gdiplus.rs`; `crates/tvp-visual/src/natives/window.rs` |
| TJS2 VM build | `crates/tjs2-sys/build.rs` |
| Bevy 0.19 | `crates/render/Cargo.toml` |
| zig cross-compiles i386 PE | §1.1 and §5.5 command output |
| wine/Rust-target absent | §1.1 environment |

---

## 12. Reproducible commands

```bash
# 1. extract the nine DLLs (reuse extract_xp3's open_index/get_file)
cd /mnt/DATA/Document/code/krkr-rs
python3 - <<'PY'
import sys, os, struct
sys.path.insert(0, 'scripts'); import extract_xp3 as E
f, index = E.open_index('/mnt/DATA/Games/Others/test/data.xp3')
names, pos = [], 0
while pos + 12 <= len(index):
    tag = index[pos:pos+4]; size = struct.unpack('<Q', index[pos+4:pos+12])[0]
    if tag == b'File':
        payload = index[pos+12:pos+12+size]; p2 = 0
        while p2 + 12 <= len(payload):
            st = payload[p2:p2+4]; ss = struct.unpack('<Q', payload[p2+4:p2+12])[0]
            if st == b'info':
                info = payload[p2+12:p2+12+ss]
                nlen = struct.unpack('<h', info[20:22])[0]
                names.append(info[22:22+nlen*2].decode('utf-16-le', 'replace'))
            p2 += 12 + ss
    pos += 12 + size
os.makedirs('/tmp/dlls', exist_ok=True)
for n in [x for x in names if x.lower().endswith('.dll')]:
    open('/tmp/dlls/'+os.path.basename(n.replace('\\','/')), 'wb').write(
        E.get_file(f, index, n.replace('\\','/').lower()))
PY

# 2. exports / imports
cd /tmp/dlls
for d in *.dll; do echo "=== $d ==="; objdump -p "$d" | sed -n '/The Export Tables/,$p' | head -40; done
for d in *.dll; do echo "=== $d imports ==="; objdump -p "$d" | awk '/Member-Name/{f=1;next} f&&/^\t[0-9a-f]{8} /{print $NF} f&&/^$/{f=0}' | sort -u; done

# 3. true host query surface (global-scope ncbind keys) and counts
for d in *.dll; do strings -n3 "$d" | grep -oE ' ::[A-Za-z_][A-Za-z0-9_]*' | sed 's/^ :://'; done \
  | sort -u | grep -v '^operator$' | wc -l          # -> 59
for d in *.dll; do n=$(strings -n3 "$d" | grep -oE ' ::[A-Za-z_][A-Za-z0-9_]*' | sed 's/^ :://' | sort -u | grep -vc '^operator$'); echo "$d $n"; done

# 4. registered TJS class/method wide strings
python3 - <<'PY'
import glob, re
for d in sorted(set(x.lower() for x in glob.glob('/tmp/dlls/*.dll'))):
    b = open(d,'rb').read(); res = set()
    for m in re.finditer(rb'(?:[\x20-\x7e]\x00){4,}', b):
        s = m.group().decode('utf-16-le','replace')
        if re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]{3,}', s): res.add(s)
    keep = sorted(s for s in res if not s.startswith(('TVP','TJS','tTJS','ncb','R6','std')))
    print(d, ", ".join(keep[:60]))
PY

# 5. build the sidecar (verified: produces PE32 i386; not run — no wine)
zig cc -target x86-windows-gnu -O2 /tmp/host_demo.c -o /tmp/host_demo.exe
file /tmp/host_demo.exe
```

`x86-windows-gnu` (not `i686-windows-gnu`) is zig 0.16's 32-bit x86 Windows
triple. `x86_64-windows-gnu` produces PE32+ and **cannot** load the i386 DLLs.
