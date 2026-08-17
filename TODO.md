# krkr-rs — Plugin & Game Runtime TODO

## Current state (Aug 17, 2026)

**Working**: mount `.xp3` (23,572 + 174 entries) → run `startup.tjs` via the C++
TJS2 VM → `system/Status.tjs` + `Initialize.tjs` + `k2compat/*` + all
`system/*.tjs` load → `begin.tjs` creates `SceneManager` + `Logo` scene →
Bevy window renders the logo/title layers → input bridge dispatches mouse/key
to the game's `onMouseDown`/`onKeyDown` → `WaveSoundBuffer` audio natives
registered (BGM path wired but **not verified audibly**).

**Remaining blockers**: the core startup plugin chain is now emulated by
built-in Rust natives. `Plugins.link(...)` records supported emulated plugins
and ignores optional desktop-only DLLs. The remaining gameplay gaps are
true pixel effects, native popup rendering, and the real-time activation
callback path.

---

## Stage 1 — Plugin registration matrix (DONE)

| Plugin | Game use | krkr-rs status |
|---|---|---|
| `extrans.dll` | unknown/optional | ignored (log only) |
| `csvParser.dll` | `new CSVParser()` (charData.csv) | ✅ real native in tvp-storages |
| `layerExDraw.dll` | layer effects (blur etc.) | ignored |
| `fstat.dll` | `Storages.stat`/file metadata and file operations | ✅ stat/fstat metadata, copyFile/deleteFile landed in tvp-storages |
| `windowEx.dll` | `System.desktop*`, `Window` ex-props | ✅ monitor context + `System.getDisplayMonitors/getMonitorInfo`; OS popup extras remain stubbed |
| `KAGParserEx.dll` | placeholder only | ✅ KAGParser native in tvp-kagparser |
| `getSample.dll` | debug only (`__DEBUGMODE__=0`) | ignored |
| `wuvorbis.dll` | `WaveSoundBuffer` (.ogg) | ✅ real natives in tvp-sound (unverified audibly) |
| `menu.dll` | `MenuItem`, `Window.menu` | ✅ logical headless-safe MenuItem tree + Window.menu fallback; native children object arrays remain ABI-limited |
| `KAGParser.dll` | `ScController extends KAGParser` | ✅ native + script-subclass ctor |

## Stage 2 — What's missing to run the title → ADV flow

Ordered by what the game hits next (all reference sources under
`reference/cpp/plugins/`):

1. **`windowEx.dll` (System.desktop*, screen size)** — ✅ `SystemContext` now
   carries desktop origin/size, the Bevy primary monitor feeds it, and
   `System.getDisplayMonitors/getMonitorInfo` provide the k2compat shape.
2. **`MenuItem` / `Window.menu` (menu.dll)** — ✅ logical state/tree support and
   headless popup behavior are present; native object-valued child arrays and
   OS menu handles remain outside the current ABI/host.
3. **`fstat.dll`** — ✅ `Storages.stat`/`fstat` return disk Date metadata and
   XP3 uncompressed sizes; copy/delete remain available.
4. **`extrans.dll`** — audited in the real game: no `new Trans` usage was found;
   the optional link remains a safe ignored plugin.
5. **`layerExDraw.dll`** — basic text/blur pixel operations now exist on the
   logical layer surface; advanced vector/effect operations remain no-op
   compatibility methods and true GPU blend modes are still pending.

## Stage 3 — Game-runtime milestones (what actually blocks gameplay)

1. **Logo → Title transition** — ✅ timer and transition polling are wired;
   `real_game_timer_loop_advances_scene` now completes successfully after
   deferring continuous-handler re-registration until the current callback
   returns.
2. **Title screen input** — input bridge wired; needs verification that
   clicking `NEW GAME` / `CONTINUE` reaches `SelectItem` → `changeScene`.
3. **`ScController` scenario loop** — `system/ScController.tjs` drives
   `loadScenario("*.ks")` → `getNextTag()` → `onTag()` handlers. KAGParser
   natives + real dicts are in; the ADV scene needs:
   - `Storages.getPlacedPath` / full path semantics for scenario files ✅
   - `Layer.drawText` metrics/rasterization fallback + box blur ✅
   - `System.getKeyState` now mirrors host VK state; cursor bridge is wired;
     layer hit-testing still needs gameplay verification
4. **BGM/SE/voice** — `WaveSoundBuffer` natives in; verify `PlayBgm("BGM02")`
   actually produces audio (rodio output is optional/no-op without a device;
   mixer advances each frame — check the per-frame poll is wired in `run_vm`).
5. **Save/load** — `saveStruct`/`system.dat` eval ✅; the save screens need
   `Storages.getFileList`, file I/O (disk-backed streams ✅), and the
   `savedata/` directory to exist under the game dir.

## Stage 4 — Known open issues (from the integration wave)

- **`real_game_timer_loop_advances_scene`** remains `#[ignore]` because it
  requires the external game fixture, but it now passes when explicitly run
  with `--ignored`.
- **Blend modes** — `layer.type` now reaches the scene and hierarchy sync;
  Bevy's default Sprite pipeline still source-over blends, so true additive /
  subtractive GPU compositing remains pending.
- **Hierarchy flattening** — parent/child order, position, opacity, and
  visibility are now composed depth-first in render sync.
- **`System.screenWidth/Height`** — logical size remains 1280×720 by design;
  desktop monitor origin/size is now supplied by Bevy when available.
- **Audio device** — rodio output only if an ALSA device exists; silent
  otherwise (natives still advance the mixer). Save-data directory creation
  and disk metadata support are now wired for save/load screens.

---

## How to verify each stage

```bash
# headless load (no window): must print "startup.tjs executed successfully"
./target/debug/krkr-vn run "/mnt/DATA/Games/Others/test" --headless

# windowed run: watch logo → title transition, then click NEW GAME
./target/debug/krkr-vn run "/mnt/DATA/Games/Others/test"

# unit + integration (full speed, parallel)
cargo test --workspace            # 395 tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

## Reference material

- `reference/cpp/plugins/` — 35 plugin sources (csvParser, fstat, windowEx,
  KAGParser, layerex_draw, win32dialog, ...)
- `reference/cpp/core/plugin/` — PluginIntf/PluginImpl (the `Plugins` class)
- `system/Initialize.tjs` — the game's plugin link list
- `k2compat/k2compat.tjs` — `delayLoadPlugin` + `requireWindowEx`/`require`
  fallback machinery
