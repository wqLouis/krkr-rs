# Audit — `VideoOverlay`

Read-only behavioural audit of the `VideoOverlay` native class. No source files
were changed. The oracle is `reference/cpp/**`; the relevant in-tree platform
backend is the FFmpeg one (`reference/cpp/core/movie/ffmpeg/KRMoviePlayer.*`,
`KRMovieLayer.*`), which is what backs the Windows reference on this machine.

Actor map:

| role | file |
|---|---|
| TJS class / member registration | `reference/cpp/core/visual/VideoOvlIntf.cpp` |
| TJS base instance, status + events | `reference/cpp/core/visual/VideoOvlIntf.cpp:28-180` |
| wrapper / status machine | `reference/cpp/core/visual/impl/VideoOvlImpl.{h,cpp}` |
| backend interface | `reference/cpp/core/visual/impl/krmovie.h` |
| backend (overlay/mixer/MFEVR) | `reference/cpp/core/movie/ffmpeg/KRMoviePlayer.{h,cpp}` |
| backend (layer mode) | `reference/cpp/core/movie/ffmpeg/KRMovieLayer.cpp` |
| gamut of modes | `reference/cpp/core/visual/voMode.h` |
| port dispatch/compat | `crates/compat/src/natives/video.rs` |
| port playback model (L2) | `crates/engine/src/video.rs` |
| port decoder seam (app) | `crates/app/src/video.rs` |

Scripts that drive it in the real game (extracted from
`/mnt/DATA/Games/Others/test/data.xp3`):

* `system/movie.tjs` — `MovieLayer extends VideoOverlay`, `mode = vomLayer`,
  `layer1 = <Layer>`, `open/play/pause/stop`, `numberOfAudioStream`,
  `audioVolume`, `onStatusChanged`.
* `system/enveffect.tjs:648-860` — `new VideoOverlay(win)`, `mode = vomLayer`,
  `layer1 = _screen`, `frame = …`, `setSegmentLoop/cancelSegmentLoop`,
  `loop`, `audioVolume`, `setPeriodEvent/cancelPeriodEvent`, `play/pause/stop`,
  `onStatusChanged`/`onPeriod`, `originalWidth`/`originalHeight`.

Surface parity is already green: `docs/native_parity.md:40` reports
`VideoOverlay` 69/69 reference members registered. All findings below are
**behavioural**, not surface gaps.

> The previous fix (`ddd7408`, "leave an idle overlay inert") removed a
> port-invented `NotOpen` for `play/pause/stop/prepare` and corrected the
> closed-state property sentinels. Those are now correct; this audit covers
> what remains and re-checks the "closed" behaviour against the reference.

---

## 0. Executive summary

The port is a faithful *shape* of the class but it drives its own L2 state
machine instead of a decoder backend. The largest divergences are not the
"backend passthrough" fields (those are inherently seam-limited) but four
places where the port invents semantics the reference does not have and where a
real script can observe it:

| # | severity | divergence |
|---|---|---|
| S1 | **high** | `setSegmentLoop` raises a port-invented `InvalidSegment`; the reference never validates and accepts `(-1, -1)`; the port also requires `loop = true` to actually wrap |
| S2 | **high** | `prepare()` semantics are rewritten: reference is `vomLayer`-only, does Pause→Rewind→Play and fires `perPrepare` on the first decoded frame; port emits `perPrepare` immediately for any mode and never changes status |
| S3 | **high** | status events: the reference fires `onStatusChanged` **synchronously** from `open`/`close`/`play`/`pause`/`stop`; the port fires no event on `open`/`close` and defers `play`/`pause`/`stop` to the next frame |
| S4 | **medium** | playback end: reference `Stop()` rewinds to frame 0, so `position` reads 0 after a finished movie; port leaves `position` at the duration |
| S5 | **medium** | `mode` is not frozen after `open` in the port (reference ignores the write once a backend exists) |
| S6 | **medium** | `playRate` semantics differ: reference reports the backend speed (0 while open-but-not-played) and accepts any value; port reports `1.0` and raises `InvalidPlayRate` |
| S7 | **medium** | period event machine: set-at-or-before-current-frame, re-arming on `position`/`frame` seeks, and `periodEventFrame` reading after a fire all differ |
| S8 | **medium** | colour-adjustment and mixing-metadata values are fabricated; the FFmpeg reference returns `-1.0` for every colour member and `1.0`/`0xFF000000` for the mixing members |
| S9 | **medium** | `vomLayer` rectangle/visibility semantics: reference `setPos/setLeft/setTop` move `layer1`/`layer2` and `setSize/setWidth/setHeight/setBounds` are no-ops; port mutates its own bounds and never touches the layers |
| S10 | **medium** | constructor accepts `null`/no argument; the reference requires a live `Window` (raises `TVPSpecifyWindow`/conversion error/bad-param-count) and registers the overlay with it |
| S11 | **medium** | `vomMixer` layer opacity is ignored in the composite (`blit_over(..., 255, …)`), while the reference passes `opacity/255` |
| S12 | **medium** | mode family: port composites `vomLayer` and `vomMixer`; the FFmpeg reference composites only `vomLayer` (mixer `PresentVideoImage`/`SetMixingBitmap` are stubs) and renders `vomOverlay`/`vomMFEVR` through a window node the port cannot reach |
| S13 | low | `vomMFEVR` must suppress `SetStatus` on play/stop/pause (reference `if(Mode != vomMFEVR)`); port sets status for all modes |
| S14 | low | `originalHeight` on a closed overlay is an unguarded backend call in the reference (UB); port returns 0 |
| S15 | low | `open`/`close` reset or fail to reset various wrapper fields the reference retains (segment/period retained by reference; backend passthrough fields reset by reference) |
| S16 | low | `onStatusChanged`/`onCallbackCommand`/`onPeriod`/`onFrameUpdate` native defaults forward to the owning `Window` in the reference; the port's are inert |
| S17 | low | a segment wrap queues an extra `onFrameUpdate` in the port; the reference `return`s before the update |
| S18 | seam | audio is decoded nowhere: `audioVolume`/`audioBalance`/stream selection are stored numbers; `numberOfAudioStream` is 0/1 |
| S19 | seam | `ready` (`ssReady`) status cannot be represented (`PlayStatus` has 4 states) |
| S20 | seam | `vomOverlay`/`vomMFEVR` have no windowed surface; `setPos/setSize/setBounds` do not go through the window zoom path (`SetRectangleToVideoOverlay`) |

Details, with citations, follow.

---

## 1. Member-by-member table

Legend: **same** = matches the in-tree reference backend; **diff** = behaviour
differs; **seam** = a difference forced by the L3↔host seam (no decoder/mixer/
audio); **n/a** = reference is UB/unreachable on this backend.

### 1.1 Methods

| member | reference behaviour | port behaviour | verdict |
|---|---|---|---|
| `open(file)` | `Close()` first; requires `Window`; strips `?param`; opens storage; builds backend by `Mode` (bitmaps in layer mode); `SetStatus(ssStop)` fires `onStatusChanged("stop")`; caches file/mode; `SetLoop` applies `(0,-1)`. Errors: storage, invalid video size. | asks `VideoSource::spec`; raises a generic error if absent; `VideoState` sets status to `Stop` directly with **no event** (`engine/src/video.rs:336`); resets frame/segment/period; no `?param`, no window check, no backend cache. | **diff** (§3.1, §3.5) |
| `play()` | if backend: `Play()`; clear graph msgs; `SetStatus(ssPlay)` **immediately** (`VideoOvlImpl.cpp:308-316`). | if open: L2 `play()`; status event queued, dispatched next `video_advance` (`video.rs:694-713`, `engine/video.rs:383-397`). | **diff** timing |
| `stop()` | if backend: `Stop()` (seek 0); `SetStatus(ssStop)` immediately (`:318-326`). | if open: L2 `stop()` sets position 0 + status; event deferred. | **diff** timing |
| `pause()` | if backend: `Pause()`; `SetStatus(ssPause)` immediately (`:328-335`). | if open: L2 `pause()`; event deferred. | **diff** timing |
| `close()` | releases/pauses backend, `SetStatus(ssUnload)` immediately, deletes bitmaps (`:248-273`). | L2 `close()` sets status `Unload` directly, **no event** (`engine/video.rs:342-350`). | **diff** (§3.1) |
| `setPos(l,t)` | `vomLayer`: `Layer1/Layer2->SetPosition`; else `Rect.set_offsets` + backend `SetRect` (`:392-402`). | always writes `overlay.bounds`; never moves the layers; no backend rect. | **diff** (§3.6) |
| `setSize(w,h)` | `vomLayer`: **return (no-op)**; else `Rect.set_size` + backend (`:404-410`). | always writes `overlay.bounds` (`video.rs:819`). | **diff** (§3.6) |
| `setBounds(l,t,w,h)` | `vomLayer`: no-op; else `Rect = rect` + backend (`:412-418`). | always writes `overlay.bounds` (`video.rs:843`). | **diff** (§3.6) |
| `rewind()` | backend `Rewind`; if `Status==ssPlay` backend `Play`; clear msgs; maybe clear `IsEventPast` (`:337-347`). | position 0 + re-arm period (`engine/video.rs:425-430`). | **same** on the playing path; period nuance §3.4 |
| `prepare()` | `vomLayer` only: `Pause();Rewind();IsPrepare=true;Play()`; `perPrepare` fires later on the first `EC_UPDATE`, then `Pause();Rewind()` (`:349-355`, `:635-640`). | `require_open`; queues `Period(Prepare)` immediately; no mode gate, no status change, no pause/rewind/play (`engine/video.rs:435-441`). | **diff** (§3.3) |
| `setSegmentLoop(a,b)` | stores `SegLoopStartFrame/EndFrame`; backend `SetLoopSegement`; **no validation**, accepts `-1` (`:357-361`). | stores; validates when open (`a<0`, `b<a`, `b>=total`) and raises `InvalidSegment` (`engine/video.rs:527-537`). | **diff** (§3.2) |
| `cancelSegmentLoop()` | stores `(-1,-1)` (`VideoOvlImpl.h:80-83`). | stores `(-1,-1)` (`engine/video.rs:540-543`). | **same** |
| `setPeriodEvent([f])` | 0 args → `-1`; 1 arg → `f`; 2+ → nothing; `IsEventPast = f <= GetFrame()` (`VideoOvlImpl.cpp:363-370`). | 0 args → `-1`; 1 arg → stored + `period_fired=false`; 2+ → nothing (`video.rs:919-947`). | **diff** (§3.4) |
| `cancelPeriodEvent()` | `EventFrame = -1` (`VideoOvlIntf.cpp:363-369`). | `period_event_frame=-1` (`engine/video.rs:563-566`). | **same** |
| `selectAudioStream(n)` | requires ≥1 arg (`TJS_E_BADPARAMCOUNT`); backend select (`VideoOvlIntf.cpp:369-380`). | 0 args silently selects `0`; stores a number; no backend (`video.rs:1047-1060`). | **diff** (§3.7) |
| `setMixingLayer(l)` | requires ≥1; `null`/absent → reset; non-Layer raises `TVPSpecifyLayer`; visible layer → `SetMixingBitmap(main_image, opacity/255)` (`VideoOvlIntf.cpp:382-402`, `VideoOvlImpl.cpp:882-913`). | requires 1; same resolution; stores bind + alpha; no mixer; alpha later unused (`video.rs:965-1028`). | **diff** (§3.8) |
| `resetMixingLayer()` | backend `ResetMixingBitmap` (`VideoOvlImpl.cpp:915-919`). | clears binding (preserving window) (`video.rs:1030-1044`). | **same** shape |
| `onStatusChanged(status)` | invokes the **window** action owner's `onStatusChanged` named arg `status` (`VideoOvlIntf.cpp:407-417`). | inert native hook; events are delivered by `call_member` on the overlay (`video.rs:1068-1079`, `1830-1862`). | **diff** (§3.5) |
| `onCallbackCommand(cmd,arg)` | window action owner (`VideoOvlIntf.cpp:419-430`). | inert; never fired (no host callback source). | **seam** |
| `onPeriod(reason)` | window action owner; `perLoop/perPeriod/perPrepare/perSegLoop` = 0/1/2/3 (`VideoOvlIntf.cpp:432-443`). | inert native hook; mapper sends numeric `reason.code()`; codes match (`video.rs:1755-1794`). | **diff** default only |
| `onFrameUpdate(frame)` | window action owner (`VideoOvlIntf.cpp:445-456`). | inert hook; mapper sends frame number. | **diff** default only |

### 1.2 Rectangle / visibility / playback properties

| member | reference | port | verdict |
|---|---|---|---|
| `position` get/set | backend time; closed → `0`. Setter does not touch `IsEventPast`. | open → `position_ms`, closed → `0`; setter re-arms the period event (`video.rs:1271-1305`, `engine/video.rs:495-500`). | **diff** (§3.4) |
| `left`/`top` get/set | reads `Rect`; in `vomLayer` setter moves `Layer1/Layer2`, not `Rect`; closed reads defaults 0. | always reads/writes `overlay.bounds` (`video.rs:1139-1195`). | **diff** (§3.6) |
| `width`/`height` get/set | reads `Rect`; in `vomLayer` setter is a **no-op**; default 320/240. | always reads/writes `overlay.bounds` (`video.rs:1197-1253`). | **diff** (§3.6) |
| `originalWidth` | closed → `0` (`VideoOvlImpl.cpp:1114-1123`); else backend video width. | `spec.width` or `0` (`video.rs:1518-1532`). | **same** |
| `originalHeight` | **unguarded** backend call; closed → UB/null-deref (`VideoOvlImpl.cpp:1125-1133`). | `spec.height` or `0` (`video.rs:1534-1548`). | **diff** (reference bug) |
| `visible` get/set | store; in `vomLayer` propagates to `Layer1/Layer2`; else backend (`VideoOvlImpl.cpp:460-475`). | store + `binding.visible`; never propagates to the layer (`engine/video.rs:574-578`, `video.rs:1255-1269`). | **diff** (§3.6) |
| `loop` get/set | store; open → backend `SetLoopSegement(0,-1)` / `(-1,-1)` (`:783-791`). | store (`engine/video.rs`) | **same** |
| `frame` get/set | backend; setter re-arms `IsEventPast` only when `EventFrame>=f && IsEventPast` (`:730-747`). | `position_ms` converted; always re-arms (`video.rs:1307-1345`, `engine/video.rs:502-507`). | **diff** (§3.4) |
| `fps` | backend (`KRMoviePlayer.cpp:63`); closed 0. | `spec.fps` / 0 (`video.rs:1563-1574`). | **same** |
| `numberOfFrame` | `GetTotalTime()*GetFPS()/1000` (`KRMoviePlayer.cpp:65-68`); closed 0. | `spec.total_frames` / 0 (`video.rs:1550-1561`). | **same** (rounding) |
| `totalTime` | backend (`KRMoviePlayer.cpp:70-73`); closed 0. | `frame_to_ms(fps,total)` / 0 (`video.rs:1576-1588`). | **same** |
| `mode` get/set | getter returns `Mode`; setter writes **only if no backend is open** (`VideoOvlImpl.cpp:794-799`). | setter always writes (`video.rs:1490`). | **diff** (§3.9) |
| `playRate` get/set | backend speed; open-but-not-played → `0`, played → `1` (`VideoPlayer.cpp:1781-1793`); setter unvalidated. | stored; `1.0` on open; setter rejects non-finite/`<=0` (`video.rs:1370-1406`, `engine/video.rs:269,485-491`). | **diff** (§3.10) |
| `segmentLoopStartFrame` / `segmentLoopEndFrame` | stored verbatim (`VideoOvlImpl.h:186-187`). | stored verbatim (`video.rs:1437-1483`). | **same** |
| `periodEventFrame` get/set | getter reads `EventFrame` (cleared to `-1` after firing); setter `IsEventPast` logic. | getter `period_event().unwrap_or(-1)` (unchanged after fire); setter stored (`video.rs:1409-1435`). | **diff** (§3.4) |

### 1.3 Audio / video / mixing / colour passthrough

| member | reference (FFmpeg backend) | port | verdict |
|---|---|---|---|
| `audioBalance` get/set | backend pan ×100000; closed seed `0` (`KRMoviePlayer.cpp:94-106`). | stored int, closed `0` (`video.rs:1468`). | **seam** (no audio) |
| `audioVolume` get/set | backend volume ×100000; closed seed `0` (`:108-112`). | stored int, closed `0` (`video.rs:1469`). | **seam** |
| `numberOfAudioStream` | real stream count; closed 0 (`KRMoviePlayer.cpp:120-122`). | `1` iff `spec.has_audio()`, else `0` (`video.rs:662`). | **seam** |
| `enabledAudioStream` get/set | backend current stream; closed `-1` (`:129-131`). | stored; closed `-1`; stale across `open` (`video.rs:1470-1475`). | **seam** |
| `numberOfVideoStream` | real count; closed 0 (`:137-139`). | `1` iff width/height > 0 (`video.rs:663`). | **seam** |
| `enabledVideoStream` get/set | backend current stream; closed `-1` (`:146-148`). | stored; closed `-1` (`video.rs:1476-1481`). | **seam** |
| `mixingMovieAlpha` get/set | backend always `1.0`; closed seed `0.0` (`KRMoviePlayer.h:112`, `VideoOvlImpl.cpp:931-941`). | stored; open default `1.0`, can change (`video.rs:1485`). | **diff** (§3.8) |
| `mixingMovieBGColor` get/set | backend always `0xFF000000`; closed seed uninitialised (`KRMoviePlayer.h:116-118`, `VideoOvlImpl.cpp:943-950`). | stored; open default `0`; closed `0` (`video.rs:1486`). | **diff** (§3.8) |
| `contrast`/`brightness`/`hue`/`saturation` get/set | backend methods are stubs; getters keep their `-1.0f` seed **even when open** (`KRMoviePlayer.h:130-168`, `VideoOvlImpl.cpp:979-1112`). | open → stored defaults `1/0/0/1` (`video.rs:1494-1497`). | **diff** (§3.8) |
| `*RangeMin/Max/DefaultValue/StepSize` (16 members) | always `-1.0` (stub backend). | open → fabricated constants `0/2/1/0.01`, `-1/1/0/0.01`, etc.; closed `-1.0` (`video.rs:1499-1514`). | **diff** (§3.8) |
| `layer1`/`layer2` get/set | store resolved `Layer`; getter returns it or `null` (`VideoOvlIntf.cpp:686-780`). | store retained Layer; getter returns it or `null` (`video.rs:1594-1750`). | **same** |

---

## 2. Event and status machine

### 2.1 States

`tTVPSoundStatus` is `{ssUnload, ssStop, ssPlay, ssPause, ssReady}`
(`reference/cpp/core/sound/SoundBufferBaseIntf.h:19-25`); the wrapper strings
are `unload/stop/play/pause/ready` (`VideoOvlIntf.cpp:67-88`). The port's
`PlayStatus` is `{Unload, Stop, Play, Pause}` (`engine/video.rs:47-88`) and
**cannot express `ready`**. With the in-tree FFmpeg backend no code path ever
posts `WM_STATE_CHANGE/vsReady` (the only producer is the Win32 backend), so
`ready` is unreachable here — but it is still a latent gap.

### 2.2 When status changes

| trigger | reference | port |
|---|---|---|
| constructor | `ssUnload` (`VideoOvlIntf.cpp:24`) | `Unload` (`engine/video.rs:263-277`) |
| `open()` | `SetStatus(ssStop)` → **sync** event if changed (`VideoOvlImpl.cpp:241`, `VideoOvlIntf.cpp:91-110`) | `status = Stop`, no event (`engine/video.rs:336`) |
| `close()` | `SetStatus(ssUnload)` → **sync** event if changed (`:265`) | `status = Unload`, no event (`engine/video.rs:349`) |
| `play()` | backend, then `SetStatus(ssPlay)` **sync** (`:308-316`) | queued `StatusChanged(Play)`, dispatched next frame (`engine/video.rs:391`, `video.rs:1830-1862`) |
| `pause()` | `SetStatus(ssPause)` **sync** (`:328-335`) | queued |
| `stop()` | `SetStatus(ssStop)` **sync** (`:318-326`) | queued |
| natural end (non-loop) | `VideoOverlay->Stop()` (rewind 0) + `SetStatusAsync(ssStop)` **posted** (`:529-550`) | position = duration (!), status `Stop`, `StatusChanged + TransitionComplete` queued in the same frame (`engine/video.rs:697-707`) |
| loop wrap | `Rewind()` + `FirePeriodEvent(perLoop)` **sync** (`:533-540`) | wrap + `Period(Loop)` queued (`engine/video.rs:688-696`) |
| segment wrap | `SetFrame(start)` + `FirePeriodEvent(perSegLoop)` **sync** (`:567-575`) | wrap + `Period(SegmentLoop)` + `TransitionComplete` + `FrameUpdate` queued (`:686-698`) |

The reference `SetStatus` uses `TVP_EPT_IMMEDIATE`
(`VideoOvlIntf.cpp:106`), which `EventIntf.cpp:217-231` proves is delivered
*inside* the posting call. The port instead appends `VideoEvent::StatusChanged`
to `VideoOverlay::pending` (`engine/video.rs:745-753`) and only drains it from
`video_advance` (`video.rs:1830-1862`).

Consequence: a handler that runs during `play()` in the reference does not run
until the next frame in the port. `open()`/`close()` produce no `onStatusChanged`
at all in the port. For `system/enveffect.tjs` the shadow `_status` makes this
tolerable, but it is a real semantic divergence.

### 2.3 `onPeriod` reasons

`perLoop=0`, `perPeriod=1`, `perPrepare=2`, `perSegLoop=3`
(`VideoOvlIntf.cpp:28-36`). The port's `PeriodReason::code()` emits the same
integers (`engine/video.rs:104-116`). Fire sites:

* `perLoop`: reference `EC_COMPLETE` with `Loop` (`:533-540`); port `advance`
  wrap when looping and no segment (`engine/video.rs:690-694`).
* `perSegLoop`: reference `EC_UPDATE` when `SegLoopEndFrame > 0` and
  `frame >= SegLoopEndFrame` (`:567-575`); port `advance` wrap when `looping`
  **and** a segment is set (`:687-695`). The port adds the `looping`
  precondition the reference does not have, and it additionally emits
  `TransitionComplete` + `FrameUpdate`.
* `perPeriod`: reference fires on `EC_UPDATE` once when `0 < EventFrame <=
  curFrame` and `!IsEventPast`, then clears `EventFrame = -1` (`:628-631`).
  Port fires when `period_event_frame >= 0 && !period_fired && frame >= it`
  and leaves the frame in place (`engine/video.rs:713-716`).
* `perPrepare`: reference fires on the first `EC_UPDATE` after a layer-mode
  `prepare()`; port fires immediately from `prepare()` (`engine/video.rs:435-441`).

### 2.4 `position` / `frame` / `playRate`, seeking, pause/resume

* **`position` getter**: backend `GetTime`, `0` when closed
  (`KRMoviePlayer.cpp:41-44`, `VideoOvlImpl.cpp:720-728`). Port returns
  `position_ms` / 0.
* **`position` setter**: backend `SetPosition`; does **not** touch the period
  event (`VideoOvlImpl.cpp:718-723`). Port's `seek_ms` sets
  `period_fired=false` (`engine/video.rs:495-500`), re-arming a fired period
  event the reference would leave fired.
* **`frame` setter**: backend `SetFrame` (which itself is
  `SeekTime(f/fps*1000)`, `KRMoviePlayer.cpp:57-60`); only re-arms `IsEventPast`
  when the armed frame is at/after the new frame (`VideoOvlImpl.cpp:730-738`).
  Port always re-arms (`engine/video.rs:502-507`).
* **`playRate`**: reference returns the live backend speed — `0` while
  open-but-not-played because `OpenInputStream` sets `DVD_PLAYSPEED_PAUSE`
  (`VideoPlayer.cpp:309`) — and `SetSpeed` multiplies by
  `DVD_PLAYSPEED_NORMAL=1000`; `SetPlayRate` performs no validation
  (`KRMoviePlayer.cpp:75-79`). Port stores `1.0` as "normal" and rejects
  `<=0`/non-finite (`engine/video.rs:485-491`).
* **pause/resume**: reference `Pause()` only zeroes the speed if non-zero
  (`VideoPlayer.cpp:213-215`); `Play()` restarts at the current position and
  does not rewind unless a prior `Stop()` did. Port `pause`/`play` match this
  except for the `play`-at-end reset (`engine/video.rs:387-390`), which the
  reference achieves through `Stop()`.

---

## 3. Divergences in detail

### 3.1 `open`/`close` do not fire `onStatusChanged`, and playback events are deferred — **high**

Reference: `Open` ends in `SetStatus(ssStop)` (`VideoOvlImpl.cpp:241`) and
`Close` calls `SetStatus(ssUnload)` (`:265`); `SetStatus` posts with
`TVP_EPT_IMMEDIATE` (`VideoOvlIntf.cpp:91-110`), which `EventIntf.cpp:217-231`
delivers synchronously. `play`/`stop`/`pause` use the same immediate path
(`VideoOvlImpl.cpp:308-335`).

Port: `open` assigns `self.status = PlayStatus::Stop` with the explicit comment
"apply_open assigns the status directly rather than firing an `onStatusChanged`"
(`crates/engine/src/video.rs:336`); `close` assigns `Unload` directly
(`:349`). The commands queue `StatusChanged` events into `pending`
(`engine/video.rs:391,404,419,745-753`), drained only from `video_advance`
(`crates/compat/src/natives/video.rs:1830-1862`).

Effect: `var f=0; v.onStatusChanged=function(s){f=1;}; v.play(); f` is `1` in
the reference and `0` in the port. `open`/`close` never notify. This also
changes event ordering relative to the same frame's `onPeriod`/`onFrameUpdate`.

### 3.2 `setSegmentLoop` invents errors and gates wrapping on `loop` — **high**

Reference `SetSegmentLoop` stores both frames and calls the backend with no
range check at all (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:357-361`);
the TJS method only requires two parameters (`VideoOvlIntf.cpp:305-317`). `-1`
is a valid value: `SetLoop(false)` itself calls `SetLoopSegement(-1,-1)`
(`VideoOvlImpl.cpp:787-789`).

Port `set_segment_loop` raises `VideoError::InvalidSegment` whenever open and
`start < 0 || end < start || end >= total_frames`
(`crates/engine/src/video.rs:527-537`), surfaced as a script error by
`vo_set_segment_loop` (`crates/compat/src/natives/video.rs:871-916`). So
`v.setSegmentLoop(-1, -1)` and any exclusive-end range both throw.

Additionally the port only wraps when `self.looping` is true
(`engine/video.rs:676-698`), whereas the reference `EC_UPDATE` checks
`SegLoopEndFrame > 0` regardless of `Loop` (`VideoOvlImpl.cpp:567-575`).

`system/enveffect.tjs:723-728` calls exactly `.setSegmentLoop(seg[0], seg[1])`,
so a game whose `seg` is exclusive or `-1` breaks inside `start()`.

### 3.3 `prepare()` is rewritten — **high**

Reference (`VideoOvlImpl.cpp:349-355`):

```cpp
if(VideoOverlay && (Mode == vomLayer)) {
    Pause(); Rewind(); IsPrepare = true; Play();
}
```

`perPrepare` is then fired from `EC_UPDATE`, after which the wrapper does
`Pause(); Rewind(); IsPrepare = false` (`:635-640`). So after `prepare()` the
overlay is paused at frame 0 and `onPeriod(perPrepare)` arrives one decoded
frame later.

Port `VideoOverlay::prepare` only checks `is_open()` and immediately queues
`VideoEvent::Period(PeriodReason::Prepare)` (`crates/engine/src/video.rs:435-441`);
`vo_prepare` (`crates/compat/src/natives/video.rs:774-790`) does not check the
mode, does not pause/rewind/play, and the status is left unchanged. On a
`vomOverlay`/`vomMixer`/`vomMFEVR` overlay the reference is a silent no-op while
the port emits `perPrepare`.

### 3.4 period-event semantics — **medium**

Three separate mismatches:

1. **Set at/before the current frame.** Reference `SetPeriodEvent` sets
   `IsEventPast = eventFrame <= GetFrame()` (`VideoOvlImpl.cpp:363-370`); a
   frame at or behind the play head therefore does **not** fire. Port clears
   `period_fired` and fires on the next advance at or past the frame
   (`engine/video.rs:555-559,713-716`).
2. **After firing.** Reference sets `EventFrame = -1` (`:628-631`), so
   `periodEventFrame` reads `-1`. Port keeps the frame, so it reads the old
   value (`crates/compat/src/natives/video.rs:1409-1435`).
3. **Seeks.** Reference `position` setter never re-arms (`VideoOvlImpl.cpp:718-723`);
   `frame` re-arms only when `EventFrame >= f && IsEventPast` (`:730-738`).
   Port's `seek_ms`/`seek_frame` always clear `period_fired`
   (`engine/video.rs:495-507`), so a period event the reference would treat as
   passed fires again after a seek.

`system/enveffect.tjs:737` sets the event from `se[_perParamIndex].frame` after
it already checked `se[i].frame > .frame`, so the common case is safe; the edge
cases above are still observable.

### 3.5 status-event defaults, window action owner, and callback command — **low/medium**

Reference event defaults (`onStatusChanged` etc.) invoke the *window* the
overlay was constructed with (`ActionOwner`, set from `param[0]`,
`VideoOvlIntf.cpp:52`; forwarding at `:407-456`). The port's native hooks are
inert (`crates/compat/src/natives/video.rs:1068-1119`) and dispatch goes through
`call_member(objthis, …)` on the overlay object (`:1755-1794`, `:1829-1862`).
If a script does not override the handler but relies on the window's default,
the port drops it. `onCallbackCommand` is registered but never fired (no host
callback source; `reference/cpp/core/visual/impl/VideoOvlImpl.cpp:418-425`).

Also, the reference `Construct` requires a `Window` argument and registers
itself with it (`VideoOvlIntf.cpp:28-55`): a missing argument returns
`TJS_E_BADPARAMCOUNT`, a non-object throws a variant conversion error, and a
null/non-Window throws `TVPSpecifyWindow`. The port accepts no argument or
`null` and just stores an optional window id
(`crates/compat/src/natives/video.rs:573-616`, `:592-610`). The real game always
passes `win`, so this is low impact, but it removes the reference's error
contract and the `Window`-invalidation → `Disconnect`/`Shutdown` path
(`VideoOvlImpl.cpp:244-300`).

### 3.6 `vomLayer` rectangle and visibility target the wrong object — **medium**

Reference, in `vomLayer` (`Mode == vomLayer`):

* `SetPosition/SetLeft/SetTop` move `Layer1` and `Layer2` (`VideoOvlImpl.cpp:392-442`).
* `SetSize/SetWidth/SetHeight/SetBounds` return without doing anything (`:404-458`).
* `SetVisible` sets `Layer1/Layer2->SetVisible` (`:460-475`).
* The `left/top/width/height` getters always read the wrapper's own `Rect`
  (`VideoOvlImpl.h:118-133`), which those setters never update in layer mode.

Port: `vo_left_set`/`vo_top_set` call `overlay.set_position`
(`crates/compat/src/natives/video.rs:1139-1195`), `vo_width_set`/`vo_height_set`
call `overlay.set_size` (`:1197-1253`), `vo_set_size`/`vo_set_bounds` always
mutate bounds (`:819`, `:843`), and `vo_visible_set` only flips
`overlay.visible`/`binding.visible` (`:1255-1269`). None of them touch
`layer1`/`layer2`, and the getters read the mutated bounds. The game sets its
layer geometry directly (`system/movie.tjs`), so this is mostly latent, but it
is a plain member-level divergence.

### 3.7 stream selection — **seam**

`selectAudioStream` requires an argument in the reference
(`VideoOvlIntf.cpp:369-380`); the port defaults a missing argument to `0`
(`crates/compat/src/natives/video.rs:1047-1060`). `enabledAudioStream`'s setter
is additionally gated on `is_open()` in the port (`:1470-1475`) while the
`selectAudioStream` method is not, so the two paths disagree. Neither reaches a
decoder; `VideoSource` has no audio (`:75-80`).

### 3.8 mixing and colour metadata — **medium**

The FFmpeg backend is the in-tree oracle and it stubs every one of these
(`reference/cpp/core/movie/ffmpeg/KRMoviePlayer.h:106-168`):

* `GetMixingMovieAlpha` → `1.0f`; `GetMixingMovieBGColor` → `0xFF000000`.
* `GetContrast*`/`GetBrightness*`/`GetHue*`/`GetSaturation*` do nothing, so the
  wrapper's `-1.0f` seed survives **even when open** (`VideoOvlImpl.cpp:979-1112`).

Port, when open, returns stored defaults (`contrast=1.0`, `brightness=0.0`,
`hue=0.0`, `saturation=1.0`; `mixing_alpha=1.0`, `mixing_bg=0`) and fabricated
range metadata (`crates/compat/src/natives/video.rs:1485-1514`), changing as the
script writes them. So `v.mixingMovieBGColor` reads `0xFF000000` in the
reference and `0` in the port by default, and every colour range reads `-1.0`
in the reference and `0/2/1/0.01` (etc.) in the port. These are the port's own
invented values not backed by the oracle (D11).

The port's existing test
`crates/compat/tests/video.rs::color_adjustment_follows_the_open_state`
encodes the port's values as if they were the reference; it should be treated
as a port-behaviour lock, not evidence.

### 3.9 `mode` is mutable after `open` — **medium**

Reference:

```cpp
void tTJSNI_VideoOverlay::SetMode(tTVPVideoOverlayMode m) {
    // mode change after video open is forbidden
    if(!VideoOverlay) { Mode = m; }
}
```

(`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:794-799`).

Port `slot_int_prop!(vo_mode_get, vo_mode_set, mode)` writes unconditionally
(`crates/compat/src/natives/video.rs:1490`). Once open, the reference ignores
`v.mode = x`; the port changes which layer the frame is composited into on the
next advance.

### 3.10 `playRate` — **medium**

Reference `GetPlayRate` reads `m_pPlayer->GetSpeed()`
(`KRMoviePlayer.cpp:81-83`). `OpenInputStream` parks the player at
`DVD_PLAYSPEED_PAUSE` (`VideoPlayer.cpp:309`), so a freshly opened, not yet
played overlay reports `0.0`; `SetSpeed` scales by 1000
(`VideoPlayer.cpp:1781-1793`) and `SetPlayRate` validates nothing. Port starts
at `1.0` (`crates/engine/src/video.rs:269`), never resets on `open`, and raises
`InvalidPlayRate` for `<=0`/non-finite (`:485-491`, `crates/compat/src/natives/video.rs:1390-1406`).
The port's normal-speed convention (`1.0`) is a legitimate L2 choice, but the
script-visible value and the invented error are not the reference's.

### 3.11 playback end position — **medium**

Reference non-loop end: `EC_COMPLETE` handler calls `VideoOverlay->Stop()`
(`VideoOvlImpl.cpp:529-550`), i.e. `BasePlayer::Stop()` = `SetSpeed(0); SeekTime(0)`
(`VideoPlayer.cpp:209-215`), then `SetStatusAsync(ssStop)`. So after a movie
finishes, `position` is `0`. Port sets `position_ms = end_ms` and leaves it
(`crates/engine/src/video.rs:697-704`). A script reading `v.position` after the
"stop" event sees the duration instead of `0`.

### 3.12 `vomMixer` opacity ignored — **medium (if it composites at all)**

Port `composite_overlay`'s `MODE_MIXER` branch blits with
`source_over = true` (`crates/compat/src/natives/video.rs:1890-1894`), which
reaches `blit_over(..., 255, false, false)` (`crates/engine/src/video.rs:811`) —
a constant full opacity. The layer's opacity, read and stored by
`vo_set_mixing_layer` (`:1005-1021`) as `mixing_layer_alpha`, is never used.
Reference `SetMixingLayer` passes `opacity/255` to
`SetMixingBitmap` (`VideoOvlImpl.cpp:882-913`).

### 3.13 mode family — see §4.

---

## 4. The mode family (`vomOverlay` / `vomLayer` / `vomMixer` / `vomMFEVR`)

`voMode.h`: `vomOverlay=0`, `vomLayer=1`, `vomMixer=2`, `vomMFEVR=3`. The port
encodes only `MODE_LAYER=1` and `MODE_MIXER=2` (`crates/compat/src/natives/video.rs:1811-1814`).

| mode | reference (FFmpeg backend) | port | verdict |
|---|---|---|---|
| `vomOverlay` (0) | `MoviePlayerOverlay`, presented through a cocos2d node on the owner window (`KRMoviePlayer.cpp:264-355`); wrapper `ResetOverlayParams` sets the window + `ZoomRectangle` + `SetRect` (`VideoOvlImpl.cpp:478-500`); `EC_UPDATE` never handles it. | `composite_overlay` `_ => {}` — nothing is drawn; no window surface (`video.rs:1875-1898`). | **seam** (windowed overlay absent) |
| `vomLayer` (1) | `MoviePlayerLayer`; `EC_UPDATE` sizes `Layer1/Layer2` to the video and assigns the front buffer (`VideoOvlImpl.cpp:553-639`). | port sizes `layer1`/`layer2` (`prepare_layer_for_frame`) and blits what it buffered (`video.rs:1883-1889`, `1900-1923`). | **same** shape; rectangle/wrap caveats §3.2/§3.6 |
| `vomMixer` (2) | `GetMixingVideoOverlayObject` returns `MoviePlayerOverlay`; `SetMixingBitmap`/`ResetMixingBitmap`/`PresentVideoImage` are **stubs** (`KRMoviePlayer.h:106-120`), so the layer is not composited — only `onFrameUpdate`/period events fire (`VideoOvlImpl.cpp:642-665`). | port actually composites the stored `mixing_layer` each frame (`video.rs:1890-1894`) but ignores the layer opacity (§3.12). | **diff** (port draws; reference stub); opacity wrong either way |
| `vomMFEVR` (3) | `GetMFVideoOverlayObject` returns `MoviePlayerOverlay`; `Play/Stop/Pause` suppress `SetStatus` because `Mode != vomMFEVR` fails (`VideoOvlImpl.cpp:308-335`); `ResetOverlayParams` still runs (`:478`). | falls into `_ => {}`. Status is set for all modes (port has no MFEVR branch). | **diff** (status suppression) |

`SetRectangleToVideoOverlay` (`VideoOvlImpl.cpp:375-390`) — the window zoom
path used by the three non-layer modes — has no port equivalent; the port's
`bounds` are consumed directly for nothing in overlay/MFEVR and only the layer
composite is realized.

---

## 5. What the seam cannot support

`compat::VideoSource` exposes only two calls
(`crates/compat/src/natives/video.rs:75-80`):

```rust
fn spec(&self, path: &str) -> Option<VideoSpec>;
fn frame_at(&mut self, path: &str, at_ms: i64) -> Option<RgbaFrame>;
```

and `api::media::VideoSpec` carries only `width, height, fps, total_frames,
sample_rate, channels` (`crates/platform/api/src/media.rs:144-159`). Therefore:

* **Audio** (`audioBalance`, `audioVolume`, `selectAudioStream`,
  `enabledAudioStream/VideoStream`, `numberOfAudioStream`) can only be stored
  numbers. `MediaVideoSource` (`crates/app/src/video.rs:109-122`) decodes
  *video only*; no sound is mixed. In `system/movie.tjs` the game sets
  `audioVolume` and expects the movie's audio — the port is silent.
* **Stream selection** has no multi-stream notion; the counts collapse to 0/1.
* **Colour adjustment** (`contrast`/`brightness`/`hue`/`saturation`) has no
  filter stage, so the port can only store values (and, per §3.8, reports
  different defaults than the reference anyway).
* **`vomOverlay`/`vomMFEVR`** need an OS/window overlay surface; the port's L2
  compositor only knows how to blit into a `Layer` bitmap, so those modes are
  inert.
* **`vomMixer` alpha** has no mixer: the frame is blitted into the layer
  bitmap with a fixed opacity, not `SetMixingBitmap(dest, alpha)`.
* **`ready` (`ssReady`)** cannot be represented by `PlayStatus`; the in-tree
  FFmpeg backend never emits it, so this is latent.
* **`onCallbackCommand`** has no host source (Flash/DirectShow callbacks), so
  it can never fire.

None of these are silent fallbacks in the L3 sense — `open` raises when the
host has no source (`crates/compat/src/natives/video.rs:645-652`) — but they
are capability limits the audit records explicitly.

---

## 6. Tests that encode port (not reference) behaviour

* `crates/compat/tests/video.rs::playback_commands_translate_to_l2` calls
  `open` then reads `position` — it does not assert the missing
  `onStatusChanged("stop")`.
* `...::invalid_loop_and_rate_raise` asserts the invented `InvalidSegment` /
  `InvalidPlayRate` raises (§3.2, §3.10).
* `...::color_adjustment_follows_the_open_state` asserts the fabricated colour
  defaults (§3.8).
* `...::playback_on_a_closed_overlay_is_inert` is correct and should stay.
* `crates/engine/tests/video.rs::commands_on_a_closed_overlay_are_typed_errors`
  locks `VideoError::NotOpen` at L2; the L3 guard is correct, but the L2 API
  still exposes the error type.

No source was modified by this audit.
