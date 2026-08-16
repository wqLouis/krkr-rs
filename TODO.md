# krkr-rs — Plugin & Game Runtime TODO

## Current state (Aug 17, 2026)

**Working**: mount `.xp3` (23,572 + 174 entries) → run `startup.tjs` via the C++
TJS2 VM → `system/Status.tjs` + `Initialize.tjs` + `k2compat/*` + all
`system/*.tjs` load → `begin.tjs` creates `SceneManager` + `Logo` scene →
Bevy window renders the logo/title layers → input bridge dispatches mouse/key
to the game's `onMouseDown`/`onKeyDown` → `WaveSoundBuffer` audio natives
registered (BGM path wired but **not verified audibly**).

**Blocked on**: the game's plugin chain. `Plugins.link(...)` currently **logs
and ignores** every plugin, and k2compat's `delayLoadPlugin` fallbacks depend
on the plugin-provided classes. Several plugin classes are no-op stubs
(`MenuItem`, `ChainItemBase`, `InputNotifyBase`, `VideoOverlay`, ...); others
are missing entirely (`CSVParser` exists but `extrans`/`fstat`/`windowEx`
surface is absent).

---

## Stage 1 — Plugin registration matrix (DONE)

| Plugin | Game use | krkr-rs status |
|---|---|---|
| `extrans.dll` | unknown/optional | ignored (log only) |
| `csvParser.dll` | `new CSVParser()` (charData.csv) | ✅ real native in tvp-storages |
| `layerExDraw.dll` | layer effects (blur etc.) | ignored |
| `fstat.dll` | `Storages.deleteFile`/`copyFile` | ✅ copyFile/deleteFile landed in tvp-storages |
| `windowEx.dll` | `System.desktop*`, `Window` ex-props | ❌ **missing** — k2compat requires it via `Krkr2CompatUtils.requireWindowEx()` |
| `KAGParserEx.dll` | placeholder only | ✅ KAGParser native in tvp-kagparser |
| `getSample.dll` | debug only (`__DEBUGMODE__=0`) | ignored |
| `wuvorbis.dll` | `WaveSoundBuffer` (.ogg) | ✅ real natives in tvp-sound (unverified audibly) |
| `menu.dll` | `MenuItem`, `Window.menu` | ⚠️ stub class `MenuItem` in tvp-natives |
| `KAGParser.dll` | `ScController extends KAGParser` | ✅ native + script-subclass ctor |

## Stage 2 — What's missing to run the title → ADV flow

Ordered by what the game hits next (all reference sources under
`reference/cpp/plugins/`):

1. **`windowEx.dll` (System.desktop*, screen size)** — `system/window.tjs` +
   `k2compat_deskinfo.tjs` read `System.desktopLeft/Top/Width/Height` and
   `System.screenWidth/Height` for window placement & fullscreen. Our
   `System` returns hardcoded 1280×720; k2compat's `requireWindowEx()` sets
   `K2COMPAT_SPEC_DESKTOPINFO`. Reference: `reference/cpp/plugins/windowEx.cpp`.
   → wire real monitor size (bevy `Window` / winit) into `System` props.
2. **`MenuItem` / `Window.menu` (menu.dll)** — k2compat registers a
   `delayLoadPlugin("menu.dll", ...)`; when clicked the title/config screens
   can open context menus. Stub exists; decide stub-vs-real.
3. **`fstat.dll`** — `Storages.stat()` returns file metadata (size/mtime) for
   the save/load list (`save/load` screens call it). Currently `stat` returns
   pending/error. Reference: `reference/cpp/plugins/fstat/main.cpp`.
4. **`extrans.dll`** — `Trans`/splash utilities; the game links it but may
   never call it (audit: no `new Trans` in game scripts → confirm, then drop
   or stub).
5. **`layerExDraw.dll`** — `Layer` blur/glow effects used by ADV effects
   (`EnvEffect.tjs`). Milestone-sized (pixel ops) — defer to the visual wave.

## Stage 3 — Game-runtime milestones (what actually blocks gameplay)

1. **Logo → Title transition** — Timer/continuous-handler rewrite landed; the
   logo's 13s sequence + `changeScene(SCENE_TITLE)` needs a **real-time
   headless verification** (the `real_game_timer_loop_advances_scene` test
   currently hangs — see below).
2. **Title screen input** — input bridge wired; needs verification that
   clicking `NEW GAME` / `CONTINUE` reaches `SelectItem` → `changeScene`.
3. **`ScController` scenario loop** — `system/ScController.tjs` drives
   `loadScenario("*.ks")` → `getNextTag()` → `onTag()` handlers. KAGParser
   natives + real dicts are in; the ADV scene needs:
   - `Storages.getPlacedPath` / full path semantics for scenario files ✅ mostly
   - `Layer.drawText` + font glyphs (message area) — **text rendering** (tvp-text)
   - `System.getKeyState` / cursor / `HitTest` for click-through
4. **BGM/SE/voice** — `WaveSoundBuffer` natives in; verify `PlayBgm("BGM02")`
   actually produces audio (rodio output is optional/no-op without a device;
   mixer advances each frame — check the per-frame poll is wired in `run_vm`).
5. **Save/load** — `saveStruct`/`system.dat` eval ✅; the save screens need
   `Storages.getFileList`, file I/O (disk-backed streams ✅), and the
   `savedata/` directory to exist under the game dir.

## Stage 4 — Known open issues (from the integration wave)

- **`real_game_timer_loop_advances_scene` test hangs** (render crate,
  `#[ignore]`d) — the logo scene never closes in the headless drive loop.
  Root-cause candidate: `beginActivation`/`setTransitionCompleteCall`
  chain needs `AsyncTrigger` idle-flush ordering vs `continuous_handler_poll`
  in the same frame (the scene *does* progress in the windowed run — verify).
- **Blend modes unimplemented** — `layer.type` (ltAdditive etc.) never reaches
  the scene; everything alpha-blends. Affects title COVER fade + ADV flashes.
- **Hierarchy flattening** — parent fills draw in FRONT of their children
  (FFI object-parent resolution pending); the logo white card covers its art.
- **`System.screenWidth/Height` hardcoded** to 1280×720 (Stage 2.1).
- **Audio device** — rodio output only if an ALSA device exists; silent
  otherwise (natives still advance the mixer).

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
