# motionplayer (`motionplayer.dll`)

**DLL name:** `motionplayer.dll` — linked with `Plugins.link("motionplayer.dll")`
**Rust module:** `crates/compat/src/natives/plugins/motion_player/`
**Reference:** `reference/cpp/plugins/motionplayer/` (`main.cpp`, `Player.*`, `EmotePlayer.*`, `MotionNode.h`, `NodeTree.cpp`, `ResourceManager.*`, `SourceCache.*`, `SeparateLayerAdaptor.*`, `D3DAdaptor.*`, `manual.tjs`)
**Registers:** `Motion`, `Player`, and the `Motion.*` subclasses
**Related:** [`psb_file`](psb_file.md) supplies the PSB container, [`emote_player`](emote_player.md) is the loader DLL that pulls this module in, [`../plugins.md`](../plugins.md) is the plugin index

The reference is a Direct3D PSB mesh renderer with a physics model. The port
keeps the whole script-visible surface and the playback model and paints
through the engine's layer system instead of a D3D rasterizer; the divergence
is spelled out under *Known gaps*.

## Surface

### Placement

The reference publishes four top-level classes — `Player`
(`reference/cpp/plugins/motionplayer/main.cpp:128`), `Motion`
(`reference/cpp/plugins/motionplayer/main.cpp:427`), `D3DEmoteModule`
(`reference/cpp/plugins/motionplayer/main.cpp:565`) and `D3DEmotePlayer`
(`reference/cpp/plugins/motionplayer/main.cpp:588`). Every other class is
registered with `NCB_REGISTER_SUBCLASS_DELAY`, which only defines its
members, and is then attached under `Motion` with `NCB_SUBCLASS`
(`reference/cpp/plugins/motionplayer/main.cpp:430-441`,
`reference/cpp/core/plugin/ncbind.hpp:1963-1970`, `:1982-1987`). Those
classes exist only as `Motion.*`.

The compat class-registration API always publishes a global, so the port
registers each namespace class top-level, aliases it into `Motion` and then
deletes the global (`crates/compat/src/natives/plugins/motion_player/mod.rs`).
The script-visible result is the reference's shape:

| class | reference | port |
|---|---|---|
| `Motion` | top-level namespace class (`main.cpp:427`) | global `Motion` |
| `Player` | top-level class (`main.cpp:128`), aliased into `Motion` by the post-registration callback (`main.cpp:511-528`) | global `Player` **and** `Motion.Player` |
| `D3DEmotePlayer` | top-level class (`main.cpp:588`) | global `D3DEmotePlayer` only — not a `Motion` member |
| `D3DEmoteModule` | top-level class (`main.cpp:565`) | global `D3DEmoteModule` only — not a `Motion` member |
| `ResourceManager`, `EmotePlayer`, `SeparateLayerAdaptor`, `D3DAdaptor`, `SourceCache`, `ObjSource`, `Point`, `Circle`, `Rect`, `Quad`, `LayerGetter` | `Motion.*` only (`main.cpp:430-441`) | `Motion.*`; the temporary registration global is deleted again |

`Rect` is registered under the internal name `MotionRect`, because the core
VM already owns a `Rect` class; the script-visible name is `Motion.Rect`
(`crates/compat/src/natives/plugins/motion_player/adaptors.rs`).

Registration is **idempotent**: a class that already exists is left alone and
the subclasses are attached to the existing `Motion` object, so installing the
plugin family twice — or alongside the `emote_player` port — cannot fail with
a duplicate-class error (`crates/compat/src/natives/plugins/motion_player/mod.rs`).

Every class registers its own constructor as a member named after the class,
so a script subclass reaches the native instance through
`super.<ClassName>()`. The member counts below are the ones the parity report
records for the port (`docs/plugin_parity.md`); the surface lock
(`docs/surface/runtime_surface.json`) adds the constructor entries on top of
them — 143 entries for `Player` and 183 for `D3DEmotePlayer`.

### `Motion`

The namespace carries the reference's constant groups
(`reference/cpp/plugins/motionplayer/main.cpp:444-477`):

| member | kind | behaviour |
|---|---|---|
| `LayerTypeObj` | constant (read-only) | `0` |
| `LayerTypeShape` | constant (read-only) | `1` |
| `LayerTypeLayout` | constant (read-only) | `2` |
| `LayerTypeMotion` | constant (read-only) | `3` |
| `LayerTypeParticle` | constant (read-only) | `4` |
| `LayerTypeCamera` | constant (read-only) | `5` |
| `ShapeTypePoint` | constant (read-only) | `0` |
| `ShapeTypeCircle` | constant (read-only) | `1` |
| `ShapeTypeRect` | constant (read-only) | `2` |
| `ShapeTypeQuad` | constant (read-only) | `3` |
| `PlayFlagForce` | constant (read-only) | `1` |
| `PlayFlagChain` | constant (read-only) | `2` |
| `PlayFlagAsCan` | constant (read-only) | `4` |
| `PlayFlagJoin` | constant (read-only) | `8` |
| `PlayFlagStealth` | constant (read-only) | `16` |
| `TransformOrderFlip` | constant (read-only) | `0` |
| `TransformOrderAngle` | constant (read-only) | `1` |
| `TransformOrderZoom` | constant (read-only) | `2` |
| `TransformOrderSlant` | constant (read-only) | `3` |
| `CoordinateRecutangularXY` | constant (read-only) | `0` |
| `CoordinateRecutangularXZ` | constant (read-only) | `1` |
| `MaskModeStencil` | constant (read-only) | `0` |
| `MaskModeAlpha` | constant (read-only) | `1` |

The namespace also holds the aliased class objects: `Motion.Player`,
`Motion.ResourceManager`, `Motion.SeparateLayerAdaptor`, `Motion.D3DAdaptor`,
`Motion.SourceCache`, `Motion.ObjSource`, `Motion.Point`, `Motion.Circle`,
`Motion.Rect`, `Motion.Quad`, `Motion.LayerGetter` and
`Motion.EmotePlayer`.

### `Player`

141 members: 91 methods and 50 properties, plus the class-name constructor
`Player` (`reference/cpp/plugins/motionplayer/main.cpp:128-296`).

#### Methods

| member | kind | behaviour |
|---|---|---|
| `Player(resourceManager)` | constructor | builds a player; a manager that is not supplied is installed by the port, so `resourceManager` never reads as void (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:175-185`, `Player.h:228-229`) |
| `random()` | method | a fresh real in `[0, 1)` from the engine's own generator (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:1318-1332`) |
| `initPhysics(metadata)` | method | takes the physics metadata; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:256`) |
| `serialize()` | method | the player's runtime state as a dictionary |
| `unserialize(state)` | method | restores a serialized state; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:258`) |
| `setRotate(angle, time, easing)` | method | the rotation of the pose; arity 3 on `Player`, arity 1 on the emote classes (`reference/cpp/plugins/motionplayer/Player.h:263`, `EmotePlayer.cpp:305-324`) |
| `setMirror(flag)` | method | the mirror flag; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:266`) |
| `setHairScale(scale)` | method | the hair scale; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:268-270`) |
| `setPartsScale(scale)` | method | the parts scale; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:268-270`) |
| `setBustScale(scale)` | method | the bust scale; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:268-270`) |
| `setDrawAffineTranslateMatrix(a, b, c, d, tx, ty)` | method | the draw-time affine; six scalars or one matrix object (`reference/cpp/plugins/motionplayer/PlayerDrawDispatch.cpp:11-60`) |
| `getCameraOffset()` | method | the camera offset as a two-element array |
| `setCameraOffset(offset)` | method | the camera offset; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:279`) |
| `modifyRoot(value)` | method | the root node modification; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:280`) |
| `debugPrint()` | method | writes the player's state to the log (`reference/cpp/plugins/motionplayer/Player.h:281`) |
| `unload(name)` | method | unloads one motion; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:298`, `PlayerResource.cpp:12-35`) |
| `unloadAll()` | method | unloads every motion and clears the variable keys (`reference/cpp/plugins/motionplayer/PlayerResource.cpp:37-61`) |
| `isExistMotion(name)` | method | whether the name resolves through the player's motion lookup (`reference/cpp/plugins/motionplayer/Player.h:300`, `PlayerResource.cpp:63-66`) |
| `findMotion(name)` | method | resolves the motion, binds it and returns the module object; a miss returns void (`reference/cpp/plugins/motionplayer/Player.h:301`, `PlayerResource.cpp:68-79`) |
| `requireLayerId(name)` | method | leases a layer id; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:302`) |
| `releaseLayerId(id)` | method | releases a leased layer id; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:303`) |
| `setClearColor(color)` | method | the clear colour; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:306`) |
| `setResizable(flag)` | method | the resizable flag; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:307`) |
| `removeAllTextures()` | method | drops the registered textures (`reference/cpp/plugins/motionplayer/Player.h:308`) |
| `removeAllBg()` | method | drops the registered backgrounds (`reference/cpp/plugins/motionplayer/Player.h:309`) |
| `removeAllCaption()` | method | drops the registered captions (`reference/cpp/plugins/motionplayer/Player.h:310`) |
| `registerBg(value)` | method | registers a background; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:311`) |
| `registerCaption(value)` | method | registers a caption; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:312`) |
| `unloadUnusedTextures()` | method | drops the textures no node references (`reference/cpp/plugins/motionplayer/Player.h:313`) |
| `alphaOpAdd()` | method | the alpha-add amount (`reference/cpp/plugins/motionplayer/Player.h:314`) |
| `captureCanvas()` | method | captures the canvas into a bitmap (`reference/cpp/plugins/motionplayer/Player.h:315`) |
| `findSource(name)` | method | the cached source for a name; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:316`) |
| `loadSource(name)` | method | loads a source; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:317`) |
| `clearCache()` | method | drops the player's source cache and runtime state (`reference/cpp/plugins/motionplayer/Player.h:318`, `PlayerRender.cpp:57-62`) |
| `setSize(width, height)` | method | the canvas size; arity 2 (`reference/cpp/plugins/motionplayer/Player.h:319`) |
| `copyRect(args)` | method | copies a rectangle; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:320`) |
| `adjustGamma(args)` | method | the gamma adjustment; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:321`) |
| `draw(target)` | method | paints the pose into the target; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:322-323`, `main.cpp:234`) |
| `frameProgress(frames)` | method | advances the playback by a frame count; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:324`) |
| `setFlip(flag)` | method | the flip flag; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:327`) |
| `setOpacity(value)` | method | the opacity; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:328`) |
| `setVisible(flag)` | method | the visibility; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:329`) |
| `setSlant(value)` | method | the slant; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:330`) |
| `setZoom(value)` | method | the zoom; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:331`) |
| `getLayerNames()` | method | the labels of the motion's own layers (`reference/cpp/plugins/motionplayer/Player.h:332`) |
| `releaseSyncWait()` | method | clears both sync flags (`reference/cpp/plugins/motionplayer/PlayerLayerQuery.cpp:118-121`) |
| `calcViewParam()` | method | the view parameters as a dictionary (`x`, `y`, `zoom`, `flip`) (`reference/cpp/plugins/motionplayer/Player.h:334`) |
| `getLayerMotion(name)` | method | the node type of a named layer, or void; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:335`) |
| `getLayerGetter(name)` | method | a `Motion.LayerGetter` for a named layer, or void; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:336`) |
| `getLayerGetterList()` | method | one getter per own layer, the root excluded (`reference/cpp/plugins/motionplayer/Player.h:337`) |
| `skipToSync()` | method | seeks every timeline to the sync point (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:12-60`) |
| `setStereovisionCameraPosition(x, y, z)` | method | the stereovision camera; arity 3 (`reference/cpp/plugins/motionplayer/Player.h:339`) |
| `setVariable(label, value)` | method | writes a variable (`reference/cpp/plugins/motionplayer/main.cpp:253-255`) |
| `getVariable(label)` | method | the value of a variable; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:344`) |
| `countVariables()` | method | the number of declared variables (`reference/cpp/plugins/motionplayer/PlayerVariable.cpp:568-574`) |
| `getVariableLabelAt(index)` | method | the label of a variable; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:346`) |
| `countVariableFrameAt(index)` | method | the number of keyframes of a variable; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:347`) |
| `getVariableFrameLabelAt(index, frame)` | method | a keyframe's label; arity 2 (`reference/cpp/plugins/motionplayer/Player.h:348`) |
| `getVariableFrameValueAt(index, frame)` | method | a keyframe's value; arity 2 (`reference/cpp/plugins/motionplayer/Player.h:349`) |
| `getTimelinePlaying(label)` | method | whether the named timeline plays; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:350`, `PlayerTimeline.cpp:65-73`) |
| `getVariableRange(label)` | method | the value range of a variable; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:351`, `PlayerVariable.cpp:632-633`) |
| `getVariableFrameList(label)` | method | the keyframes of a variable; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:352`, `PlayerVariable.cpp:647-648`) |
| `countMainTimelines()` | method | the number of main timelines (`reference/cpp/plugins/motionplayer/Player.h:353`) |
| `getMainTimelineLabelAt(index)` | method | a main timeline's label; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:354`) |
| `getMainTimelineLabelList()` | method | the main timeline labels (`reference/cpp/plugins/motionplayer/Player.h:355`) |
| `countDiffTimelines()` | method | the number of difference timelines (`reference/cpp/plugins/motionplayer/Player.h:356`) |
| `getDiffTimelineLabelAt(index)` | method | a difference timeline's label; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:357`) |
| `getDiffTimelineLabelList()` | method | the difference timeline labels (`reference/cpp/plugins/motionplayer/Player.h:358`) |
| `getLoopTimeline(label)` | method | the loop timeline for a label; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:359`, `PlayerTimeline.cpp:129-137`) |
| `countPlayingTimelines()` | method | the number of playing timelines (`reference/cpp/plugins/motionplayer/Player.h:360`) |
| `getPlayingTimelineLabelAt(index)` | method | a playing timeline's label; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:361`) |
| `getPlayingTimelineFlagsAt(index)` | method | a playing timeline's flags; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:362`) |
| `getTimelineTotalFrameCount(label)` | method | a timeline's length in frames; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:363`, `PlayerTimeline.cpp:176-195`) |
| `playTimeline(label, flags)` | method | starts a timeline; arity 2 (`reference/cpp/plugins/motionplayer/Player.h:364`) |
| `stopTimeline(label)` | method | stops a timeline; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:365`) |
| `setTimelineBlendRatio(label, ratio)` | method | a timeline's blend ratio; arity 2 (`reference/cpp/plugins/motionplayer/Player.h:366`) |
| `getTimelineBlendRatio(label)` | method | a timeline's blend ratio; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:367`) |
| `fadeInTimeline(label, seconds, flags)` | method | fades a timeline in; arity 3 (`reference/cpp/plugins/motionplayer/Player.h:368`) |
| `fadeOutTimeline(label, seconds, flags)` | method | fades a timeline out; arity 3 (`reference/cpp/plugins/motionplayer/Player.h:369`) |
| `getPlayingTimelineInfoList()` | method | the playing timelines as an array (`reference/cpp/plugins/motionplayer/Player.h:370`, `PlayerTimeline.cpp:337-339`) |
| `isSelectorTarget(name)` | method | whether a name is a selector target; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:373`) |
| `deactivateSelectorTarget(name)` | method | drops a selector target; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:374`) |
| `getCommandList()` | method | the command list (`reference/cpp/plugins/motionplayer/Player.h:377`) |
| `getD3DAvailable()` | method | `true` (`reference/cpp/plugins/motionplayer/Player.h:378`, `PlayerLayerQuery.cpp:309`) |
| `doAlphaMaskOperation()` | method | the alpha-mask operation (`reference/cpp/plugins/motionplayer/Player.h:379`) |
| `onFindMotion(name, flags)` | method | the motion-found callback; arity 2 (`reference/cpp/plugins/motionplayer/Player.h:380`) |
| `play(label, flags)` | method | starts a clip and returns whether it started |
| `progress(dt)` | method | advances the playback by `dt` milliseconds and returns the playback ratio (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:1281-1305`) |
| `isPlaying()` | method | whether a timeline plays (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:625-639`) |
| `stop()` | method | clears the player's playing flag and returns `true`; the timeline state is left intact (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:641-659`) |
| `motionList()` | method | the labels of the loaded clips |
| `emoteEdit(args)` | method | the emote editor payload; arity 1 (`reference/cpp/plugins/motionplayer/Player.h:424`) |

#### Properties

| member | kind | behaviour |
|---|---|---|
| `x`, `y` | property (read/write) | the player's position (`reference/cpp/plugins/motionplayer/main.cpp:133-136`) |
| `left`, `top` | property (read/write) | the same position under the reference's other names (`main.cpp:135-136`) |
| `completionType` | property (read/write) | the interpolation mode of the completion |
| `metadata` | property (read/write) | the metadata variant the player was given |
| `chara` | property (read/write) | the character name |
| `motion` | property (read/write) | the motion name; the setter runs `onFindMotion` through the object, so a script override wins (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:445-474`) |
| `motionKey` | property (read/write) | the motion key; the setter resolves and loads the motion (`reference/cpp/plugins/motionplayer/RuntimeSupport.cpp:1319-1349`) |
| `outline` | property (read/write) | the outline name |
| `priorDraw` | property (read/write) | the draw-order flag |
| `frameLastTime` | property (read/write) | the last frame's length |
| `frameLoopTime` | property (read/write) | the loop frame's length |
| `loopTime` | property (read/write) | the loop time |
| `processedMeshVerticesNum` | property (read/write) | the mesh vertex count |
| `playing` | property (read-only) | whether a timeline plays (`main.cpp:153`) |
| `queuing` | property (read/write) | the queuing flag |
| `directEdit` | property (read/write) | the direct-edit flag |
| `selectorEnabled` | property (read/write) | the selector flag |
| `variableKeys` | property (read/write) | the variable labels; an empty array while no motion is bound (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:1248-1254`) |
| `allplaying` | property (read-only) | whether every timeline plays (`main.cpp:158`) |
| `syncWaiting` | property (read/write) | the sync-wait flag |
| `syncActive` | property (read/write) | the sync-active flag |
| `hasCamera` | property (read/write) | whether the motion has a camera |
| `cameraActive` | property (read/write) | whether the camera is active |
| `stereovisionActive` | property (read/write) | the stereovision flag |
| `tickCount` | property (read/write) | the tick counter |
| `speed` | property (read/write) | whether the playback advances; `false` freezes every clock |
| `frameTickCount` | property (read/write) | the accumulated frame count |
| `maskMode` | property (read/write) | `Motion.MaskModeStencil` or `Motion.MaskModeAlpha` |
| `colorWeight` | property (read/write) | the packed colour weight, read and written in the swapped channel order (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:24-27`, `:144-152`) |
| `independentLayerInherit` | property (read/write) | the layer-inheritance flag |
| `zFactor` | property (read/write) | the depth-to-screen factor |
| `cameraTarget` | property (read/write) | the camera target |
| `cameraPosition` | property (read/write) | the camera position |
| `cameraFOV` | property (read/write) | the camera field of view |
| `cameraAlive` | property (read/write) | the camera-alive flag |
| `canvasCaptureEnabled` | property (read/write) | the canvas-capture flag |
| `clearEnabled` | property (read/write) | whether the draw clears the target before compositing |
| `hitThreshold` | property (read/write) | the hit-test threshold |
| `preview` | property (read/write) | the preview flag; preview playback does not splice the child motions |
| `outsideFactor` | property (read/write) | the outside force factor |
| `resourceManager` | property (read/write) | the resource manager the player was built with |
| `stealthChara` | property (read/write) | the character the stealth motion was stored for |
| `stealthMotion` | property (read/write) | the pending stealth motion |
| `tags` | property (read/write) | the tags variant |
| `project` | property (read/write) | the project the player is bound to |
| `useD3D` | property (read/write) | the D3D request flag (`main.cpp:188`) |
| `meshline` | property (read/write) | the mesh line name |
| `busy` | property (read-only) | the busy flag (`main.cpp:190`) |

`Player.useD3D` exists twice: as the **instance** property above and as a
**class-static** probe object on the `Player` class object. The reference's
post-registration callback attaches an empty dictionary to the class with
`TJS_STATICMEMBER` (`reference/cpp/plugins/motionplayer/main.cpp:489-509`), so
`Motion.Player.useD3D` reads as an object; the port creates the marker once
and duplicates it on every read, so its identity is stable.

### `Motion.<Class>` members

#### `Motion.ResourceManager` — 10 members

| member | kind | behaviour |
|---|---|---|
| `ResourceManager(window, cacheSize)` | constructor | arity 2 (`reference/cpp/plugins/motionplayer/main.cpp:405`); the window argument gets the three keybinder dictionaries when they are absent (`reference/cpp/plugins/motionplayer/ResourceManager.cpp:84-101`) |
| `load(path)` | method | reads, parses and caches the PSB at `path` and returns the module object (`reference/cpp/plugins/motionplayer/ResourceManager.cpp:143-170`) |
| `loadSource(path)` | method | loads a source into the manager's cache |
| `unload(path)` | method | drops every cache key that points at the module (`reference/cpp/plugins/motionplayer/ResourceManager.cpp:172-205`) |
| `clearCache()` | method | drops the modules and resets the layer-id allocator (`reference/cpp/plugins/motionplayer/ResourceManager.cpp:207-224`) |
| `findSource(path)` | method | the cached module object, or void (`reference/cpp/plugins/motionplayer/ResourceManager.cpp:263-265`) |
| `requireLayerId()` | method | leases an increasing layer id, starting at 1 (`reference/cpp/plugins/motionplayer/ResourceManager.cpp:292-305`) |
| `releaseLayerId(id)` | method | releases a leased layer id; arity 1 (`reference/cpp/plugins/motionplayer/ResourceManager.h:37`) |
| `ResourceManager.setEmotePSBDecryptSeed(seed)` | static method | the PSB decryption seed; exactly one integer (`reference/cpp/plugins/motionplayer/ResourceManager.cpp:112-121`) |
| `ResourceManager.setEmotePSBDecryptFunc(func)` | static method | the PSB decryption function (`reference/cpp/plugins/motionplayer/ResourceManager.cpp:127-141`) |

#### `Motion.SeparateLayerAdaptor` — 6 members

| member | kind | behaviour |
|---|---|---|
| `SeparateLayerAdaptor(targetLayer)` | constructor | the adaptor; the owner is taken from the construction-time `targetLayer.window` (`reference/cpp/plugins/motionplayer/SeparateLayerAdaptor.cpp:26-34`, `:231`) |
| `clear()` | method | releases the private render state and leaves `absolute` and `targetLayer` alone (`reference/cpp/plugins/motionplayer/SeparateLayerAdaptor.cpp:262-279`) |
| `assign(another)` | method | copies the managed render lists; a non-adaptor argument is a no-op (`reference/cpp/plugins/motionplayer/SeparateLayerAdaptor.cpp:438-467`) |
| `layerTreeOwnerInterface()` | method | the owner's own `layerTreeOwnerInterface` property, or void when there is no owner (`reference/cpp/plugins/motionplayer/SeparateLayerAdaptor.cpp:297-330`) |
| `absolute` | property (read/write) | the absolute-mode integer, default `0` (`reference/cpp/plugins/motionplayer/SeparateLayerAdaptor.h:97-98`, `:147`) |
| `targetLayer` | property (read/write) | the layer the adaptor draws into |

#### `Motion.D3DAdaptor` — 16 members

| member | kind | behaviour |
|---|---|---|
| `D3DAdaptor(window, width, height, centerX, centerY)` | constructor | arity 5 and a `Window` first argument, otherwise it raises `must set Window object` (`reference/cpp/plugins/motionplayer/D3DAdaptor.cpp:19-27`) |
| `setPos(x, y)` | method | the canvas position |
| `setSize(width, height)` | method | the canvas size |
| `setClearColor(color)` | method | the clear colour |
| `setResizable(flag)` | method | the resizable flag |
| `removeAllTextures()` | method | drops the registered textures |
| `removeAllBg()` | method | drops the registered backgrounds |
| `removeAllCaption()` | method | drops the registered captions |
| `registerBg(value)` | method | registers a background |
| `registerCaption(value)` | method | registers a caption |
| `unloadUnusedTextures()` | method | drops the textures no node references |
| `captureCanvas()` | method | captures the canvas; it raises unless the argument is a `Layer` |
| `visible` | property (read/write) | the canvas visibility |
| `alphaOpAdd` | property (read/write) | the alpha-add amount |
| `canvasCaptureEnabled` | property (read/write) | the canvas-capture flag |
| `clearEnabled` | property (read/write) | the clear flag |

#### `Motion.SourceCache` — 4 members

| member | kind | behaviour |
|---|---|---|
| `SourceCache(target, size)` | constructor | the cache (`reference/cpp/plugins/motionplayer/main.cpp:27-31`) |
| `loadSource(path)` | method | loads a source; arity 1 |
| `clearCache()` | method | drops the cached sources |
| `bufLayer` | property (read-only) | the buffer layer |

#### `Motion.ObjSource` — 1 member

| member | kind | behaviour |
|---|---|---|
| `ObjSource()` | constructor | the object source (`reference/cpp/plugins/motionplayer/main.cpp:33`) |

#### `Motion.Point` — 5 members

| member | kind | behaviour |
|---|---|---|
| `Point()` | constructor | the point |
| `contains(x, y)` | method | a stub that always misses; arity 2 (`reference/cpp/plugins/motionplayer/SourceCache.h:136`) |
| `type` | property (read-only) | `0` (`Motion.ShapeTypePoint`) (`reference/cpp/plugins/motionplayer/SourceCache.h:129-137`) |
| `x`, `y` | property (read-only) | the coordinates |

#### `Motion.Circle` — 6 members

| member | kind | behaviour |
|---|---|---|
| `Circle()` | constructor | the circle |
| `contains(x, y)` | method | the real radius test; arity 2 (`reference/cpp/plugins/motionplayer/SourceCache.h:148-151`) |
| `type` | property (read-only) | `1` (`Motion.ShapeTypeCircle`) (`reference/cpp/plugins/motionplayer/SourceCache.h:140-152`) |
| `x`, `y`, `r` | property (read-only) | the centre and the radius |

#### `Motion.Rect` — 6 members, plus the `MotionRect` constructor

The class object is registered internally as `MotionRect`, because the core VM
already owns a `Rect` class, so its constructor member is `MotionRect` and a
script subclass calls `super.MotionRect()`. The script-visible name is
`Motion.Rect`.

| member | kind | behaviour |
|---|---|---|
| `MotionRect()` | constructor | the rectangle |
| `contains(x, y)` | method | the half-open rectangle test; arity 2 (`reference/cpp/plugins/motionplayer/SourceCache.h:164-166`) |
| `type` | property (read-only) | `2` (`Motion.ShapeTypeRect`) (`reference/cpp/plugins/motionplayer/SourceCache.h:155-167`) |
| `l`, `t`, `w`, `h` | property (read-only) | the edges and the size |

#### `Motion.Quad` — 4 members

| member | kind | behaviour |
|---|---|---|
| `Quad()` | constructor | the quad |
| `contains(x, y)` | method | a stub that always misses; arity 2 (`reference/cpp/plugins/motionplayer/SourceCache.h:177`) |
| `type` | property (read-only) | `3` (`Motion.ShapeTypeQuad`) (`reference/cpp/plugins/motionplayer/SourceCache.h:170-178`) |
| `p` | property (read-only) | a stub that reads back as `void` (`reference/cpp/plugins/motionplayer/SourceCache.h:176`) |

#### `Motion.LayerGetter` — 28 members

| member | kind | behaviour |
|---|---|---|
| `LayerGetter()` | constructor | the getter |
| `type` | property (read-only) | the node type; `0` by default (`reference/cpp/plugins/motionplayer/SourceCache.h:212`, `:241`) |
| `label` | property (read-only) | the node label, empty by default (`SourceCache.h:213`, `:242`) |
| `visible`, `branchVisible`, `layerVisible` | property (read-only) | the visibility flags, `true` by default (`SourceCache.h:214`, `:243`) |
| `x`, `y`, `left`, `top` | property (read-only) | the position (`SourceCache.h:217-220`, `:244`) |
| `flipX`, `flipY` | property (read-only) | the flips |
| `zoomX`, `zoomY` | property (read-only) | the zooms, `1.0` by default (`SourceCache.h:223`, `:246`) |
| `angleDeg`, `angleRad` | property (read-only) | the angle in both units |
| `slantX`, `slantY` | property (read-only) | the slant |
| `originX`, `originY` | property (read-only) | the origin |
| `opacity` | property (read-only) | the integer `0..255` opacity, `255` by default (`SourceCache.h:231`, `:250`) |
| `mtx` | property (read-only) | the node's matrix as a six-element array; void until a player fills it in (`reference/cpp/plugins/motionplayer/PlayerLayerQuery.cpp:48-71`, `SourceCache.h:251-253`) |
| `vtx` | property (read-only) | the node's vertices as an eight-element array |
| `color` | property (read-only) | the node's colour as a four-element array; a node without a colour of its own reports `(128, 128, 128, 255)` (`reference/cpp/plugins/motionplayer/PlayerLayerQuery.cpp:66-71`) |
| `bezierPatch`, `shape`, `motion`, `particle` | property (read-only) | the variant-valued members; nothing fills them, so they read back as `void` (`reference/cpp/plugins/motionplayer/SourceCache.h:235-238`) |

#### `Motion.EmotePlayer` — 181 members

`EmotePlayer` is a thin shell over `Player` in the reference
(`reference/cpp/plugins/motionplayer/EmotePlayer.h`); the port registers it
with the whole `Player` surface plus the shell members, which is the
inherited shape. The 181 members are the `Player` set (141), the inherited
`Player` constructor member, 24 shell methods and 15 shell properties.

| member | kind | behaviour |
|---|---|---|
| `create()` | method | drops the module and the motion state (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:183-187`) |
| `load(data)` | method | stores the module and activates its registered snapshot; arity 1 (`reference/cpp/plugins/motionplayer/EmotePlayer.h:134`, `EmotePlayer.cpp:189-196`) |
| `clone()` | method | a fresh `EmotePlayer` with the shell state copied over (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:198-253`) |
| `show()`, `hide()` | method | the shell visibility (`reference/cpp/plugins/motionplayer/EmotePlayer.h:138-139`) |
| `assignState(state)` | method | copies a serialized state into the shell (`reference/cpp/plugins/motionplayer/EmotePlayer.h:140`) |
| `setRot(angle, time, easing)` | method | the rotation; the emote entry point wants the angle alone (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:305-324`) |
| `getRot()` | method | the reference's hardcoded `0.0` (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:325-326`) |
| `setCoord(x, y, time, easing)` | method | the position; arity 2 (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:345-347`) |
| `setScale(scale, time, easing)` | method | the scale; arity 1 (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:375-377`) |
| `getScale()` | method | the reference's hardcoded `1.0` (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:387-388`) |
| `setColor(color, time, easing)` | method | the diffuse colour; arity 1 (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:415-417`) |
| `getColor()` | method | the reference's hardcoded `0` (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:426-427`) |
| `startWind(minAngle, maxAngle, amplitude, freqX, freqY)` | method | the wind; arity 5 (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:492-509`, `EmotePlayer.h:183-185`) |
| `stopWind()` | method | stops the wind (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:511-514`) |
| `setOuterForce(label, x, y, time, easing)` | method | the outer force of a category; arity 3 (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:853-879`) |
| `getOuterForce()` | method | the `bust` outer force as a dictionary (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:1666-1681`, `EmotePlayer.cpp:881-883`) |
| `skip()` | method | seeks to the sync point (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:800-812`) |
| `pass()` | method | the parameterless pass (`reference/cpp/plugins/motionplayer/main.cpp:388`) |
| `addPlayCallback(callback)` | method | registers a playback callback (`reference/cpp/plugins/motionplayer/main.cpp:387`) |
| `setTimeline(label, loop)` | method | starts the named timeline; arity 2 (`reference/cpp/plugins/motionplayer/EmotePlayer.h:217`, `EmotePlayer.cpp:633-636`) |
| `isTimelinePlaying()` | method | whether the emote timeline plays (`reference/cpp/plugins/motionplayer/main.cpp:376`) |
| `isLoopTimeline(label)` | method | whether a label is the loop timeline (`reference/cpp/plugins/motionplayer/main.cpp:371`) |
| `contains(x, y)` | method | the emote hit test; arity 2, or 3 with a layer label (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:913-937`) |
| `module` | property (read-only) | the loaded module object (`reference/cpp/plugins/motionplayer/main.cpp:303`) |
| `smoothing` | property (read/write) | the smoothing flag (`reference/cpp/plugins/motionplayer/main.cpp:317`) |
| `meshDivisionRatio` | property (read/write) | the mesh subdivision ratio (`reference/cpp/plugins/motionplayer/main.cpp:318`) |
| `queing` | property (read/write) | the reference's spelling of the queuing flag (`reference/cpp/plugins/motionplayer/main.cpp:319`) |
| `bodyScale` | property (read/write) | the body scale (`reference/cpp/plugins/motionplayer/main.cpp:323`) |
| `hairScale` | property (read/write) | the hair scale (`reference/cpp/plugins/motionplayer/main.cpp:320`) |
| `partsScale` | property (read/write) | the parts scale (`reference/cpp/plugins/motionplayer/main.cpp:321`) |
| `bustScale` | property (read/write) | the bust scale (`reference/cpp/plugins/motionplayer/main.cpp:322`) |
| `visible` | property (read/write) | the shell visibility (`reference/cpp/plugins/motionplayer/main.cpp:316`) |
| `modified` | property (read/write) | the dirty flag (`reference/cpp/plugins/motionplayer/main.cpp:326`) |
| `drawvisible` | property (read/write) | the draw visibility (`reference/cpp/plugins/motionplayer/main.cpp:327`) |
| `drawOpacity` | property (read/write) | the draw opacity (`reference/cpp/plugins/motionplayer/main.cpp:328`) |
| `opengl` | property (read/write) | the OpenGL flag (`reference/cpp/plugins/motionplayer/main.cpp:329`) |
| `animating` | property (read-only) | whether the animation runs (`reference/cpp/plugins/motionplayer/main.cpp:330`) |
| `playCallback` | property (read-only) | whether a playback callback is registered (`reference/cpp/plugins/motionplayer/main.cpp:331`) |

The reference registers both a `progress` property and a `progress(dt)` method
on this class (`main.cpp:325`); the member table holds one entry per name, so
the callable method wins.

### `D3DEmotePlayer` and `D3DEmoteModule`

`D3DEmotePlayer` is the reference's D3D subclass of `EmotePlayer`
(`reference/cpp/plugins/motionplayer/main.cpp:588-687`) and carries the same
181 members as `Motion.EmotePlayer`; it is a top-level class and is not a
`Motion` member.

`D3DEmoteModule` — 9 members, plus the class-name constructor:

| member | kind | behaviour |
|---|---|---|
| `D3DEmoteModule()` | constructor | the settings object (`reference/cpp/plugins/motionplayer/main.cpp:566`) |
| `setMaxTextureSize(width, height)` | method | a stub that stores nothing; arity 2 (`reference/cpp/plugins/motionplayer/D3DEmoteModule.h:37-40`, `main.cpp:585`) |
| `maskMode` | property (read/write) | the mask mode, `1` (`Motion.MaskModeAlpha`) by default (`reference/cpp/plugins/motionplayer/D3DEmoteModule.h:43-48`) |
| `maskRegionClipping` | property (read/write) | the region-clipping flag |
| `mipMapEnabled` | property (read/write) | the mip-map flag |
| `alphaOp` | property (read/write) | the alpha operation |
| `protectTranslucentTextureColor` | property (read/write) | the translucent-colour flag |
| `pixelateDivision` | property (read/write) | the pixelate division |
| `D3DEmoteModule.TimelinePlayFlagParallel` | static property (read-only) | `1` (`reference/cpp/plugins/motionplayer/main.cpp:568-569`) |
| `D3DEmoteModule.TimelinePlayFlagSequential` | static property (read-only) | `2` (`reference/cpp/plugins/motionplayer/main.cpp:570-571`) |

Every backing field of the reference class is `inline static`
(`reference/cpp/plugins/motionplayer/D3DEmoteModule.h:43-48`), so all
instances of the class share one value; the port keeps that shared state per
engine.

### Globals

The reference's post-registration callback defines two empty dictionaries on
the global object when they are unset, because an encrypted `keybinder.tjs`
dereferences them unconditionally
(`reference/cpp/plugins/motionplayer/main.cpp:534-544`):

* `global.ShortCutInitialPadKeyMap`
* `global.ShortCutInitialGamePadKeyMap`

They are ordinary writable members, so a game can replace them.

## Behaviour

### The motion model

A motion is a PSB container. The port parses it with the shared
[`psb_file`](psb_file.md) container parser, maps the tree onto a motion
snapshot and decodes the icon pixel chunks itself
(`crates/compat/src/natives/plugins/motion_player/model.rs`,
`texture.rs`). The snapshot holds the clips, the timelines, the variables and
the metadata; the player walks the active clip's `layer` array into a node
tree and evaluates it against the playing timeline's time
(`reference/cpp/plugins/motionplayer/NodeTree.cpp`, `MotionNode.h`).

Loading a motion registers the container's resources in the `psb://` storage
media under the container's name and under its final path component, so a
nested chunk is reachable by its `/`-joined member path
(`reference/cpp/plugins/motionplayer/RuntimeSupport.cpp:1425-1432`,
`reference/cpp/plugins/psbfile/PSBMediaRegistry.cpp:16-77`).

### The draw pipeline

`Player.draw(target)` evaluates the node tree and paints it into the target
layer's image through the engine's layer system:

* the target image is **not cleared** by the ordinary draw path: the pose is
  painted over whatever the target already holds, so pixels the pose does not
  cover survive (`reference/cpp/plugins/motionplayer/PlayerRenderTargets.cpp:1127-1131`,
  `PlayerRenderInternal.cpp:444-451`). The port clears the target only when
  `Player.clearEnabled` is set, using the clear colour.
* each icon source is multiplied by the node's four packed vertex colours,
  bilinearly interpolated across the source. The reference's default
  half-gray and opaque-white sets are neutral and leave the source untouched
  (`reference/cpp/plugins/motionplayer/PlayerRenderItems.cpp:599`,
  `SourceCache.cpp:44-55`, `:82-150`), and a `bm` carrying the `0x10` bit
  uses the 128 divisor (`SourceCache.cpp:719-729`).
* a **composite group** (a type-12 node whose `stencilType` has bit 2 set) is
  rendered into an offscreen the size of its paint box intersected with the
  nearest type-7 shape's viewport and the destination bounds, and composited
  once with the group's blend mode and accumulated opacity. A child's opacity
  is divided by the group's, so the group factor is applied exactly once.
  Preview playback does not splice the child motions.
* a composite group whose `stencilCompositeMaskLayerList` names layers renders
  each named layer's own source into a mask bitmap and applies the reference's
  `0x6AF104` alpha operation to the composed group before the composite
  (`reference/cpp/plugins/motionplayer/PlayerRenderInternal.cpp:623`). The
  group's raw `stencilType` selects the operation (`1` intersects, `2`
  intersects with the inverse, `5`/`6` union the alpha), `maskMode ==
  Motion.MaskModeStencil` cuts at the threshold 64 and `Motion.MaskModeAlpha`
  blends the alpha arithmetically, and a group whose low bits are `1` clears
  the alpha the mask does not cover. The mask layers are matched by label and
  the last node with that label wins.
* a **mesh node** (`meshTransform` 1 or 2) builds its subdivision grid from
  `meshDivision`, warps it through the previous frame's grid and every mesh
  ancestor's patch, and draws one affine cell per grid quad. `LayerGetter.vtx`
  reports the four deformed grid corners, and a child of a mesh node is warped
  through the parent's patch.
* each **type-3 node** owns a child clip instance. Its clock comes from the
  keyframe that is active now (`clipStartTime` and `motion.timeOffset`), so a
  later keyframe that names the same clip restarts it and a keyframe that
  names a different clip rebuilds the instance. The instance is placed at its
  motion layer's origin corner
  (`reference/cpp/plugins/motionplayer/PlayerUpdateChildMotion.cpp:106-215`).
* any item with a viewport (a type-7 shape ancestor) is clipped to its paint
  box intersected with that viewport; a trimmed item is rendered into a
  clip-sized offscreen and composited once.
* the vertex and paint-box geometry maps a node's depth (`z`) onto the screen
  `y` through the player's `zFactor`; the quad painter places the source at
  the node's accumulated position, and the stencil-mask sampler uses that
  same drawn box, so a mask samples the pixels its layer actually paints.
* `getLayerNames` reports the label map's keys: a layer without a label
  contributes none, and a label several nodes share appears once.
* the target layer's own geometry, opacity and visibility are left alone; the
  draw only writes pixels and marks the image modified
  (`reference/cpp/plugins/motionplayer/PlayerRenderTargets.cpp:1094-1135`,
  `PlayerDrawDispatch.cpp:236-285`).

A draw target is a `Motion.SeparateLayerAdaptor`, which paints into the layer
its live `targetLayer` names, or a `Layer` itself. A target that names no
layer leaves nothing to draw into and the call returns void without raising
(`reference/cpp/plugins/motionplayer/PlayerRenderTargets.cpp:1228-1252`).

### The playback model

* `progress(dt)` advances the playback by `dt` milliseconds. A delta outside
  the accepted window is **dropped**, not clamped, so one bad tick cannot skip
  the animation ahead (`reference/cpp/plugins/motionplayer/PlayerFrameProgress.cpp:1002-1004`).
* A motion that was just activated sits behind the **frame gate**: the tick
  that follows it only presents the first pose, and the layer update that runs
  with it releases the gate
  (`reference/cpp/plugins/motionplayer/PlayerFrameProgress.cpp:819-826`).
* `Player.loopTime` reads `0` until the first full frame has advanced, while
  `Player.playing` keeps its reference value.  KAG's `[ev storage]` image wait
  (`EnvLayerObject.createWait(1)`) reads `loopTime` through
  `AffineSourceMotion.canSync`; the neutral value keeps that wait from
  covering the movie.  The following `[ev waitmovie]`
  (`EnvLayerObject.createWait(2)`) reads the image's `_playing` field, which
  stays true, and waits the remaining playback (`system/world.tjs`,
  `system/affinesourcemotion.tjs`).
* `Player.progress(dt)` returns the playback ratio; the emote classes discard
  it (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:1281-1305`).
* The timelines, the variables and the transition animators are stepped
  together, and the action and sync keyframes of the active clip are
  dispatched to the receiver.
* `play(label, flags)` starts a clip; the flags are the `Motion.PlayFlag*`
  values. `Motion.PlayFlagForce` re-runs the load for the same key,
  `Motion.PlayFlagStealth` stores the motion without activating it while no
  project is bound, and a same-key request without force or as-can is ignored
  (`reference/cpp/plugins/motionplayer/PlayerMotionLoad.cpp:20-40`).
* `speed = false` freezes every clock: `progress` records the delta and
  advances nothing.

### Motion lookup

`motionKey` resolves through the storage. A name that carries a path
separator or an extension is looked up as it is; a bare name is expanded to
`<name>.mtn`, `<name>.psb`, `motion/<name>.mtn` and `motion/<name>.psb` in
that order, and the first mounted candidate wins
(`reference/cpp/plugins/motionplayer/RuntimeSupport.cpp:1319-1349`).

The `motion` property is the other entry point. Its setter builds
`{chara, motion}`, calls `onFindMotion` **through the object** — so a script
override runs — and reads the returned object back, so the override can
rewrite both the character and the motion
(`reference/cpp/plugins/motionplayer/PlayerCore.cpp:445-474`). On the emote
classes the setter only stores the clip label
(`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:167-171`).

`variableKeys` is the array of the active motion's variable labels. It reads
back as an empty array while no motion is bound, and the getter never
replaces a stored `void` with the motion's labels
(`reference/cpp/plugins/motionplayer/PlayerCore.cpp:1248-1254`).

### The resource manager

`Motion.ResourceManager.load(path)` reads the PSB through the storage,
registers the container's resources in the `psb://` media, builds the motion
snapshot and returns a module object. That object carries `path`, `metadata`
(the parsed metadata tree), `object` (the parsed root tree), `width` and
`height` (the logical canvas) and the internal snapshot handle
(`reference/cpp/plugins/motionplayer/ResourceManager.cpp:143-170`).

`Motion.Player` and `Motion.EmotePlayer` share the manager's snapshot store,
so a player and the manager it was built from observe the same data
(`crates/compat/src/natives/plugins/motion_player/resource.rs`).

### The emote shell

The emote classes add the shell state on top of the `Player` surface: the
position, scale, rotation and colour transitions, the wind, the outer force
and the dirty flag. The transitions are queued keyframes with an ease weight
in `[0, 1]`, stepped by the frame delta
(`crates/compat/src/natives/plugins/motion_player/animator.rs`).

`getRot`, `getScale` and `getColor` return the reference's hardcoded values
(`0.0`, `1.0` and `0`), because the reference's binary getters are stubs
(`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:325-326`, `:387-388`,
`:426-427`).

### Coordination with the emote player DLL

The reference splits the classes across two DLLs: `motionplayer.dll` and
`emoteplayer.dll`, the latter only loading the former
(`reference/cpp/plugins/EmotePlayer.cpp:5-8`). The compat VM rejects
duplicate class names, so the registration is idempotent: a class that
already exists is left alone and the subclasses are attached to the existing
`Motion` object. Running alongside the separate
[`emote_player`](emote_player.md) port therefore cannot make the installation
fail.

The `emoteplayer.dll` entry declares `motionplayer.dll` as its **dependency**,
so a script that links the loader shim gets this port registered first, and
the `Motion` namespace is on the VM by the time that link returns. Linking
`motionplayer.dll` directly registers this port on its own. The dependency is
not a link of its own, so `Plugins.getList` reports only the name the script
linked.

## State and lifetime

* Every class keeps its payload in its native instance: the player's motion
  state, the manager's cache and layer-id allocator, the adaptors' targets
  and the geometry classes' fields.
* The snapshot store, the decoded pixel cache, the PSB decryption seed and
  function, the `D3DEmoteModule` settings and the random generator live in
  the per-engine dispatch state, never in a process global
  (`crates/compat/src/natives/plugins/motion_player/resource.rs`).
* A player that has already loaded a snapshot keeps its own handle, so
  unloading the module from the manager does not disturb a motion in flight
  (`reference/cpp/plugins/motionplayer/RuntimeSupport.cpp:1575-1582`).
* The `Motion.Player.useD3D` probe marker is created once at registration
  and duplicated on every read, so its identity is stable across reads
  (`crates/compat/src/natives/plugins/motion_player/mod.rs`).
* The instance payload is released when the native instance is destroyed.

## Errors

| condition | outcome |
|---|---|
| a native member called with fewer arguments than the reference's arity | a typed callback error (`reference/cpp/core/plugin/ncbind.hpp:1178`) |
| a member that needs a loaded motion, called before one is bound | a typed error, never a substitute value |
| a PSB container that cannot be read, parsed or decrypted | the storage's or the container parser's error |
| a PSB decryption that fails | the decryption error |
| `Motion.ResourceManager.setEmotePSBDecryptSeed` with a non-integer | `invalid parameter` (`reference/cpp/plugins/motionplayer/ResourceManager.cpp:112-121`) |
| `new Motion.D3DAdaptor(...)` with fewer than five arguments or a first argument that is not a `Window` | `must set Window object` (`reference/cpp/plugins/motionplayer/D3DAdaptor.cpp:19-27`) |
| `Motion.D3DAdaptor.captureCanvas` with an argument that is not a `Layer` | `D3DAdaptor.captureCanvas expects a Layer` |
| the compat context is not bound | `motionplayer: compat context is not bound` |
| an icon whose texture cannot be decoded | a warning per missing source, once per source per motion, and the icon is skipped (`crates/compat/src/natives/plugins/motion_player/texture.rs`) |

A draw target that names no layer, a `Point` or `Quad` hit test and a
`Motion.Quad.p` read are the reference's own no-ops; they return void or
`false` instead of raising.

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/motion_player.rs` | the namespace constants and the subclass placement (top-level versus `Motion.*`), the geometry hit-test classes, the `LayerGetter` defaults and its array identity, the resource manager's layer-id allocator and the decrypt seed, the `D3DEmoteModule` class statics, the player's property surface, the variable and timeline queries, the arity gates, the `onFindMotion` override, the emote shell, the PSB module load and unload, the `motionKey` candidate list, the `ShortCutInitial*` globals, the `SeparateLayerAdaptor` owner, the layer getter, the colour-weight channel order, the progress gate and the dropped delta, the image-wait/movie-wait `canSync` split, and the draw path (texture sampling, the draw affine, the node colour tint, the uncleared target, the untouched target geometry, the reused allocation, the `psb://` media registration) |
| `crates/compat/tests/emote_player.rs` | the loader shim defines no globals of its own and the `motionplayer.dll` classes are reachable through it |
| `crates/compat/src/natives/plugins/motion_player/player.rs` unit tests | the replay of a short clip after a long one, the queuing gate and the dropped delta, the canSync loop-time gate before the first full frame, the reset of the evaluation time on replay, the nearest-texel sampling of a scaled placement, the child motion's start at its parent keyframe, the action and sync dispatch, the packed colour order, the shape hit test, the logo's held corner and transition, the inherited parent matrix, the node matrix order, the draw affine, the shared label and the layer getter list, and the node blend mode |
| `crates/compat/tests/plugin.rs` | that `Plugins.link("motionplayer.dll")` resolves to the built-in table |

## Known gaps

* **The rendering backend differs.** The reference draws through Direct3D on
  Windows; the port draws through the engine's layer system. Every value a
  script can observe — the member getters and setters, the return values, the
  raised errors, `typeof`, the string and number conversions — matches the
  reference, but the rendered mesh is the engine's rasterization of the same
  data rather than the reference's D3D output.
* **Mesh rasterization is piecewise affine.** The grid geometry, the
  previous-frame warp, the ancestor cascade, the moved origin, the child
  deformation and `vtx` follow the reference, but the texture is drawn with
  one affine blit per grid quad, and the sampling is nearest-texel
  (`reference/cpp/plugins/motionplayer/PlayerRenderExecute.cpp:455-456`); the
  reference's `BezierPatchCopy`/`MeshCopy` interpolates each cell per pixel.
* **The D3D adaptor route is not wired.** `Motion.D3DAdaptor` registers its
  whole surface and validates its constructor arguments, but the draw path
  only resolves a `Motion.SeparateLayerAdaptor` or a `Layer`; a D3D adaptor
  is not a draw target.
* `Player.useD3D` is a script-settable flag that the draw path does not
  consult: the port always draws through the engine's layer system. The
  reference on Windows switches to the D3D backend when the flag is set and
  the target is a D3D adaptor
  (`reference/cpp/plugins/motionplayer/PlayerDrawDispatch.cpp:157-173`,
  `:246-248`).
* `content.mesh.bp` is not read. The reference warps a child through the
  parent's grid, which is what the port does; the authored patch the field
  declares has no reader in the reference either.
* `D3DEmoteModule.maskMode` is a per-engine class static; it is not copied
  into a player at construction, so the mask operation follows the player's
  own `maskMode` (the reference `Player._maskMode`, default
  `Motion.MaskModeStencil`).
* The compat class-registration API always publishes a global, so the port
  registers each namespace class, aliases it into `Motion` and then deletes
  the registration global. The observable result is the reference's shape —
  the namespace-only classes are reachable only as `Motion.*` — and the
  surface lock holds no bare `EmotePlayer`, `Point`, `Circle`, `Quad`,
  `LayerGetter` or `ResourceManager` global.
* `Motion.Rect` is registered as `MotionRect` internally, because the core
  VM already owns the `Rect` class; the script-visible name is `Motion.Rect`,
  and the constructor member is `MotionRect`.
* `Motion.ResourceManager`'s cache-size argument is accepted but no cache
  eviction is implemented, so the argument has no observable effect
  (`reference/cpp/plugins/motionplayer/ResourceManager.h:17`).
* `getCommandList` returns an empty array, and `Player.random` draws from the
  engine's own generator rather than the reference's `Math.RandomGenerator`
  instance; the observable contract — a fresh real in `[0, 1)` per call — is
  the same (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:1318-1332`).
