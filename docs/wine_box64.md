# Wine + box64 usability study: loading the game's real Windows plugin DLLs (and/or its `.exe`) into krkr-rs

Scope: **DEV builds only.** This is a feasibility investigation, not an implementation
plan. It asks whether Wine (plus box64 on ARM) can load the game's real Windows
plugin DLLs into the Rust/Bevy (`winit`) process, or run the game's `.exe`, while
`winit`/Bevy keeps the render surface.

Repo: `/mnt/DATA/Document/code/krkr-rs`
Game: `/mnt/DATA/Games/Others/test`
Reference engine in tree: `reference/` (the **KrKr2 Emulator**,
`github.com/2468785842/krkr2`, per `reference/README.md:1`).

---

## 0. Verdict summary

| # | Approach | Verdict | One-line reason |
|---|---|---|---|
| A | Load the real PE DLLs **in-process** into krkr-rs via Wine / `libwine` | **Not viable** | Modern Wine has no supported embedding API; it *is* the process (`__wine_main`), and the TVP ABI passes raw host function pointers, so plugin and host must share one address space. |
| B | **Cross-process RPC bridge**: a Wine worker loads the DLL and proxies every plugin export | **Not viable** (except a bespoke single-plugin shim) | Every plugin calls ~89 host functions *and* hands back callbacks/`iTJSDispatch2` objects; a generic bridge is a distributed object system. |
| C | Run the game's **`.exe`** under Wine while winit/Bevy provides the surface | **Not viable with these assets** | The package contains **no Windows `.exe`** — it is an Android `Kirikiroid2` APK whose engine is ARM `libgame.so`. Even with an `.exe`, Wine owns the window. |
| D | **X11 window embedding**: Wine renders into its own X11 window, reparented under a winit/Bevy frame | **Viable-with-caveats** (X11 only) | Technically possible; Wine still owns input/focus/geometry, the WM can interfere, and it does nothing on Wayland. Latency + fragility. |
| E | Build krkr-rs / the plugin host as a **32-bit Windows PE** and run it under Wine, loading the DLLs in-process (Bevy's Windows/winit backend) | **Viable-with-caveats** | The only way to load the DLLs without reimplementing them; but the process is a Wine process, it's i686, and the host exporter (~89 functions) must be rebuilt. |
| F | Out-of-process Wine worker for one **self-contained compute** task (e.g. a Windows-only codec), shared-memory IPC | **Viable-with-caveats** | Clean isolation, bounded surface; but krkr-rs already decodes the game's audio natively, so the value is niche. |
| G | Continue the **native Rust reimplementation** (current) | **Viable** — recommended | Already working; matches what Kirikiroid2 and the KrKr2 reference do. |
| H | **ARM/Android** Wine + box64/box86 + Winlator/Hangover | **Not viable for embedding; viable only as a full Windows app under Wine** | i386 PE needs WoW64 + Box64/FEX (Hangover) or Box86; extra 2–10×+ emulation; still Wine-owned process/window. |
| I | **Minimal Wayland compositor as Wine's display backend** (Android + box64): run Windows `.exe`s, not the DLLs | **Viable-with-caveats** for running Windows games; **not** a way to call the game's DLLs in-process | Wine's `winewayland.drv` needs only a small strict set of globals that Smithay already provides; the hard parts are the Android EGL backend and the box64/Wine packaging. See §12. |

The two **DLL-focused** approaches worth sketching are **D** (window-level
coexistence) and **E** (process-level, same-address-space DLL loading). See §8.
The separate, more promising **product** path — running whole Windows `.exe`s
under Wine behind an in-app minimal Wayland compositor — is evaluated in §12.

---

## 1. Local environment (verified)

```
$ uname -m
x86_64
$ uname -a
Linux wqlouis-desktop 7.2.4-arch1-2 #1 SMP PREEMPT_DYNAMIC ... x86_64 GNU/Linux
$ which wine wine64 wine32 box64 box86
which: no wine in (...)
which: no wine64 in (...)
which: no wine32 in (...)
which: no box64 in (...)
which: no box86 in (...)
$ cat /etc/os-release
NAME="Arch Linux"
```

* Host arch is **x86_64**. `wine`/`wine64`/`wine32`/`box64`/`box86` are **not installed**.
* **`box64` is irrelevant on this host.** box64 ("Linux Userspace x86_64 Emulator …
  targeted at ARM64, RV64 and LoongArch", `github.com/ptitSeb/box64`) only makes
  sense on a non-x86 host. On x86_64, Wine executes the PE natively:
  * a **32-bit i386 PE** is handled by Wine's **WoW64** path (on a 64-bit-only Wine),
    or by a classic 32-bit Wine/`wine` in a multilib prefix;
  * a **64-bit x86_64 PE** runs natively.
* The game's DLLs are **i386** (see §2), so the relevant x86_64 question is
  "wine + WoW64", not box64.

Browser note: the WineHQ wiki is behind bot protection (Anubis returned
"Access Denied"). I used (a) the archived WineHQ wiki, and (b) the Wine source
mirror via `raw.githubusercontent.com/wine-mirror/wine`, plus the box64 / Winlator /
Hangover GitHub pages. All non-obvious claims below cite a fetched source path.

---

## 2. The game: what is actually in `/mnt/DATA/Games/Others/test`

It is **not** a Windows install. It is a **Kirikiroid2 Android package**:

```
data.xp3                633,260,458 B
Kirikiroid2_1.3.9.apk    32,304,636 B
patch.xp3                59,404,159 B
patch.tjs                    70,928 B
system.dat                    1,672 B
savedata/…
```

The `.apk` native payload is **ARM**, not PE:

```
$ bsdtar -tf Kirikiroid2_1.3.9.apk | grep '\.so$'
lib/arm64-v8a/libffmpeg.so
lib/arm64-v8a/libgame.so
lib/arm64-v8a/libSDL2.so
lib/armeabi-v7a/libffmpeg.so
lib/armeabi-v7a/libgame.so
lib/armeabi-v7a/libSDL2.so
$ file apk/lib/arm64-v8a/libgame.so
ELF 64-bit LSB shared object, ARM aarch64, … built by NDK r16b, stripped
```

There is **no `.exe`** anywhere (game dir or inside the archives), and no native
`.so` plugin. Kirikiroid2's engine is `libgame.so`; strings in it reference
*statically compiled* plugin sources (`../../src/plugins/KAGParserEx.cpp`,
`ExtKAGParser.cpp`). So Kirikiroid2, exactly like the in-tree KrKr2 reference,
**does not load the Windows PE plugins** — it links plugin code at build time.

### The real Windows DLLs *are* shipped inside `data.xp3`

`data.xp3` (23,572 entries) contains exactly nine `.dll` members, and they are
**32-bit Windows PE**:

```
$ python3 scripts/extract_xp3.py data.xp3 extrans.dll > extrans.dll   # etc.
$ file /tmp/dlls/*.dll
csvparser.dll:   PE32 executable for MS Windows 4.00 (DLL), Intel i386, 5 sections
extrans.dll:     PE32 executable for MS Windows 4.00 (DLL), Intel i386, 5 sections
fstat.dll:       PE32 executable for MS Windows 4.00 (DLL), Intel i386, 5 sections
kagparserex.dll: PE32 executable for MS Windows 6.00 (DLL), Intel i386, 5 sections
layerexdraw.dll: PE32 executable for MS Windows 4.00 (DLL), Intel i386, 5 sections
menu.dll:        PE32 executable for MS Windows 5.01 (DLL), Intel i386, 5 sections
win32dialog.dll: PE32 executable for MS Windows 5.00 (DLL), Intel i386, 5 sections
windowex.dll:    PE32 executable for MS Windows 5.01 (DLL), Intel i386, 5 sections
wuvorbis.dll:    PE32 executable for MS Windows 4.00 (DLL), Intel i386, 7 sections
```

These are the game's **original Windows plugins**, carried along because the
`.xp3` is a platform-agnostic archive. The Android build cannot execute them.

### Which plugins the scripts actually ask for

Decoding the extracted scripts (they are UTF-16LE; a plain `grep` misses them):

```
system/initialize.tjs:127: Plugins.link("extrans.dll");
system/initialize.tjs:128: Plugins.link("csvParser.dll");
system/initialize.tjs:129: Plugins.link("layerExDraw.dll");
system/initialize.tjs:130: Plugins.link("fstat.dll");
system/initialize.tjs:132: Plugins.link("windowEx.dll");
system/initialize.tjs:135: Plugins.link("KAGParserEx.dll");
system/initialize.tjs:137: Plugins.link("getSample.dll");
system/sound.tjs:4:        Plugins.link("wuvorbis.dll");
k2compat/win32dialog.tjs:1: Plugins.link("win32dialog.dll") if (typeof global.WIN32Dialog == "undefined");
k2compat/k2compat.tjs:68,83,98-101: loadPlugin("win32dialog.dll") / loadPlugin("windowEx.dll") → Plugins.link(...)
k2compat/k2compat.tjs:237,246:       delayLoadPlugin("menu.dll", …) / ("KAGParser.dll", …)
```

Cross-check with the DLLs actually present: `getSample.dll` and `KAGParser.dll`
are **not shipped** (the game tolerates the failed load), the rest are.

krkr-rs's current behavior is to accept-and-log every `Plugins.link`:
`crates/tvp-natives/src/plugins.rs:62-84` (`native_link`) and the emulated-name
list at `:100-140` (`is_emulated_plugin`: csvparser, extrans, fstat, kagparser,
kagparserex, menu, wuvorbis, windowex, layerexdraw). Note `win32dialog` is *not*
in that list, even though the game asks for it; `getSample`/`KAGParser` are also
absent. That gap is currently handled by TJS fallbacks (`startup.tjs` defines a
`WIN32Dialog` class; `k2compat/win32dialog.tjs` adds `WIN32DialogEX`).

---

## 3. The TVP plugin ABI: why plugin and host must share one address space

The legacy TVP2 C ABI is declared in the reference, but **compiled out** in the
KrKr2 fork we vendor:

`reference/cpp/core/plugin/PluginImpl.h:21`
```c
#if 0
…
struct iTVPFunctionExporter                                  // :28
{
    virtual bool QueryFunctions(const tjs_char **name, void **function,        // :30
        tjs_uint count) = 0;
    virtual bool QueryFunctionsByNarrowString(const char **name, void **function, // :32
        tjs_uint count) = 0;
};
extern "C" {
    iTVPFunctionExporter * __stdcall TVPGetFunctionExporter();  // :48
    typedef HRESULT (_stdcall * tTVPV2LinkProc)(iTVPFunctionExporter *); // :51
    typedef HRESULT (_stdcall * tTVPV2UnlinkProc)();                     // :52
}
…
#endif                                                       // :66
```

The ABI is a **function-pointer table**:

1. the host (engine) exports `TVPGetFunctionExporter()`;
2. the plugin's `V2Link(exporter)` calls `QueryFunctionsByNarrowString` to obtain
   **raw C function pointers into the host process**;
3. the plugin then calls those pointers directly, for the rest of its life.

We measured exactly which host entry points the game's nine DLLs ask for by
extracting the signature strings the plugins query (ncbind uses
`"<ret> ::<name>(<args>)"`-style keys; the same strings appear verbatim in the
PEs):

```
csvparser.dll   20   extrans.dll     13   fstat.dll       37
kagparserex.dll 41   layerexdraw.dll 27   menu.dll        33
win32dialog.dll 31   windowex.dll    36   wuvorbis.dll     7
TOTAL distinct host entry points requested: 89
```

(examples: `TVPAddLog`, `TVPGetScriptDispatch`, `TVPExecuteExpression`,
`TVPThrowExceptionMessage`, `TJSCreateNativeClassForPlugin`,
`TJSNativeClassRegisterNCM`, `TVPFillARGB`, `TVPConstAlphaBlend_SD`,
`TVPAddTransHandlerProvider`, `TVPCreateEventObject`, …). The DLLs export only
`V2Link`/`V2Unlink` (plus the i386 stdcall decorations `_V2Link@4`/`_V2Unlink@0`).

**Consequences for any "bridge":**

* Every one of those 89 callbacks has native C++-ish types (`ttstr&`,
  `tTJSVariant*`, `iTJSDispatch2*`, `HWND`, raw pixel buffers). A cross-process
  bridge would need a thunk for each, i.e. re-declare the entire host API at an
  RPC boundary.
* Plugins also receive **host object pointers** and register **callbacks back
  into the host** (`TVPAddTransHandlerProvider`, event hooks, TJS closures). The
  object graph is shared, not copyable.
* The host side must *provide* all of this. The in-tree reference does **not**:
  KrKr2 replaced runtime loading with compile-time registration.
  `PluginImpl.cpp:95` `TVPLoadInternalPlugin` → `:143/:145`
  `ncbAutoRegister::LoadModule(...)`, and `ncbind.cpp:12` resolves the name
  against an in-process map `_internal_plugins` populated by
  `ncbAutoRegister::AllRegist()` (`PluginImpl.cpp:91`, `ncbind.hpp:2096`). The
  reference's own plugins live in `reference/cpp/plugins/*.cpp` and are linked in.
  The `Plugins.link` TJS method (`PluginImpl.cpp:291-340`) therefore only
  dispatches to that static registry.

So "load the real DLL" requires **both** (i) an address space in which the plugin
can call host pointers, and (ii) a host that implements ~89 functions. Wine gives
you (i) only if Wine owns the process; krkr-rs gives you (ii) only if it is the
Windows process.

---

## 4. Wine architecture: why Wine owns the process and the window

Fetched from `github.com/wine-mirror/wine` (master).

### 4.1 There is no supported "embed libwine into my process" API

* Modern Wine has **no `libwine`**. `libs/` contains only `winecrt0` (a static
  startup library: `libs/winecrt0/Makefile.in` → `exe_main.c`, `dll_main.c`, …),
  and `include/wine/` no longer ships a `library.h` embedding header.
* The process entry point is `__wine_main` in `dlls/ntdll/unix/loader.c:2109`:
  it calls `init_paths()`, `virtual_init()`, `init_environment()`, then
  `start_main_thread()`. That function (`loader.c:1861`) allocates the Windows
  thread/process environment, connects to `wineserver`, and loads `ntdll`:
  ```
  struct thread_data *data = virtual_alloc_first_thread_data();
  server_init_process( data );          // wineserver (separate Unix daemon)
  virtual_map_user_shared_data();
  init_cpu_info(); init_files(); init_startup_info(); dbg_init();
  init_thread_stack( data->teb, … );
  load_ntdll();
  load_wow64_ntdll( main_image_info.Machine );
  server_init_process_done();
  ```
* The **main module is a PE**: `load_main_exe` (`loader.c:1460`) opens the image
  (`open_main_image`) and maps it. A winelib app is a normal Unix executable that
  is *launched by Wine*; it is not a foreign host that happens to have loaded
  Wine.

There is no documented, supported way to bring that up inside an already-running
non-Wine ELF process (e.g. a Rust/winit/Bevy binary). Wine would have to take
over signal handling, TLS, the heap, `ntdll`, the PE address space, and the
thread-startup path. That is exactly the package `wine`/`winelib` provides, and it
changes the identity of the process.

### 4.2 The display driver is an in-process PE and it owns the window

`dlls/win32u/driver.c` is the user/GDI driver layer. It lazily loads a display
driver **into the process** (`load_display_driver` → `load_desktop_driver` →
`KeUserModeCallback(NtUserLoadDriver, …)`), with a `struct user_driver_funcs`
table (`pCreateWindow`, `pCreateWindowSurface`, …). On Linux the driver is
`dlls/winex11.drv` (X11) or `dlls/winewayland.drv` (Wayland). Window creation
goes through `NtUserCreateWindowEx` (`dlls/win32u/main.c:1338`).

X11 window management lives in `dlls/winex11.drv/window.c`:
* `is_window_managed` / `managed_mode` (`:347`, `:438`) — Wine talks to the WM;
* the driver explicitly supports **XEMBED** for its **system-tray** icons:
  `_XEMBED_INFO` at `:1565`, `make_window_embedded` at `:2122`;
* but `make_window_embedded` is only called from `X11DRV_SystrayDockInsert`
  (`:2984`), i.e. it is a tray feature, **not** a general "embed my top-level
  game window in your app" API. The `XReparentWindow` calls in the file
  (`:2315`, `:2336`, `:2686`) are Wine's own client/whole/clip-window plumbing.
* A "virtual desktop" mode exists (`is_virtual_desktop()`,
  `X11DRV_init_desktop`, `x11drv.h:858-860`; implementation in
  `programs/explorer/desktop.c`) but it is still one Wine-owned X11 window, not
  an offscreen surface you can sample.

**Net:** Wine's renderer and its window are the same process. Given a host
window, the only realistic coexistence is at the **X11 window** level (§5), or by
letting Wine be the whole app (§8-B).

### 4.3 32-bit on x86_64

Wine has a first-class WoW64 implementation (`dlls/wow64`, `dlls/wow64cpu`,
`dlls/wow64win`). On x86_64 it can run the i386 DLLs natively (the CPU runs
32-bit code); no box64 is involved. On ARM, the 32-bit x86 execution must be
emulated (see §6).

---

## 5. Surface-integration options for a winit/Bevy window

| Option | How | Feasibility / cost |
|---|---|---|
| **X11 reparenting / XEmbed** | `winit` creates an X11 window; find Wine's top-level X11 window and `XReparentWindow` it (or use an XEmbed client container). | **Best of the coexistence options, X11-only.** Fragile: the WM/managed-mode fights reparenting; Wine's own `embedded` flag is systray-only (`window.c:2984`); you must forward input and reconcile focus. No equivalent on Wayland. |
| **Wine virtual desktop offscreen + readback** | Run Wine with a virtual desktop in one X window; capture with XComposite/XShmGetImage, upload into a Bevy `Image`. | Works but adds a full GPU→CPU→GPU round-trip per frame, plus tearing/latency. Good for a debug overlay, bad for a 60 fps VN. |
| **Wayland sub-surface** | Attach Wine's Wayland surface as a sub-surface of the winit surface. | **Not viable as-is.** Wine's Wayland driver creates top-level `xdg_toplevel` windows; Wayland has no reparent/embed protocol for foreign clients. |
| **D3D/Vulkan external-memory interop** | Share a Vulkan image between Wine (winevulkan / DXVK / vkd3d) and Bevy via `VK_KHR_external_memory_fd`. | Theoretically possible on the same GPU, but the game's plugins here are GDI/GDI+/user32 (not D3D), so this buys nothing. High complexity. |
| **Offscreen render → readback → present** | Make Wine render to an offscreen drawable. | Wine's `null` driver renders nothing; the X11 driver renders to an X11 surface. You are back to capture. |

**Who owns input/focus:** Wine. Once Wine is running, its window (or embedded
window) receives and dispatches input on the Wine side. To drive Wine from Bevy
you would feed synthetic events over X11, which loses the raw input path
(`winit`/Bevy `ButtonInput`) and re-introduces latency.

---

## 6. box64 / box86 on ARM/Android (Winlator, Hangover)

Relevant because the game also targets ARM, but note the asset problem first:
**this package has no Windows `.exe`** — the ARM engine is `libgame.so`. So on
ARM the only thing Wine could run is a *different* Windows KiriKiri engine you
would have to supply, plus the game data.

What the projects actually provide:

* **box64** (`github.com/ptitSeb/box64`, MIT): runs **x86_64 Linux** binaries on
  ARM64/RV64/LoongArch. For **32-bit x86** it says plainly: *"For 32-bit binaries,
  use Box86 or Box32"*; *"For 32-bit components, Box86 is required"*; a Wine
  **WOW64 build** can run x86 Windows programs box64-only, but *"this is still
  experimental."* Our DLLs are i386, so plain box64 is not the right tool.
* **Winlator** (`github.com/brunodev85/winlator`, LGPL-2.1): an Android app that
  *"runs Windows (x86_64) applications with Wine and Box86/Box64"* (plus
  DXVK/VKD3D/Mesa). It is an **application runner**; it owns the window and the
  process. There is no embedding hook for a foreign native renderer.
* **Hangover** (`github.com/AndreRH/hangover`, LGPL-2.1): *"runs Win64 and Win32
  applications on arm64 Linux."* It uses **emulator DLLs** rather than emulating
  all of Wine: *"As soon as the application does a Windows/Wine system call …
  it's executed outside the emulator (native, fast)."* For i386 it uses
  `wowbox64.dll` (Box64) by default, or `libwow64fex.dll` (FEX), or
  `wow64cpu.dll` for native i386 on x86_64. This is the technically strongest ARM
  route for **32-bit Windows PE**, but the plugin still runs inside a Wine-owned
  process and window; it does not make the DLL callable from a separate Rust
  process.

**Emulation cost:** even with Hangover's syscall breakout, the application's own
code is emulated (Box64/FEX), and the render plugin (`layerExDraw`) is per-pixel
hot code — the worst case for emulation. A VN may tolerate it; a render-heavy
plugin will not.

**Bottom line on ARM:** Wine+box64/Hangover can *run a Windows KiriKiri app on
ARM*, but it cannot *inject the DLLs into krkr-rs/Bevy*. That is a different
product (`krkr-rs` is not involved), and it still lacks a Windows engine binary in
this package.

---

## 7. Alternatives, with honest verdicts

**(a) Run the game's Windows KiriKiri `.exe` under Wine (x86_64 or ARM).**
*Not viable with these assets.* There is no `.exe`; the only engine present is
ARM `libgame.so` (and it is Android-flavored). A generic `krkr2.exe` from
elsewhere would be a different engine build and a distribution/legal problem.
If you did supply one, this is just "play the Windows version under Wine": real
DLLs are used, but krkr-rs and winit/Bevy play no part. **Verdict: not the
integration asked for.**

**(b) Continue the native Rust reimplementation (current).**
Works today (README: plugin surface emulated; ADV loop in progress). It is what
Kirikiroid2 and the KrKr2 reference both do. **Verdict: viable.**

**(c) Narrow out-of-process Wine worker for self-contained compute (e.g. a
Windows-only codec) with shared-memory IPC.**
Clean isolation, bounded API, no ABI bridge. For this game, however, audio
decoding is already native (`tvp-sound`: symphonia + libopus), and the Windows
plugins that *look* like compute (wuvorbis) are exactly the ones already covered.
**Verdict: viable-with-caveats, low value here.**

**(d) Build krkr-rs as a `winelib` app / Windows PE that links the real DLLs and
uses Wine's windowing (no winit).**
This *is* the only way to use the DLLs unmodified. But: the plugin DLLs are
**i386**, so the host must be i686 too; the TJS2 C++ VM must build for
`i686-pc-windows-gnu`; the ~89-function TVP exporter must be reimplemented (the
reference does not have it — it links plugins statically); and Wine owns the
window. You can keep Bevy/wgpu via its Windows (DX12/Vulkan) backend and winit's
Windows backend, but you are no longer on Linux/native. **Verdict:
viable-with-caveats, large effort.**

---

## 8. Architecture sketches for the two most promising approaches

### Sketch A — window-level coexistence (X11): Wine plugin/engine as a sibling surface

Use when you want **the real DLLs** but are willing to let Wine own a sub-surface,
while Bevy keeps the rest of the app. X11 only.

```
 ┌──────────────────────────── rust / winit / Bevy process (x86_64 ELF) ───────────────────────────┐
 │                                                                                                  │
 │  Bevy App (DefaultPlugins, WindowPlugin)              Bevy 2D scene / UI / word-wrap / effects   │
 │   └─ primary winit window (X11) ── XReparentWindow ──┐                                            │
 │        (frame / chrome / overlays)                   │                                            │
 │  tvp native plugins… (native reimpl)                 │                                            │
 │                                                      ▼                                            │
 └──────────────────────────────────────────────────────────────────────────────────────────────┘
                                    X11 parent window
                                          │  (reparent)
 ┌────────────────────────────────────────┴─────────────────────────────────────────────────────┐
 │                     wine process (own address space, own Win32 world)                          │
 │  krkr2.exe + real layerexDraw.dll / windowEx.dll / win32dialog.dll / menu.dll / …              │
 │     V2Link(iTVPFunctionExporter*)  →  host = *this* Wine process's engine                      │
 │  winex11.drv creates a top-level X11 window  →  reparented into the winit frame                │
 │  input: X11 events → Wine (X11 focus). To drive it, winit forwards synthetic X11 events.       │
 └───────────────────────────────────────────────────────────────────────────────────────────────┘
```

* **Address-space model:** the DLL lives entirely inside the Wine process and
  calls *that* process's engine. No ABI bridge. The Rust side never calls plugin
  functions in-process.
* **Windowing ownership:** Wine owns the child X11 window and its input; Bevy
  owns the frame. Focus is a genuine problem (two GUI toolkits, one seat).
* **Readback variant:** if embedding is unreliable, capture the Wine window with
  XComposite/XShm and present it as a Bevy texture (adds a frame of latency).
* **What you still need:** the real Windows KiriKiri engine + the DLLs, and a
  Wine prefix. Neither is in the repo/package today.
* **Useful for:** `win32dialog` / `windowEx` / `menu` native dialogs and the
  `layerExDraw` render path, if you accept a Wine sibling window.
* **Not useful for:** integrating plugin output into the Bevy render graph without
  a copy.

### Sketch B — process-level, same address space: 32-bit Windows PE plugin host under Wine

The only architecture in which `V2Link` works unmodified. Two sub-variants.

```
 (B1) Everything is a Wine app (simplest same-address-space story)

 ┌───────────────────────── wine process (i686 PE) ──────────────────────────┐
 │  krkr-rs engine (built i686-pc-windows-gnu)                               │
 │    ├─ TJS2 C++ VM (mingw)                                                 │
 │    ├─ TVP host exporter: TVPGetFunctionExporter() + ~89+ functions        │
 │    │      (TVPAddLog, TVPGetScriptDispatch, TVPExecuteExpression, …)      │
 │    ├─ Plugins.link → LoadLibraryA("layerexDraw.dll") → V2Link(exporter)   │
 │    │      real DLL calls back into the host via raw function pointers     │
 │    ├─ Bevy/wgpu + winit (Windows backend: DX12/Vulkan + user32)           │
 │    └─ winex11.drv / winewayland.drv owns the window                       │
 └───────────────────────────────────────────────────────────────────────────┘

 (B2) Split: native Linux krkr-rs controller + i686 Wine "plugin service"

 ┌────────────── Linux host (Rust/Bevy/winit) ──────────────┐
 │  Scene, storage, TJS VM (native)                          │
 │  PluginClient ───── IPC (shared mem + control) ─────┐     │
 └─────────────────────────────────────────────────────┼─────┘
                                                        ▼
 ┌──────────────── wine process (i686 PE) ────────────────────────────────┐
 │  thin host: implements the ~89-function exporter against a *mirror*    │
 │  of the engine state; LoadLibraryA(real DLL); marshals every call and  │
 │  every callback (iTJSDispatch2, ttstr, pixel buffers).                 │
 └────────────────────────────────────────────────────────────────────────┘
```

* **B1** has no marshalling at all; it is "port krkr-rs to 32-bit Windows and run
  it on Wine". You get the real DLLs, but you inherit a second toolchain
  (mingw/i686 + Wine), and Wine is now your platform.
* **B2** tries to keep the Linux/Bevy host but must proxy the entire shared object
  graph across a process boundary — the Distributed-`iTJSDispatch2` problem. This
  is where approach B in the verdict table becomes "research project", not
  engineering.

---

## 9. Risks

**Licensing / distribution**
* Wine: LGPL-2.1-or-later (`wine-mirror/wine/LICENSE`). Bundling is possible with
  the usual relink/source obligations, and Wine has many system/library
  dependencies.
* box64: MIT; Winlator: LGPL-2.1; Hangover: LGPL-2.1 (GitHub API `license.spdx_id`).
* The **game's DLLs are proprietary** and not ours. Redistributing them (in a
  repo, an APK, or a Wine prefix shipped with the app) is a legal risk and must
  not be part of a DEV/release build. Any "real DLL" approach also has the game's
  own EULA to consider.
* Shipping a whole Wine prefix + Windows engine is a large, hard-to-audit
  distribution surface.

**Performance**
* Window capture/readback: one full GPU→CPU→GPU copy per frame plus compositing
  latency; unacceptable for smooth ADV transitions.
* Cross-process bridge (B2): an RPC per plugin call, and `layerExDraw` is
  per-pixel — the cost is in the hot path.
* ARM: Box64/FEX emulates the plugin's own code; Hangover removes emulation for
  Wine syscalls but not for the plugin's compute. Expect a multiple-x slowdown.

**Maintainability**
* ABI surface is large and grows per game: 89 host entry points for *this* game
  alone, each with hand-written marshalling in any bridge.
* Wine regressions, prefix configuration, `winex11.drv`/WM behaviour, focus
  races, and Wayland gaps are all ongoing operational cost.
* The KrKr2 reference deliberately reimplements plugins from source
  (`reference/cpp/plugins/`) and statically registers them — Wine integration
  would be an off-road fork of both upstream and the port's architecture.
* Debugging spans three runtimes (Rust, C++ TJS2, Wine/Win32) and two ISAs on ARM.

**Correctness**
* `V2Link` plugins assume they can allocate with `TVP_malloc` and free in the
  host, pass host pointers around, and register callbacks. Splitting that across
  processes is not just slow, it is semantically wrong without careful ownership
  rules.

---

## 10. Effort estimates

Scale: **S** < 1 day · **M** ~days · **L** ~1–3 weeks · **XL** months.

| Work item | Effort | Notes |
|---|---|---|
| Verify Wine/WoW64 runs an i386 KiriKiri engine on this box | M | Install Wine + multilib/new-WoW64; supply an engine we do not have |
| X11 reparenting demo (Sketch A) with a dummy Wine window | M–L | Input forwarding + focus are the hard parts; X11-only |
| X11 capture→Bevy texture (readback variant) | L | One frame latency; not for production |
| Implement TVP host exporter (~89 functions, real semantics) | XL | The core of any in-process DLL use; also needed by B1 |
| Port TJS2 C++ VM + engine to i686-pc-windows-gnu / mingw | XL | Second toolchain; MSVC-isms in plugin headers |
| Load the real DLLs in-process (Sketch B1) | XL on top of the above | `LoadLibraryA` + `V2Link` is the *easy* part once the host exists |
| Generic cross-process plugin bridge (B2) | Research (>>XL) | Distributed object graph; not recommended |
| Wine worker for one compute task (F) | M–L | Only worth it for a plugin krkr-rs cannot reimplement |
| Native reimplementation of one plugin | S–M | Current strategy; already done for most of this game's set |

---

## 11. Recommendation

**Do not build on Wine or box64 for this project.**

1. **In-process DLL loading is architecturally impossible** without Wine owning
   the process, because `V2Link` hands the plugin raw host function pointers
   (`PluginImpl.h:28-52`) and the host API (~89 entry points for this game alone)
   is a shared object graph. There is no supported `libwine` embedding API in
   modern Wine, and the process entry (`dlls/ntdll/unix/loader.c:2109`) shows why.
2. **The render surface cannot be cleanly shared.** Wine's display driver is an
   in-process PE that owns its window (`dlls/win32u/driver.c`,
   `dlls/winex11.drv/window.c`); Wine's XEmbed support is systray-only
   (`window.c:2984`). The best you get is X11 reparenting/capture — fragile,
   X11-only, and focus-hostile.
3. **The premise is partly missing anyway.** This package has no Windows `.exe`
   or native plugin `.so`; the Android engine is ARM `libgame.so`, and the real
   DLLs in `data.xp3` are i386 PE that the Android build never loads. The mature
   KrKr2 reference and Kirikiroid2 both reimplement/compile plugins rather than
   load the Windows binaries.
4. **Continue native reimplementation (approach G).** It already handles the
   game's plugin set (`plugins.rs`), and adding `win32dialog` / `getSample` /
   `KAGParser` as built-in natives (as `reference/cpp/plugins/*.cpp` does) is
   orders of magnitude cheaper and more maintainable than any Wine bridge.
5. **If a genuinely Windows-only binary must be used**, isolate it as a
   **narrow, out-of-process Wine worker** (approach F) with a file/shared-memory
   interface — never as a generic in-process ABI bridge, and not as the main
   render path.

On ARM/Android, the same conclusion holds: Wine + Box64/Hangover/Winlator can run
a *Windows* KiriKiri app, but it cannot inject those DLLs into krkr-rs, and this
package does not contain a Windows engine to run.

This recommendation concerns **coupling Wine to krkr-rs**. A *separate* goal —
running arbitrary Windows `.exe`s behind an in-app minimal Wayland compositor —
is evaluated in §12, and the relationship between the two product paths is
summarised in §13.

---

## 12. Minimal Wayland compositor as Wine's display backend (Android + box64)

This is a **different product goal** from §1–§11: not "call the game's DLLs from
krkr-rs", but "run arbitrary **Windows `.exe`s** (x86/x86_64) on Android with
box64, without a desktop, X11, or full DE". The app owns the Android surface
(`ANativeWindow`) and runs a **minimal Wayland compositor** inside itself; Wine
connects to that compositor as an ordinary Wayland client and renders into it.
The `.dll`-calling question is explicitly **secondary / out of scope** here.

### 12.1 What Wine's Wayland driver actually requires

`dlls/winewayland.drv/wayland.c` binds globals in `registry_handle_global`
(`:93`) and then hard-fails in the "required protocol globals" block
(`:325-349`):

| Global | Required? | Source |
|---|---|---|
| `wl_compositor` (v4) | **hard** (init fails) | `wayland.c:116-119`, check `:326` |
| `xdg_wm_base` (v2) | **hard** | `:121-128`, check `:331` |
| `wl_shm` (v1) | **hard** | `:130-132`, check `:336` |
| `wl_subcompositor` | **hard** | `:161-164`, check `:341` |
| `wp_viewporter` (v1) | **hard** | `:156-159`, check `:346` |
| `wl_output` (+ `zxdg_output_manager_v1` v≤3) | needed in practice (screen size/scale) | `:99-114` |
| `wl_seat` (v≤8) | needed in practice (input) | `:134-151`, `seat_listener :83` |
| `zwp_pointer_constraints_v1`, `zwp_relative_pointer_manager_v1`, `zwp_text_input_manager_v3`, `wl_data_device_manager`/`zwlr_data_control_manager_v1`, `xdg_toplevel_icon_manager_v1`, `wp_fractional_scale_manager_v1` | optional (ERR + continue) | `:352-374` |
| `wp_cursor_shape_manager_v1`, `wp_pointer_warp_v1`, `wp_alpha_modifier_v1`, `wl_fixes` | optional, bound if present | `:197-226` |

Notably, **`zwp_linux_dmabuf_v1` is not in Wine's list.** Grepping the whole
driver for `linux_dmabuf`/`zwp_linux` finds nothing. Wine's *own* window buffer
path is **`wl_shm`**: `window_surface.c` defines `struct wayland_shm_buffer`
(`:59`, created at `:135/:159`) and `wayland_surface.c:507` attaches it with
`wl_surface_attach`. Child/owned Win32 windows become **subsurfaces**
(`wayland_surface.c:386-390` `wl_subcompositor_get_subsurface`, `:402`
`wl_subsurface_set_desync`). That is why `wl_subcompositor` is hard-required
even though there is no "desktop".

So the **CPU baseline is `wl_shm`** and is sufficient for GDI-windowed apps
(which includes most visual novels and 2D games).

GPU acceleration is a different story, and it does **not** go through Wine's
shm buffers:

* **OpenGL / D3D-via-wined3d:** `opengl.c` creates a `wl_egl_window`
  (`:51`, `:98` `wl_egl_window_create`) and calls `eglCreateWindowSurface`
  (`:99`) with `EGL_PLATFORM_WAYLAND_KHR` (`:114`). The **native Mesa EGL
driver** (running inside the Wine process) decides how to present — normally
  dmabuf, falling back to shm.
* **Vulkan (native Vulkan games / DXVK / vkd3d):** `vulkan.c:41-56` builds a
  `VkWaylandSurfaceKHR` on Wine's own `process_wayland.wl_display` and the
  window's `wl_surface` (`p_vkCreateWaylandSurfaceKHR`). The **native Vulkan
driver** presents directly to the compositor via Wayland WSI (dmabuf).

Therefore the compositor should advertise `zwp_linux_dmabuf_v1` and import
dmabufs to get accelerated games — but it is **Mesa/the app's GL/Vulkan stack**
that binds it, not `winewayland.drv`.

**Can a Smithay compositor satisfy this?** Yes, for the strict set and most of
the optional set:

* `CompositorState::new` creates **both** `wl_compositor` and `wl_subcompositor`
  in one delegate (`src/wayland/compositor/mod.rs:5-6`, `:689-726`).
* Smithay ships `shm`, `shell::xdg`, `viewporter`, `fractional_scale`,
  `output` (`zxdg_output_manager_v1`), `seat`, `dmabuf` (linux-dmabuf +
  `import_dmabuf` in the GLES renderer), `pointer_constraints`,
  `relative_pointer`, `pointer_warp`, `cursor_shape.rs`, `alpha_modifier`,
  `xdg_toplevel_icon.rs`, `text_input`, `fixes.rs`, and `selection`
  (`wl_data_device_manager`).
* dmabuf import is real: `src/wayland/dmabuf/mod.rs` + `GlesRenderer::import_dmabuf`
  (`backend/renderer/gles/mod.rs:1258`) via EGLImage (`:85`).
* Only `zwlr_data_control_manager_v1` (a wlroots extension) has no Smithay
  equivalent; Wine falls back to `wl_data_device_manager`.

`examples/minimal.rs` in Smithay is already a **client-usable, winit-backed
compositor**: it creates `Display`, `CompositorState`, `ShmState`,
`XdgShellState`, `SeatState` (`:143-154`), binds a real socket
(`ListeningSocket::bind("wayland-5") :162`), sets `WAYLAND_DISPLAY` (`:171`),
and accepts external clients (`insert_client :244`). It is missing only
`wp_viewporter` and an `wl_output`, which are small additions.

**Xwayland / Xvfb fallback.** If a game needs `winex11.drv` (some do), run
**Xwayland** as a client of our compositor and set Wine's `Graphics` driver
accordingly (`HKCU\Software\Wine\Drivers\Graphics = wayland,x11`, the same key
the Hangover README documents). That keeps GPU paths but adds Xwayland
integration (`xwayland_shell`, `xwayland_keyboard_grab` exist in Smithay).
`Xvfb` is the degenerate fallback: no Wayland, but nothing GPU-accelerated -
you would capture an X11 framebuffer on the CPU.

### 12.2 Compositing Wine's buffers into the Android surface

The app owns the `ANativeWindow` (NativeActivity/GameActivity). Two decisions:

**Which renderer composites?** Smithay core ships a **GLES** renderer and a
**pixman** (CPU) renderer; it has **no wgpu renderer**. So:

| Path | Mechanism | Cost / verdict |
|---|---|---|
| **GLES (recommended)** | Create an EGL context/surface on the `ANativeWindow`; Smithay `GlesRenderer`; import client shm and dmabufs (EGLImage) | Best performance; needs an Android EGL bridge (below) |
| **CPU (baseline)** | Smithay `pixman` composites shm buffers; upload to wgpu/GLES texture | Trivial to prove, too slow for real games |
| **wgpu** | Drive `ANativeWindow` with wgpu/Bevy; feed Smithay's client buffers in as wgpu textures | wgpu has no stable dmabuf-import API, so zero-copy is hard; a custom renderer is a sizeable project |

**Android EGL gap.** Smithay's `EGLNativeDisplay`/`EGLNativeSurface` are public
traits (`backend/egl/native.rs:131`, `:284`) but the only impls are GBM, X11, and
Wayland (`:378`, `:401`); there is **no `EGL_PLATFORM_ANDROID_KHR` impl**. Two
ways out: (a) implement the traits for Android
(`eglGetPlatformDisplay(EGL_PLATFORM_ANDROID_KHR, EGL_DEFAULT_DISPLAY, …)` +
`eglCreateWindowSurface(display, config, ANativeWindow*, …)`); or (b) create the
EGL display externally with `khronos-egl`/`ndk` and wrap it with
`EGLDisplay::from_raw` (documented at `backend/egl/display.rs:239-241`). This is
bounded, well-defined work, but it is the first piece Smithay does not give you.

**Zero-copy vs readback.** dmabuf import via EGLImage is true zero-copy on the
GPU; the failure mode is Adreno/Turnip **format+modifier negotiation**
(`zwp_linux_dmabuf_v1` feedback). The safe fallback is `wl_shm` for GDI apps and
a GPU→CPU copy for accelerated ones (acceptable for a prototype, not for 60 fps).

**Input injection.** Android touch/key events arrive in the app; the compositor
injects them as `wl_pointer`/`wl_keyboard`/`wl_touch` on its `wl_seat`. Wine
consumes them via `wayland_pointer.c` / `wayland_keyboard.c`
(`wayland.c:65-86` seat capabilities). This needs an Android-keycode → evdev →
XKB → Windows-VK mapping and pointer-region mapping (touch to the composited
window rect). box64 is not involved in input.

### 12.3 Process and lifecycle

The app is the compositor **and** the launcher:

```
Wayland socket:  $XDG_RUNTIME_DIR/wayland-krkr   (app-private dir, mode 0700)
WINEPREFIX:      <app files>/prefix              (wineboot on first run)
env:             WAYLAND_DISPLAY=wayland-krkr, XDG_RUNTIME_DIR=<private dir>
web:             HKCU\Software\Wine\Drivers\Graphics = wayland   (if autodetect fails)
launch:          box64 wine game.exe   (or `wine game.exe` under Hangover)
```

* No desktop shell, no X server, no DE. Wine creates `xdg_toplevel`s
  (`window.c:254-284`) which the compositor maps fullscreen (or scaled) into the
  `ANativeWindow`; children become subsurfaces.
* Wine still forks its own `wineserver` daemon; the compositor must keep
  `display.dispatch_clients`/`flush_clients` running on its event loop while
  Wine is alive.
* Lifecycle = spawn Wine, wait, tear down the prefix/socket; handle crashes and
  per-game env. This is ordinary process management and is the *easy* part.

### 12.4 What already exists on Android, and what we build

The Wine + box64 + GPU stack is largely productized already:

* **Winlator** assembles Wine + **Box86/Box64** + Mesa **Turnip/Zink/VirGL** +
  **DXVK/VKD3D** + wined3d as downloadable `.tzst` components — its repo has
  `installable_components/box64/box64-0.3.{3,5,7}.tzst`,
  `turnip-24.1.0/25.0.0/26.0.3`, `dxvk-0.96 … 2.3.1`, and
  `wined3d-4.21/7.8/10.0`. It is a **full app + display server** today; our idea
  is to replace its display layer with an in-app minimal Wayland compositor.
* **Hangover** is aarch64 Wine with emulator DLLs: i386 via `wowbox64.dll`
  (Box64, default), `libwow64fex.dll` (FEX), or `wow64cpu.dll` (native i386 on
  x86_64); it breaks out of emulation at the Win32/Wine syscall boundary and has
  Termux packages. It still needs a Wayland (or X11) compositor.
* **box64** is the x86_64-on-ARM64 emulator (`DynaRec` "5–10× faster than the
  interpreter alone"); 32-bit x86 needs box86/box32, or Wine's WOW64 build
  ("experimental"), or Hangover's `wowbox64`.

What **we** would build (the XL part): the minimal Smithay compositor + Android
EGL backend + input bridge + the launcher/prefix-management UI. The Wine, box64,
and GPU drivers are existing artifacts to bundle.

**Performance / compatibility envelope:** x86_64 Windows games are the sweet
spot (box64 + Turnip + DXVK); i386 games are the fragile case (box86 on
AArch64-capable hosts, or experimental WOW64/Hangover); GPU compatibility
depends on the device's Vulkan/GL drivers (Turnip for Adreno, Zink/VirGL
otherwise). Expect a per-game compatibility matrix, not a guarantee.

### 12.5 Smallest first milestone: prove it on Linux x86_64, then port

Do **not** start on Android. Wine runs i386/x86_64 PE natively on x86_64, so the
same compositor can be validated on the desktop with zero emulation:

| Stage | Goal | Works? | Effort |
|---|---|---|---|
| **0** | Take Smithay `examples/minimal.rs`; add `ViewporterState` + one `wl_output` + fullscreen `xdg_toplevel` mapping. Run it with its winit output on Linux x86_64. | Compositor usable by external clients | **S/M** |
| **1** | `WINEPREFIX=$(mktemp -d) WAYLAND_DISPLAY=wayland-5 wine notepad.exe` (or a 2D GDI game). Confirm Wine binds our globals, maps an `xdg_toplevel`, and renders through `wl_shm`. | CPU/GDI render proven | **S** |
| **2** | Add `zwp_linux_dmabuf_v1` (Smithay `DmabufState` + `import_dmabuf`) and `wl_seat` input injection. Run a D3D/Vulkan Windows game; verify GPU presentation. | Accelerated render + input proven | **M/L** |
| **3** | Replace the winit output with "composite into a texture" (Bevy/wgpu or GLES-on-GL) and expose a library API; add touch→`wl_seat`. | The in-app integration shape proven on desktop | **M** |
| **4** | Android: NativeActivity/GameActivity + `ANativeWindow`; implement `EGLNativeDisplay`/`EGLNativeSurface` for Android (or `EGLDisplay::from_raw`); socket in app-private dir. | Wine renders into the Android surface | **L/XL** |
| **5** | Bundle box64 + Wine (+ Turnip/DXVK); bootstrap/repair the prefix; per-game config and a launcher UI. | Shippable game runner | **XL** |

Stage 0–2 are the decisive feasibility test and are cheap. If a stock Wine
game cannot render through a ~300-line Smithay compositor on Linux x86_64, the
Android stage will not rescue it.

### 12.6 Verdict per stage, and the biggest risks

| Stage | Verdict |
|---|---|
| 0 — minimal compositor (Linux) | **Viable** |
| 1 — Wine GDI/`.exe` via `wl_shm` | **Viable** |
| 2 — dmabuf GPU + input | **Viable-with-caveats** (modifier negotiation, seat mapping) |
| 3 — composite into app texture | **Viable-with-caveats** (no wgpu renderer in Smithay; GLES or CPU) |
| 4 — Android EGL + `ANativeWindow` | **Viable-with-caveats** (custom EGL platform; XL-ish) |
| 5 — box64 + Wine packaging | **Viable-with-caveats** (per-game compat, drivers, maintenance) |

Biggest risks:

1. **Android EGL is not a Smithay platform.** The single largest new subsystem;
   dmabuf modifier mismatches on mobile GPUs are a common failure mode.
2. **wgpu is a poor fit for the compositor.** Smithay has no wgpu renderer, and
   wgpu lacks stable dmabuf import, so the attractive "Bevy/wgpu owns everything"
   design needs a custom renderer or an extra GPU→CPU copy. GLES keeps zero-copy.
3. **Compatibility is a long tail.** box64/WOW64 + Turnip + DXVK + per-game
   quirks is a support burden Winlator already carries; we would inherit it.
4. **This is a games launcher, not the VN engine.** Running `.exe`s this way
   conflicts with Android's app model (process lifetime, storage, audio focus,
   permissions) and is a much larger, more fragile product than krkr-rs.
5. **Licensing/distribution.** Wine LGPL-2.1 (bundle obligations), box64 MIT,
   Winlator/Hangover LGPL-2.1; the Windows games themselves are the user's. Same
   caution as §9.
6. **It does not help the DLL problem.** Even with this running, the TVP plugin
   DLLs still execute only inside Wine's process; krkr-rs still cannot call them
   (§3–§4).

---

## 13. Two independent product paths

The §12 Wayland/Wine work and the krkr-rs native port do **not** depend on each
other, and should be decided separately:

* **krkr-rs native reimplementation (approach G)** is the right path for
  *running KiriKiri games with fidelity, performance, and control* — the current
  goal. Keep reimplementing the plugin surface as built-in natives; do not route
  it through Wine. This is where the existing investment already is.
* **Wine + minimal Wayland compositor on Android (§12)** is the right path only
  if the goal changes to *a general Windows-`.exe` runner* for games that are
  **not** KiriKiri ports and have no native reimplementation. It is technically
  promising (the strict protocol set is small, Smithay covers it, and the
  Wine/box64/GPU stack exists) but it is a separate, XL-scale product with its
  own compatibility and distribution burden.

**Recommendation:** continue to invest in the native krkr-rs path; treat the
Wayland-compositor runner as an independent, optional research track. If the
runner is pursued, timebox **Stage 0–2 (Linux x86_64)** first — it is cheap and
it answers the only question that matters: does a stock Wine game render and
accept input through a minimal Smithay compositor? Only then commit to the
Android/box64 packaging. Do not merge the two products into one engine, and keep
"load the game's DLLs in-process" a non-goal in both.

---

## Appendix: commands used (reproducible)

```bash
uname -m                                    # x86_64
which wine wine64 wine32 box64 box86        # none found
bsdtar -tf Kirikiroid2_1.3.9.apk | grep '\.so$'
file apk/lib/arm64-v8a/libgame.so

# plugin binaries inside the archive
python3 scripts/extract_xp3.py data.xp3 extrans.dll > /tmp/dlls/extrans.dll
file /tmp/dlls/*.dll                        # PE32 ... Intel i386

# plugin references in UTF-16LE scripts
python3 /tmp/grep_utf.py 'loadPlugin|Plugins\.link'   # initialize.tjs / sound.tjs / k2compat

# exported entry points
objdump -p /tmp/dlls/extrans.dll | grep -A2 'Export'
# host entry points requested by the plugins (string extraction)
strings -n4 /tmp/dlls/*.dll | grep -oE '::[A-Za-z_][A-Za-z0-9_]*\(' | sort -u | wc -l
```
