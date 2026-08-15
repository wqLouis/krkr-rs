# Migration Plan — core modules to Rust (+ Bevy)

This document maps the reference engine's C++ core modules and sequences their
migration to Rust. The C++ TJS2 VM stays (vendored, via tjs2-sys); everything
else moves to Rust. Bevy replaces the cocos2d rendering layer.

## Module inventory (reference/cpp/core)

| module | size | deps | Rust status |
|---|---|---|---|
| tjs2 | 37K | fmt/spdlog/boost/oniguruma | C++ (vendored) — stays |
| visual | 64K | cocos2d (Texture/Size/FileUtils), ffmpeg? | **Bevy** (wave 3) |
| base | 11K | tjs2, io, msg, archive, storage, +private visual/sound/plugin/environ | wave 2 (natives) |
| sound | 9.4K | oboe/opensl (android), vorbis/opus | wave 3 (rodio/cpal) |
| utils | 4.2K | tjs2, base, environ | tvp-util (wave 1) |
| environ | 3.2K | cocos2d AppDelegate, ConfigManager, ui | wave 3 |
| plugin | 651 | ncbind | wave 3 |
| io | 176 | tjs2, msg | tvp-streams (wave 1) |
| archive | — | xp3/zip/7z/tar | xp3 done; zip/7z/tar → crates (wave 2) |
| extension/movie/msg/storage/common | small | — | wave 2/3 |

## Native classes (TJS surface games call)

29 classes total. Registered on the VM global object by `registerObject(...)`
in `base/ScriptMgnIntf.cpp`. Critical path for running `startup.tjs`:

- **System** (base/impl/SystemImpl.cpp) — setArgument, inform, ...
- **Storages** (base/impl/StorageImpl.cpp) — mount/read archives from scripts
- **Scripts** (base/ScriptMgnIntf.cpp) — execStorage/evalStorage/compileStorage
- **Debug** (utils/DebugIntf.cpp) — logging
- **KAGParser** (base/KAGParser.cpp) — .ks scenario interpreter
- later: Window/Layer/Bitmap/Font/ImageFunction (visual → Bevy),
  WaveSoundBuffer/CDDASoundBuffer/MIDISoundBuffer/PhaseVocoder (sound),
  Plugins/Pad/Clipboard/Timer/AsyncTrigger/...

## Dependency graph

```
tjs2 (C++ VM) ──┬── tjs2-sys FFI ──┬── tvp-natives (System/Storages/Scripts/Debug/KAGParser) ──┐
xp3 (done) ─────┤                  └── engine (load module, app bootstrap)                    │
kag (.ks parser)┼────────────────────────────────────────────────────────────► engine ────────┴─► Bevy app
tvp-util        ┤
tvp-streams     ┘
```

## Waves

- **Wave 1 (in flight)**: `kag` (.ks parser), `tvp-util` (encodings/md5/random),
  `tvp-streams` (binary/text streams), and the **tjs2-sys FFI extension**
  (native-class registration + value marshaling) — the critical path.
- **Wave 2**: `tvp-natives` crate (System, Storages, Scripts, Debug, KAGParser
  natives in Rust), engine wires natives into `load_game`; goal: real
  `startup.tjs` runs past line 2. Also: zip/7z/tar archive readers, storage
  write support, ConfigManager.
- **Wave 3**: Bevy render module (Window/Layer/Bitmap/Font/ImageFunction from
  `visual/`), audio (`sound/` → rodio/cpal), movie, plugin system (ncbind),
  environ (AppDelegate/ConfigManager/ui) → Bevy app shell.

## FFI design (wave 1)

`crates/tjs2-sys` gets a stable C ABI addition for Rust-implemented native
classes (static methods first):

```c
typedef struct { const char* name; tjs2_native_method_fn fn; } tjs2_native_method;
typedef int (*tjs2_native_method_fn)(void* engine, int argc, const tjs2_value* argv,
                                     tjs2_value* out, char** out_error);
int tjs2_register_native_class(tjs2_engine* e, const char* class_name,
                               const tjs2_native_method* methods, int count);
```

The C++ shim builds a `TJS::tTJSNativeClass` at runtime
(`TJSCreateNativeClassMethod` + `tTJSNativeClass(name)` + `PropSet` on the
global object), marshaling `tTJSVariant` ↔ `tjs2_value`. Rust implements the
methods; the VM stays untouched.
