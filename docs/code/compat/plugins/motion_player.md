# motionplayer (`motionplayer.dll`)

**DLL name:** `motionplayer.dll` — linked with `Plugins.link("motionplayer.dll")`
**Rust module:** `crates/compat/src/natives/plugins/motion_player/`
**Reference:** `reference/cpp/plugins/motionplayer/` (`main.cpp`, `Player.*`, `MotionNode.h`, `NodeTree.cpp`, `ResourceManager.*`, `manual.tjs`)
**Registers:** `Motion`, `Player`, and the `Motion.*` subclasses; the emote classes

## Surface

Only `Player`, `Motion`, `D3DEmotePlayer` and `D3DEmoteModule` are top-level
classes in the reference (`reference/cpp/plugins/motionplayer/main.cpp:128`,
`:427`, `:565`, `:588`). The remaining classes are registered through the
global-publishing ABI and their global is deleted right after they are
aliased into `Motion`, so they exist only as `Motion.*`.

### `Motion` namespace

The namespace carries the reference's constant groups:

| group | members |
|---|---|
| layer types | `LayerTypeObj`, `LayerTypeShape`, `LayerTypeLayout`, `LayerTypeMotion`, `LayerTypeParticle`, `LayerTypeCamera` |
| shape types | `ShapeTypePoint`, `ShapeTypeCircle`, `ShapeTypeRect`, `ShapeTypeQuad` |
| play flags | `PlayFlagForce`, `PlayFlagChain`, `PlayFlagAsCan`, `PlayFlagJoin`, `PlayFlagStealth` |
| transform order | `TransformOrderFlip`, `TransformOrderAngle`, `TransformOrderZoom`, `TransformOrderSlant` |
| coordinate system | `CoordinateRecutangularXY`, `CoordinateRecutangularXZ` |
| mask mode | `MaskModeStencil`, `MaskModeAlpha` |

The namespace also holds the aliased subclasses: `Motion.Player`,
`Motion.ResourceManager`, `Motion.SeparateLayerAdaptor`, `Motion.D3DAdaptor`,
`Motion.SourceCache`, `Motion.ObjSource`, `Motion.Point`, `Motion.Circle`,
`Motion.Rect`, `Motion.Quad`, `Motion.LayerGetter` and
`Motion.EmotePlayer`.

### `Player`

The class carries the reference's full member set (141 members).

#### Methods

| member | behaviour |
|---|---|
| `Player()` | the constructor |
| `random()` | a random value from the player's own generator |
| `initPhysics(...)` | initialises the physics model |
| `serialize()` | serializes the player's state |
| `unserialize(...)` | restores a serialized state |
| `setRotate(...)`, `setMirror(...)`, `setHairScale(...)`, `setPartsScale(...)`, `setBustScale(...)` | the body-part transforms |
| `setDrawAffineTranslateMatrix(...)` | the draw-time affine translation |
| `getCameraOffset()`, `setCameraOffset(...)` | the camera offset |
| `modifyRoot(...)` | modifies the root node |
| `debugPrint()` | dumps the player's state |
| `unload()`, `unloadAll()` | release the loaded resources |
| `isExistMotion(name)`, `findMotion(name)` | look a motion up |
| `requireLayerId(...)`, `releaseLayerId(...)` | the layer-id lease |
| `setClearColor(...)`, `setResizable(...)` | the canvas setup |
| `removeAllTextures()`, `removeAllBg()`, `removeAllCaption()`, `registerBg(...)`, `registerCaption(...)`, `unloadUnusedTextures()` | the texture and background registries |
| `alphaOpAdd(...)`, `captureCanvas()` | the alpha and canvas operations |
| `findSource(name)`, `loadSource(name)`, `clearCache()` | the source cache |
| `setSize(...)`, `copyRect(...)`, `adjustGamma(...)` | the canvas geometry |
| `draw(...)` | draws the player |
| `frameProgress(...)` | advances the playback by a frame count |
| `setFlip(...)`, `setOpacity(...)`, `setVisible(...)`, `setSlant(...)`, `setZoom(...)` | the layer appearance |
| `getLayerNames()`, `releaseSyncWait(...)`, `calcViewParam(...)` | the layer queries |
| `getLayerMotion(...)`, `getLayerGetter(...)`, `getLayerGetterList(...)` | the per-layer queries |
| `skipToSync(...)` | skips to the next sync point |
| `setStereovisionCameraPosition(...)` | the stereovision camera |
| `getVariable(...)`, `countVariables()`, `getVariableLabelAt(...)`, `countVariableFrameAt(...)`, `getVariableFrameLabelAt(...)`, `getVariableFrameValueAt(...)`, `getTimelinePlaying(...)`, `getVariableRange(...)`, `getVariableFrameList(...)` | the variable queries |
| `countMainTimelines()`, `getMainTimelineLabelAt(...)`, `getMainTimelineLabelList(...)` | the main timelines |
| `countDiffTimelines()`, `getDiffTimelineLabelAt(...)`, `getDiffTimelineLabelList(...)` | the difference timelines |
| `getLoopTimeline(...)`, `countPlayingTimelines()`, `getPlayingTimelineLabelAt(...)`, `getPlayingTimelineFlagsAt(...)` | the playing timelines |
| `getTimelineTotalFrameCount(...)` | a timeline's length |
| `playTimeline(...)`, `stopTimeline(...)`, `setTimelineBlendRatio(...)`, `getTimelineBlendRatio(...)`, `fadeInTimeline(...)`, `fadeOutTimeline(...)`, `getPlayingTimelineInfoList(...)` | the timeline controls |
| `isSelectorTarget(...)`, `deactivateSelectorTarget(...)` | the selector |
| `getCommandList(...)` | the command list |
| `getD3DAvailable()` | whether the D3D backend is available |
| `doAlphaMaskOperation(...)` | the alpha-mask operation |
| `onFindMotion(...)` | the motion-found callback |
| `motionList()` | the available motions |
| `emoteEdit(...)` | the emote editor |
| `setVariable(...)` | writes a variable |
| `play(...)`, `progress(...)`, `isPlaying()`, `stop()` | the playback controls |

#### Properties

| member | behaviour |
|---|---|
| `x`, `y`, `left`, `top` | the player's position |
| `completionType`, `metadata` | the playback metadata |
| `chara`, `motion`, `motionKey` | the current character and motion |
| `outline` | the outline setting |
| `priorDraw` | the draw-order flag |
| `frameLastTime`, `frameLoopTime`, `loopTime` | the frame timing |
| `processedMeshVerticesNum` | the mesh vertex count |
| `playing`, `queuing`, `directEdit`, `selectorEnabled` | the playback state |
| `variableKeys` | the variable names |
| `allplaying`, `syncWaiting`, `syncActive`, `hasCamera`, `cameraActive`, `stereovisionActive` | the sync and camera state |
| `tickCount`, `speed`, `frameTickCount` | the clock |
| `maskMode`, `colorWeight`, `independentLayerInherit`, `zFactor` | the compositing |
| `cameraTarget`, `cameraPosition`, `cameraFOV`, `cameraAlive` | the camera |
| `canvasCaptureEnabled`, `clearEnabled`, `hitThreshold`, `preview`, `outsideFactor` | the canvas behaviour |
| `resourceManager` | the resource manager |
| `stealthChara`, `stealthMotion` | the stealth mode |
| `tags`, `project` | the tags and the project |
| `useD3D` | whether the D3D backend is used |
| `meshline`, `busy` | the mesh line and the busy flag |
| `smoothing`, `meshDivisionRatio`, `queing`, `bodyScale`, `hairScale`, `partsScale`, `bustScale` | the mesh and body scales |
| `visible`, `modified`, `drawvisible`, `drawOpacity`, `opengl` | the draw state |
| `animating`, `playCallback` | the emote state |

`Player.useD3D` exists in two places: an **instance** property on the player
and a **class-static** probe object on the `Player` class object
(`reference/cpp/plugins/motionplayer/main.cpp:498-509`). The reference's
post-registration callback attaches an empty dictionary to the `Player` class
with `TJS_STATICMEMBER`, so `Motion.Player.useD3D` reads as an object; the
port creates the marker once and duplicates it on every read so its identity
is stable.

### `EmotePlayer` and `D3DEmotePlayer`

`EmotePlayer` is a thin shell over `Player` in the reference
(`reference/cpp/plugins/motionplayer/EmotePlayer.h`); `D3DEmotePlayer` is its
Windows D3D subclass. The port registers both with the same callbacks as
`Player` plus the EmotePlayer shell members, which is exactly the inherited
shape.

The shell members are:

| member | kind | behaviour |
|---|---|---|
| `create`, `load`, `clone`, `show`, `hide`, `assignState` | methods | the emote lifecycle |
| `setRot`, `getRot`, `setCoord`, `setScale`, `getScale`, `setColor`, `getColor` | methods | the emote placement |
| `startWind`, `stopWind`, `setOuterForce`, `getOuterForce` | methods | the wind and the outer force |
| `skip`, `pass`, `addPlayCallback`, `setTimeline` | methods | the playback control |
| `isTimelinePlaying`, `isLoopTimeline`, `contains` | methods | the state queries |
| `module`, `smoothing`, `meshDivisionRatio`, `queing`, `bodyScale`, `hairScale`, `partsScale`, `bustScale`, `visible`, `modified`, `drawvisible`, `drawOpacity`, `opengl`, `animating`, `playCallback` | properties | the emote state |

`D3DEmotePlayer` is a genuine top-level class in the reference and is not a
namespace member.

### `D3DEmoteModule`

| member | kind | behaviour |
|---|---|---|
| `D3DEmoteModule()` | constructor | the settings object |
| `setMaxTextureSize(...)` | method | sets the maximum texture size |
| `maskMode` | property (read/write) | the mask mode |
| `maskRegionClipping` | property (read/write) | the region-clipping flag |
| `mipMapEnabled` | property (read/write) | the mip-map flag |
| `alphaOp` | property (read/write) | the alpha operation |
| `protectTranslucentTextureColor` | property (read/write) | the translucent-colour flag |
| `pixelateDivision` | property (read/write) | the pixelate division |
| `D3DEmoteModule.TimelinePlayFlagParallel` | static property | `1` |
| `D3DEmoteModule.TimelinePlayFlagSequential` | static property | `2` |

The reference declares every backing field `inline static`
(`reference/cpp/plugins/motionplayer/D3DEmoteModule.h:43-48`), so all
instances of the class share one value and the initial values are the
class-wide defaults (`maskMode` starts at `1` = `MaskModeAlpha`). The port
keeps the shared state per engine.

### `Motion.ResourceManager`

| member | kind | behaviour |
|---|---|---|
| `ResourceManager()` | constructor | the manager |
| `load(...)` | method | loads a resource |
| `loadSource(...)` | method | loads a source |
| `unload(...)` | method | unloads a resource |
| `clearCache()` | method | clears the cache |
| `findSource(name)` | method | finds a source |
| `requireLayerId(...)`, `releaseLayerId(...)` | methods | the layer-id lease |
| `ResourceManager.setEmotePSBDecryptSeed(...)` | static method | sets the PSB decryption seed |
| `ResourceManager.setEmotePSBDecryptFunc(...)` | static method | sets the PSB decryption function |

### `Motion.SeparateLayerAdaptor`

| member | kind | behaviour |
|---|---|---|
| `SeparateLayerAdaptor()` | constructor | the adaptor |
| `clear()` | method | clears the adaptor |
| `assign(...)` | method | assigns a layer tree |
| `layerTreeOwnerInterface` | property (read-only) | the layer-tree owner |
| `absolute` | property (read/write) | the absolute-mode flag |
| `targetLayer` | property (read/write) | the target layer |

### `Motion.D3DAdaptor`

| member | kind | behaviour |
|---|---|---|
| `D3DAdaptor()` | constructor | the adaptor |
| `setPos(...)`, `setSize(...)`, `setClearColor(...)`, `setResizable(...)` | methods | the canvas setup |
| `removeAllTextures()`, `removeAllBg()`, `removeAllCaption()`, `registerBg(...)`, `registerCaption(...)`, `unloadUnusedTextures()` | methods | the texture registries |
| `captureCanvas()` | method | captures the canvas |
| `visible`, `alphaOpAdd`, `canvasCaptureEnabled`, `clearEnabled` | properties (read/write) | the canvas state |

### `Motion.SourceCache`

| member | kind | behaviour |
|---|---|---|
| `SourceCache()` | constructor | the cache |
| `loadSource(...)` | method | loads a source |
| `clearCache()` | method | clears the cache |
| `bufLayer` | property (read-only) | the buffer layer |

### `Motion.ObjSource`

| member | kind | behaviour |
|---|---|---|
| `ObjSource()` | constructor | the object source |

### The geometry classes

`Motion.Point`:

| member | kind | behaviour |
|---|---|---|
| `Point()` | constructor | the point |
| `contains(...)` | method | whether the point contains a coordinate |
| `type` | property (read-only) | the shape type |
| `x`, `y` | properties (read/write) | the coordinates |

`Motion.Circle`:

| member | kind | behaviour |
|---|---|---|
| `Circle()` | constructor | the circle |
| `contains(...)` | method | whether the circle contains a coordinate |
| `type` | property (read-only) | the shape type |
| `x`, `y`, `r` | properties (read/write) | the centre and the radius |

`Motion.Rect` (registered internally as `MotionRect`, because the core VM
already owns the `Rect` class):

| member | kind | behaviour |
|---|---|---|
| `MotionRect()` | constructor | the rectangle |
| `contains(...)` | method | whether the rectangle contains a coordinate |
| `type` | property (read-only) | the shape type |
| `l`, `t`, `w`, `h` | properties (read/write) | the edges and the size |

`Motion.Quad`:

| member | kind | behaviour |
|---|---|---|
| `Quad()` | constructor | the quad |
| `contains(...)` | method | whether the quad contains a coordinate |
| `type` | property (read-only) | the shape type |
| `p` | property (read/write) | the points |

### `Motion.LayerGetter`

| member | kind | behaviour |
|---|---|---|
| `LayerGetter()` | constructor | the getter |
| `type` | property (read-only) | the layer type |
| `label` | property (read-only) | the layer label |
| `visible`, `branchVisible`, `layerVisible` | properties (read-only) | the visibility flags |
| `x`, `y`, `left`, `top` | properties (read-only) | the position |
| `flipX`, `flipY`, `zoomX`, `zoomY` | properties (read-only) | the flips and the zooms |
| `angleDeg`, `angleRad` | properties (read-only) | the angle |
| `slantX`, `slantY` | properties (read-only) | the slant |
| `originX`, `originY` | properties (read-only) | the origin |
| `opacity` | property (read-only) | the opacity |
| `mtx`, `vtx` | properties (read-only) | the matrix and the vertices |
| `color` | property (read-only) | the colour |
| `bezierPatch`, `shape`, `motion`, `particle` | properties (read-only) | the layer's payload |

### Globals

The reference's post-registration callback defines two empty dictionaries on
the global object when they are unset, because an encrypted `keybinder.tjs`
dereferences them unconditionally
(`reference/cpp/plugins/motionplayer/main.cpp:534-544`):

* `global.ShortCutInitialPadKeyMap`
* `global.ShortCutInitialGamePadKeyMap`

## Behaviour

The reference is a large PSB-driven mesh and physics renderer. The port keeps
the complete script-visible surface and the playback model — timelines,
variables, transitions, the wind and the outer force, the layer queries and
the particles — and composes the engine's `Scene` layer system for drawing
instead of re-implementing the D3D rasterizer.

PSB data is parsed by the shared [`psb_file`](psb_file.md) container parser,
mapped to the runtime model, and its pixel chunks are decoded by the texture
decoder.

### The draw pipeline

`Player.draw` evaluates the node tree and paints each item through the engine
`Scene` bitmaps:

* a type-12 node with `stencilType` bit 2 is a **composite group**. Its type-0
  children and the layers spliced from its type-3 child motions are its items.
  The group is rendered into an offscreen bitmap the size of its paint box
  intersected with the nearest type-7 shape's viewport and the destination
  bounds, then composited once with the group's blend mode and accumulated
  opacity. A child's opacity is divided by the group's, so the group factor is
  applied exactly once. Preview playback does not splice the child motions.
* a composite group whose `stencilCompositeMaskLayerList` names layers renders
  each named layer's own source into a mask bitmap and applies the reference
  `0x6AF104` alpha operation to the composed group before the composite. The
  group's raw `stencilType` selects the operation (`1` intersects, `2`
  intersects with the inverse, `5`/`6` union the alpha), `maskMode ==
  MaskModeStencil` cuts at the threshold 64 and `MaskModeAlpha` blends the
  alpha arithmetically, and a group whose low bits are `1` clears the alpha the
  mask does not cover. The mask layers are matched by label, last node wins,
  exactly like the reference's node label map.
* a mesh node (`meshTransform` 1) builds its subdivision grid from
  `meshDivision`, warps it through the previous frame's grid and every mesh
  ancestor's patch, and draws one affine cell per grid quad. `LayerGetter.vtx`
  reports the four deformed grid corners, and a child of a mesh node is warped
  through the parent's patch.
* each type-3 node owns a **child clip instance**. Its clock comes from the
  keyframe that is active now (`clipStartTime` and `motion.timeOffset`), so a
  later keyframe that names the same clip restarts it; a keyframe that names a
  different clip rebuilds the instance. The instance is placed at its motion
  layer's origin corner.
* any item with a viewport (a type-7 shape ancestor) is clipped to its paint
  box intersected with that viewport; a trimmed item is rendered into a
  clip-sized offscreen and composited once.
* the vertex and paint-box geometry maps a node's depth (`z`) onto the screen
  `y` through the player's `zFactor`; the quad painter places the source at
  the node's accumulated position, and the stencil-mask sampler uses that
  same drawn box, so a mask samples the pixels its layer actually paints.
* `getLayerNames` reports the label map's keys: a layer without a label
  contributes none, and a label several nodes share appears once.

### Coordination with the emote player DLL

The reference splits the classes across two DLLs (`motionplayer.dll` and
`emoteplayer.dll`); the compat VM rejects duplicate class names, so
`register` is **idempotent**: it skips any class that already exists and
attaches its subclasses to the existing `Motion` object. Running alongside
the separate [`emote_player`](emote_player.md) port therefore cannot make the
installation fail.

### The namespace classes

The reference's `NCB_REGISTER_SUBCLASS_DELAY` only defines their members, and
`NCB_SUBCLASS` attaches them under `Motion` as a `TJS_STATICMEMBER`
(`reference/cpp/core/plugin/ncbind.hpp:1963-1970`, `:1982-1987`,
`reference/cpp/plugins/motionplayer/main.cpp:427-441`). The compat ABI always
publishes a global for a registration, so the port registers each subclass
and deletes that global after aliasing it into `Motion`. The idempotency
therefore keys on the `Motion` member, not on the deleted global.

`Player` is the only aliased class that keeps its global.

## State and lifetime

* Each class keeps its payload in the native instance.
* The playback state, the model, the animator, the resource store and the
  source cache live in the per-engine dispatch state (`MotionStore`),
  never in a process global.
* `D3DEmoteModule`'s settings are shared per engine, matching the
  reference's `inline static` fields.
* The PSB decryption seed and function live in the per-engine store.

## Errors

| condition | outcome |
|---|---|
| a member that needs a loaded resource, called before one is loaded | typed error |
| a PSB container that cannot be parsed or decompressed | the container parser's typed error |
| a decryption that fails | the decryption error |
| `EmotePlayer.load` on a source that does not exist | typed error |
| the compat context is not bound | typed error |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/motion_player.rs` | the class surface, the namespace constants, the playback model, the timelines and the variables, the layer queries, the resource manager and the source cache, the drawing path (clip selection, draw affine, clear, the stencil groups of the committed e-mote module) |
| `crates/compat/src/natives/plugins/motion_player/player.rs` unit tests | the six-clip motion-key selection, the per-keyframe child clip instance, the composite group's single opacity and viewport clip, the stencil mask list, and the mesh grid/`vtx`/child deformation |
| `crates/compat/tests/emote_player.rs` | the `EmotePlayer` / `D3DEmotePlayer` shell, the `D3DEmoteModule` settings, and the idempotent registration alongside the loader DLL |

## Known gaps

* **The rendering backend differs.** The reference draws through Direct3D on
  Windows; the port draws through the engine's `Scene` layer system. Every
  value a script can observe — the member getters and setters, the return
  values, the raised errors, `typeof`, the string and number conversions —
  matches the reference, but the rendered mesh is the engine's rasterization
  of the same data rather than the reference's D3D output.
* **Mesh rasterization is piecewise affine.** The grid geometry, the
  previous-frame warp, the ancestor cascade, the moved origin, the child
  deformation and `vtx` follow the reference, but the texture is drawn with one
  affine blit per grid quad; the reference's `BezierPatchCopy`/`MeshCopy`
  interpolates each cell per pixel.
* `content.mesh.bp` is not read. The reference's `sub_69AE74` warps a child
  through the parent's grid, which is what the port does; the authored patch
  the field declares has no reader in the reference either.
* `D3DEmoteModule.maskMode` is a per-engine class static; it is not copied into
  a player at construction, so the mask operation follows the player's own
  `maskMode` (the reference `Player._maskMode`, default `MaskModeStencil`).
* `Player.useD3D` reports that the D3D backend is unavailable, which is true
  on this host; the reference on Windows would report otherwise.
* The reference's nested classes are published only under `Motion`; the port
  registers them as globals first and then aliases them, so the bare names
  exist briefly during registration. The final script-visible shape is the
  nested one, and the extra top-level names are locked into the surface
  baseline.
* `Motion.Rect` is registered as `MotionRect` internally, because the core
  VM already owns the `Rect` class; the script-visible name is `Motion.Rect`.
