# `app::game` — the game host

Source: `crates/app/src/game.rs`

`GameHost` is the composition root's counterpart of the empty-scene `Host`:
it adds the L4 VM host, the mounted `Storage`, the L1 audio device and the
ordered frame body, and it runs the startup sequence. It is the object a
launcher builds when the run has a game.

---

## 1. State and lifetime

```rust
pub struct GameHost {
    audio: AudioOutput,     // the L1 device, dropped first
    vm: VmHost,             // the L4 host, dropped before its targets
    storage: Box<Storage>,  // the mounted archives
    host: Box<Host>,        // the L2 engine host
    polls: Polls,           // the ordered frame body and its cost record
    open_start: Instant,
    last_frame: FrameAttribution,
    first_frame_logged: bool,
}
```

The **field order is load-bearing**. Rust drops fields in declaration order,
and three of these values are tied together by pointers:

* `audio` is declared first, so the L1 device is stopped — its state drop
  stops the stream — **before** the VM dies;
* `vm` is declared before `storage` and `host`, because the VM holds raw
  pointers to the `Storage` and to the `Scene`, `TimerState` and
  `InputState` inside the engine: the VM must go while those are still
  alive;
* `host` is **boxed**, so the engine's address — and with it the scene, the
  timer registry and the input state — is stable across the move into the
  struct.

The public accessors are `host()`, `host_mut()`, `storage()`, `scene()` and
`vm()`.

---

## 2. Opening a game

`GameHost::open(host, archives, base_dir)` is
`open_with_preload(..., preload_archives = false)`. The open sequence is
ordered, and every step of it matters:

1. **The audio device.** `AudioOutput::open(host.is_headless())` runs first:
   a windowed host opens the real device and fails fast when there is none,
   a headless host opens the null backend. See [audio.md](audio.md).
2. **The file system and the mount.** `FileSystem::host(base_dir)` is the
   root. An explicit archive list is mounted in order (later wins); an empty
   list means a **directory launch**, and the root then discovers every
   `*.xp3` under the base directory and mounts the discovered list. The
   discovered order and the precedence are L2's contract.
3. **The project path and the project directory.**
   `storage.set_project(...)` records the project path chosen from the mount
   input, and `storage.set_app_dir(...)` records the **project directory** —
   the folder the project archive lives in. The disk namespace and the
   app-path scripts are rooted there, so a launch whose base directory is the
   *parent* of the game folder still reaches the game's own loose files —
   see §3.
4. **The L2 service attachment.** The scene, the storage, the timer registry
   and the input state are handed to the compat surface *before* the
   surface is installed. The order is not cosmetic: a plugin may read a
   service while it registers (the `xp3filter` plugin reads
   `xp3filter.tjs` through the storage), so the handle must already be
   there when `CompatHost::install` runs.
5. **The L1 seams.** The image codec, the audio decoder and the video
   source are installed, the system font directories are supplied, the
   emulated Windows host facts are installed and the inert clipboard
   backend is set — see §4.
6. **The `.tpm` plugins.** The reference auto-loads the plugin manifests
   before the startup scripts run. The root enumerates them with the L1 file
   system and compat links the ordered names — see §4.
7. **The startup sequence.** The plan is resolved, the deferred half of the
   plugin registration is completed, and the scripts run — see §5.

The mount and the startup are timed, and the totals are logged once at
`info`: the archive count, the summed index entries and archive bytes with
the mount time, the decoded-image count with its time, the audio and video
decoders opened, and the startup script count with its time.

---

## 3. The project path

The reference takes its project path from the launcher and only decides
whether it is an archive or a folder. The root reconstructs it:

| launch | project path |
|---|---|
| directory (D35 scan, loose or archive-only) | the conventional `data.xp3` when it mounted, else the scanned archive with the boot scripts and the most entries, else the directory (folder project) |
| an explicit `--archive` list | the named archive with the boot scripts and the most entries, else the first named archive (archive project) |

The "boot scripts" are `startup.tjs` and `system/Initialize.tjs` (an archive
entry lookup is case-insensitive, so either spelling finds the same entry).
The conventional `data.xp3` is matched without regard to case as well, and
only when that archive mounted: an archive that failed to mount is skipped as
a project candidate; the app cannot use it as the project archive, and the
next candidate is used. The project archive is searched first, so a later
`patch.xp3` overrides an earlier `data.xp3` only when `data.xp3` is not the
project.

Several archives can hold the boot scripts: a game ships the archive it is
played from and a patch archive, and the patch carries a copy of
`startup.tjs`. The reference takes the project from its launcher — the
command-line archive, or the archive the file selector picks
(`reference/cpp/core/environ/win32/Platform.cpp:89-104`,
`reference/cpp/core/environ/ui/MainFileSelectorForm.cpp:135-145`); a
directory launch has no user, so the root picks the data archive — the
boot-script archive with the most entries. The patch holds the changed files
only and the game registers it itself as an auto path
(`Storages.addAutoPath("patch.xp3>")`), so the patch's files are reached
through the auto path and not through the project, while the data archive's
folders (`system/`, `scenario/`, `rule/`, …) are the ones the game's relative
auto paths name.

A relative `Storages.addAutoPath("system/")` resolves against the project
path, so it scopes to the project archive when the launch names one and to
the project directory when the project is the folder.

### The project directory

The project directory is the folder the project archive lives in; the base
directory stays the mount root the archives are discovered under.

| launch | project directory |
|---|---|
| directory (D35 scan) | the parent of the project archive — the game folder, even when the base directory is its parent; the base directory itself when the project is the folder |
| an explicit `--archive` list | the base directory: the command line named it, and it is the parent of the first named archive when the command line named none |

The reference's application path is the project directory
(`reference/cpp/core/base/impl/StorageImpl.cpp:503-509`), and the project
directory is the launch path
(`reference/cpp/core/environ/Application.cpp:319`,
`reference/cpp/core/base/impl/SysInitImpl.cpp:160-165`). The script-visible
system paths (`System.exePath`, `System.dataPath`, `System.appDataPath`,
`System.personalPath`, `System.savedGamesPath`) follow it.

---

## 4. The installed seams

The compat surface never links a codec and never sees a device. It exposes
the seams and expects the composition root to fill them:

| seam | installed by | what it provides |
|---|---|---|
| image codec | `image_codec.rs` | `Bitmap.load` / `loadAsync` / `loadHeader` / `save` |
| audio decoder | `audio.rs` | the `add_audio_decoder` chain, so a sound buffer can decode and play |
| video source | `video.rs` | the `VideoOverlay` and `LayerExMovie` seams, over the L1 decoder and the L2 storage |
| system font directories | `platform::system_font_dirs()` | the host's font directories, supplied before a startup script can resolve a face |
| Windows host facts | `window_ex.rs` | the monitors, the desktop rectangle, the system metrics, the cursor, the environment |
| clipboard | an inert backend | `hasFormat` is false, `asText` reads void, a write is a no-op — the reference clipboard does nothing |

The **image codec** decodes and probes through the stateless L1 backend. The
save path encodes through the same backend and writes the bytes through the
storage's file system at the storage's game directory, so a later read sees
the file. A saved name may contain sub-directories; missing parents are
created. The write resolves the name through the storage's case-correcting
helper, so the fully lowercased absolute form `System.dataPath` produces
lands on the real mixed-case path instead of creating a second lowercase
tree. An unknown save type is a typed encoder error surfaced to the script,
never a silently skipped write.

The **`.tpm` plugin scan** keeps the regular files whose name ends in
`.tpm` (case-insensitively) from the app directory, `<app>/system` and
`<app>/plugin`, sorted by ASCII file name. A missing or non-directory entry
contributes nothing; any other listing error propagates. The scan is the
root's job because compat owns no file-system access; the reference
behaviour it follows is recorded in the
[plugin evidence trail](../../../docs.old/migration/08-tjs-vm-and-native-plugins.md).

---

## 5. The startup sequence

The plan is resolved by L2 (`storage.startup_plan()`) and executed here, in
the reference order:

1. **`patch.tjs`** — project directory only, no search. A failure is
   reported and startup continues.
2. **`startup.tjs`** — searched across the storage. When it fails and the
   plan carries a `system/Initialize.tjs` fallback, the fallback runs; when
   it fails and there is no fallback, the open fails with
   `GameError::Startup`.
3. **`AfterStartup.tjs`** — project directory only. A failure is reported
   and startup continues.

When neither a startup script nor a fallback exists, the open fails with
`GameError::NoStartup`. There is no warn-and-continue path: a game that
cannot boot is an error the caller sees.

The deferred half of the plugin registration is completed *before* the
scripts run, so a plugin that needs an L2 service finds it installed rather
than running unfiltered.

---

## 6. The frame hook

`VmHook` implements the L2 `GameFrame` trait — the single upward edge of the
frame. The engine fires the due timers, then calls `before_render`, then
projects and draws.

### `before_render(now_ms, dt_ms)`

* the frame's `dt` is handed to the frame body;
* the movie and the video step are bounded by the audio pump's playback cap,
  so a long frame leaves the picture and the sound equally far behind instead
  of pulling them apart. The step itself is the engine clock's elapsed time,
  which the compatibility layer reads from the clock the composition root
  attached;
* the position-carrying events are **mapped from device pixels into VM
  pixels** through the L2 translation. Only the pointer position and a
  touch contact carry a coordinate; buttons, wheels, keys, text, focus and
  resize mean the same in both spaces and pass through unchanged, and an
  identity translation is skipped entirely;
* the VM tick runs: the input frame is cleared, the events are adapted and
  dispatched, the frame is committed, and the ordered polls run;
* the audio drain runs immediately after the tick.

The mapping is the root's job precisely because L3 never sees the
translation: an L3 hit-test stays in VM space, and the L3 code keeps its
VM-coordinate contract. See [`../rules/arch_rules.md`](../../rules/arch_rules.md) §5.

### `after_project(drawn_layers)`

The self-updating transitions advance on the projection's own draw report,
and the script-cached layers are refreshed, so a transition that is never
updated by a script still advances when the projection draws it.

### The frame body

`Polls` implements the L3 `FrameBody` seam. The order is enforced by
`compat::run_frame_body`, not by the implementor:

```
drain the onTimer posts the engine already fired
  → sound_poll      → compat::sound_poll
  → video_poll      → compat::video_advance(audio cap)
  → async_trigger_poll
  → continuous_handler_poll
  → idle compaction
  → frame_tail      → movie poll, the transition step, the cached layers,
                      then the bring-to-front / update / capture-release drains
```

The transition step runs inside the frame tail, before the engine uploads
and projects, so a cache that holds a transition composites the frame the
transition just blended.

---

## 7. Slow-frame attribution

The root measures the frame and, when the gap since the previous frame
reaches `SLOW_FRAME_MS` (250 ms), logs one line that names the **previous**
frame — the gap spans it, so its phases and host costs are the ones that
explain the gap.

The line carries: whether the frame it spans reached the surface
(`presented`), so a skipped presentation shows up as `presented=false`
with the wait attributed to the GPU `acquire` instead of the VM; the five
poll phases and their sum; the VM tick and its three input segments, plus the
tick residual; the engine frame's sync, upload, prune, project and render
costs, the GPU acquire/present/submit/fence costs, and the host residual; the
audio drain; and the total. The current step's `platform_wait` and `input`
come from the platform loop's own pacing measurement, and `unaccounted`
closes the budget.

A fast frame emits nothing, and the very first frame emits nothing: there
is no predecessor to attribute.

---

## 8. Failure modes

| error | raised when |
|---|---|
| `GameError::Mount` | an explicitly named archive could not be mounted |
| `GameError::Storage` | a storage probe failed while resolving the startup sequence |
| `GameError::Startup { script, error }` | the startup script failed and no fallback exists |
| `GameError::NoStartup` | no `startup.tjs` and no `system/Initialize.tjs` fallback were found |
| `GameError::Host` | a render action failed |
| `GameError::Audio` | the L1 audio device could not be opened or written |
| `GameError::Vm` | the VM could not start, or a frame body failed |
| `GameError::Plugin` | auto-loading a `.tpm` plugin failed |

A **scanned** archive that cannot be read is skipped with a warning — one
stray file in a game folder cannot stop an otherwise playable mount — while
an **explicitly named** archive is a typed error.

---

## 9. Tests

The module's unit tests cover the slow-frame line (a fast gap emits
nothing, the first frame emits nothing, the line names the previous frame
and balances the gap, and a later frame replaces the attribution), the
`.tpm` scan (regular files only, from the three directories, in ASCII name
order, with a missing directory tolerated), the project-path choice (the
`directory_project` and `explicit_project` cases: the conventional `data.xp3`,
its case-insensitive spelling, an unmounted `data.xp3` skipped for the
boot-script archive, the boot-script archive, the data archive over a
patch archive, the folder fallback, an
unmounted `data.xp3` leaving the folder as the project, the explicit named
archive, the explicit boot-script archive, the explicit data archive over a
patch archive, and the explicit first named
archive), the input mapping (a pointer and a touch mapped into VM pixels, a
key passed through), and the service-attachment order (the `xp3filter` seam is
installed by `GameHost::open`).

The project-path tests that drive a real launch live in
`crates/app/src/game.rs`: `boot_script_entries_reads_the_mounted_archives`
mounts real XP3 bytes and reads the boot scripts out of them (`startup.tjs`,
`system/Initialize.tjs`, neither, and a relative archive path);
`directory_launch_skips_an_unmountable_data_archive` opens a game whose
`data.xp3` is not an archive, so the archive fails to mount, the folder stays
the project, and the loose `startup.tjs` next to it still runs; and
`directory_launch_uses_the_boot_archive_as_the_project` opens a game whose only
archive holds the boot script, and checks that the relative name resolves
inside that project archive rather than the loose file beside it.

The unit test `step_runs_the_game_hook_inside_the_engine_frame` in
`crates/app/src/game.rs` drives the production path: a script installs a
timer, and the counter only advances because the engine frame ran the
host's hook.

---

## 10. Related documents

| topic | document |
|---|---|
| the crate overview and the wiring | [README.md](README.md) |
| the CLI and the entry point | [main.md](main.md) |
| the audio seam and the device | [audio.md](audio.md) |
| the video seam | [video.md](video.md) |
| the L3 ↔ L2 seam the root attaches through | [`../rules/api_rules.md`](../../rules/api_rules.md) |
| the frame contract and the hook | [`../../migration/12-layer-tree-and-frame.md`](../../../docs.old/migration/12-layer-tree-and-frame.md) |
| game loading and startup discovery | [`../../migration/09-game-loading.md`](../../../docs.old/migration/09-game-loading.md) |
| the frame body and the plugin registry | [`../../migration/08-tjs-vm-and-native-plugins.md`](../../../docs.old/migration/08-tjs-vm-and-native-plugins.md) §4, §6 |
