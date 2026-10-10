# Audit — object writes/calls and how the receiver is derived

# Audit — object writes/calls and how the receiver is derived

> **Status update — the motivating hypothesis was disproven by measurement.** gdb on the real
> game established the layer identities: layer 2 = the `Logo` (the enclosing object), layer **21**
> = `_logo` (the `Sprite` itself), layer 22 = `_logo._image` (the inner `AffineLayer` image).
> `with(_logo){ .visible = true }` writes **layer 21 — the correct object**; `loadImages` reaches
> layer 22 only because `AffineLayer.loadImages` forwards to its `_image`. The real white-screen
> cause was unrelated: `CallOnPaint`/`pending_paint` was set and **never dispatched**, so
> `AffineLayer.onPaint` never composited `_image` onto the visible layer (fixed in L3 as
> `compat::paint_poll`, `LayerIntf.cpp:6401-6402`).
>
> Consequences for this document: the mechanism in §1 is real (the VM's
> `ObjThis ? ObjThis : ra[-1]` fallback exists), but it **did not fire** in the observed case, and a
> closure whose `ObjThis` is the intended object behaves correctly. The "suspect" verdicts below
> are therefore **hypotheses with no observed instance** — the `crates/compat/tests/dispatch_*.rs`
> suites verify the receiver per family, and a family should be treated as affected only once one
> of those tests fails.

Scope: every script‑visible member **write** or **call** that reaches an object in L3, and
how the *receiver* is derived. Read‑only audit; the motivating bug is being fixed in
`crates/compat` separately. This document only *classifies* the pattern, it does not propose
a patch.

Motivating measurement (from the task): `system/Title.tjs` does
`_logo = new Sprite(win, this)` and then, in a timer‑invoked function,
`with(_logo){ .visible = true; }`. With instance‑pointer instrumentation:
`with(_logo){ .loadImages(…) }` during construction reaches layer 22 (the Sprite);
the timer's `with(_logo){ .visible = true }` reaches layer 21 (the Logo — the enclosing
object), so layer 22 never becomes visible.

---

## 1. Mechanism

Native dispatch has exactly **one receiver channel**: the `iTJSDispatch2 *objthis`
argument the VM passes to `PropGet`/`PropSet`/`FuncCall`. The C ABI does not carry the
closure's `Object` (the object the member was actually found on); it carries only the
receiver. Consequently every native member family must recover its target from that one
pointer, and any VM path that supplies the wrong pointer misroutes the write.

The VM derives that pointer in `tTJSInterCodeContext::SetPropertyDirect`
(`crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp:1440`, identical to
`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:1430`):

```cpp
tTJSVariantClosure clo = ra_code1->AsObjectClosureNoAddRef();
tjs_error hr = clo.PropSetByVS(flags, name->AsStringNoAddRef(),
                               TJS_GET_VM_REG_ADDR(ra, code[3]),
                               clo.ObjThis ? clo.ObjThis : ra[-1].AsObjectNoAddRef());
```

`ra[-1]` is the *executing function's* `this` (`tjsInterCodeExec.cpp:719`,
`ra[-1].SetObject(objthis, objthis)`). `tTJSVariantClosure::PropSet` then forwards
`ObjThis ? ObjThis : (objthis ? objthis : Object)` (`crates/tjs/cpp/tjs2/tjsVariant.h:261-269`,
`reference/cpp/core/tjs2/tjsVariant.h:261`). So a receiver is taken, in priority order, from:

1. the closure's own `ObjThis` (the object the member reference was read from),
2. else the current function's `this` (`ra[-1]`),
3. else the closure's `Object`.

A `with(obj)` block stores the referenced value in a frame register
(`T_WITHDOT_PROXY` returns `data.RefRegister`,
`crates/tjs/cpp/tjs2/tjsInterCodeGen.cpp:2523-2534`; `EnterWithCode` writes it at
`tjsInterCodeGen.cpp:3309-3330`). When that register holds a plain object / a closure
whose `ObjThis` is not the intended object, rule (2) leaks the *enclosing* function's
`this` into the write. A method call reads the same fallback
(`CallFunctionDirect`, `tjsInterCodeExec.cpp:2161`; `CallFunction`, `:2113`), so the
call/‑write asymmetry must come from how the member reference itself carries `ObjThis`
(`ObjThis` for a call vs. for the member write), not from `with` compilation per se — see
§4 for what could not be pinned down without running code.

The ABI then makes the wrong receiver unrecoverable:

* **Instance classes** (`tjs2_register_native_class_instance`): the Rust payload is looked
  up **only** from `objthis` via `NativeInstanceSupport(TJS_NIS_GETINSTANCE, classid)`.
  Method dispatch: `crates/tjs/cpp/tjs2_abi.cpp:1136`, payload read `:1144`, passed to Rust
  `:1170`. Property set: `crates/tjs/cpp/tjs2_abi.cpp:1327`, payload read `:1330`, passed
  `:1345`. The Rust callback's `objthis` field is only informative; the write target is the
  `instance` payload derived from `objthis` (`crates/tjs/cpp/tjs2_abi.h:429-446`).
* **Objthis members** (`tjs2_register_native_instance_members`, `tjs2_abi.h:514-535`):
  the raw `objthis` is handed straight to Rust. Method: `tjs2_abi.cpp:825`
  (`self->fn(e, (void *)objthis, …)`). Property set: `tjs2_abi.cpp:946`
  (`self->set(e, (void *)objthis, &value, &out_error)`).
* **Static members** (`tjs2_register_native_class_ex` /
  `tjs2_register_native_static_members`): `objthis` is discarded —
  `tjs2_abi.cpp:569` (`(void)objthis; // static methods only`),
  `:671` / `:718` for static properties. Receiver is engine/global state.

So the question “does this site repeat the bug?” reduces to: *does the site's target come
from `objthis`?* If yes, it inherits the VM's receiver resolution and is suspect. If it is
static, or resolves the target from an explicit argument, it is not exposed to this specific
bug.

---

## 2. ABI surface — argument order and receiver source

Reference the C header `crates/tjs/cpp/tjs2_abi.h` and the Rust mirror
`crates/tjs/src/lib.rs`.

| C++ dispatch (tjs2_abi.cpp) | Rust callback type (lib.rs) | receiver passed to Rust | receiver source |
|---|---|---|---|
| `tjs2_dispatch_native_method` (`:564`) | `NativeMethodFn` | none | **dropped** (`:569`) |
| `tjs2_dispatch_native_property_get/set` (`:668`,`:715`) | `NativePropertyGet/SetFn` | none | **dropped** (`:671`,`:718`) |
| `tjs2_dispatch_objthis_method` (`:796`) | `NativeObjThisMethodFn` (`:166`) | `(void*)objthis` (`:825`) | raw `objthis` |
| `tjs2_dispatch_objthis_property_get/set` (`:891`,`:933`) | `NativeObjThisPropertyGet/SetFn` (`:184`,`:193`) | `(void*)objthis` (`:912`,`:946`) | raw `objthis` |
| `tjs2_dispatch_native_instance_method` (`:1123`) | `NativeInstanceMethodFn` (`:109`) | `instance` (`:1170`), `objthis` appended as last arg | `objthis -> NativeInstanceSupport(classid)` (`:1136`) |
| `tjs2_dispatch_native_instance_property_get/set` (`:1247`,`:1309`) | `NativeInstancePropertyGet/SetFn` (`:130`) | `instance` (`:1287`/`:1345`), `objthis` appended | `objthis -> NativeInstanceSupport(classid)` (`:1263`,`:1327`) |
| `tjs2_native_instance_constructor_dispatch::FuncCall` (`:1392`) | `NativeInstanceMethodFn` | `instance` (`:1451`) | `objthis -> NativeInstanceSupport(GET/REGISTER)` (`:1408`,`:1420`) |

Reference behaviour for the same shape: the reference macros `TJS_GET_NATIVE_INSTANCE` /
`TJS_GET_NATIVE_INSTANCE_OUTER` also resolve the instance from `objthis`
(`reference/cpp/core/tjs2/tjsNative.h:298-322`); the property/static registration macros are
`TJS_END_NATIVE_PROP_DECL` / `TJS_END_NATIVE_STATIC_PROP_DECL` (`tjsNative.h:422-444`).
The compat instance families are therefore **reference‑faithful** in their use of
`objthis`; the exposure is that neither side can see the closure's real `Object`.

One compat‑specific deviation worth flagging: the reference's native‑class instance
construction copies every non‑static member onto the new object and rebinds its `ObjThis`
to the object (`tTJSNativeClass::FuncCall`,
`reference/cpp/core/tjs2/tjsNative.cpp:305-375`, in particular the
`val.ChangeClosureObjThis(Dest)` at `:353`). Compat's class‑name constructor member
(`tjs2_native_instance_constructor_dispatch`, `tjs2_abi.cpp:1392-1470`; class at `:1021`)
registers the
native instance and calls the Rust constructor but does **not** perform that member copy.
Direct `new Layer(...)` still goes through `tTJSNativeClass::FuncCall`, which does copy;
`super.Layer()` / script subclass construction goes through the constructor member, which
does not. That is a concrete compat‑side candidate for the observed script‑subclass
receiver leak (see §4).

---

## 3. Trampoline families in `crates/compat/src`

Legend: **instance** = Rust uses the `instance` payload (C++ derived it from `objthis`);
**objthis** = Rust uses the raw `objthis`; **static** = no receiver. Risk is about the
*receiver*, under a `with`‑proxy write or an un‑copied script subclass.

### 3.1 Instance‑payload families (`instance` used, `_objthis` ignored) — suspect

| site (file:line) | family | receiver | risk | reference | repro plan |
|---|---|---|---|---|---|
| `visual/layer/mod.rs:1071` | `layer_int_prop!` (visible/type/left/top/width/height/absolute/cursor/face/…) | `instance` -> scene layer | **suspect** (the measured bug) | `LayerIntf.cpp:10592` (`visible` uses `TJS_GET_NATIVE_INSTANCE`) | the exact `Title.tjs` shape |
| `visual/layer/mod.rs:1050` | `layer_int_get!` (image_width/height, …) | `instance` | suspect (read misroute) | `LayerIntf.cpp` same macro | `with(_logo){ return .imageWidth; }` from a timer |
| `visual/layer/mod.rs:1085` | hand‑written `layer_visible_set` logs only; writes via `with_layer_write` | `instance` | suspect | `LayerIntf.cpp:10592` | as above |
| `visual/layer/pixels.rs:556,699,910` | `affine_method!`/`stretch_method!`/`rect_method!` | `instance` (`with_layer_write`) | needs‑test (method, arg objects explicit) | `LayerIntf.cpp` affine/stretch | call via `with` with a wrong enclosing Layer |
| `visual/layer/pixels.rs:1954` | `loadImages` (hand) | `instance` | needs‑test (method; user reports it works) | `LayerIntf.cpp` `loadImages` | — |
| `visual/window.rs:812,855` | `window_i64_prop!`/`window_bool_prop!` | `instance` (`WindowInst`) | **suspect** | `WindowIntf.cpp` properties | `with(w){ .left = 10 }` inside a `Window` method |
| `visual/font.rs:353` | `font_int_prop!` | `instance` | suspect | `FontIntf.cpp` | `with(font){ .height = 12 }` |
| `sound.rs:956` | `channel_setter!` | `instance` (`SoundInst`) | **suspect** | `WaveIntf.cpp` channel props | `with(buf){ .volume = 0 }` |
| `sound.rs:910,933` | `channel_int_getter!`/`channel_real_getter!` | `instance` | suspect | `WaveIntf.cpp` | read via `with` |
| `sound/wave_flags.rs:148` | `flag_property!` | `instance` (`WaveFlags`) | suspect | `WaveIntf.cpp:1743` | `with(flags){ .loop = true }` |
| `cdda_midi.rs:676,705` | `disc_channel_int_getter!`/`disc_channel_setter!` | `instance` | suspect | `CDDAIntf.cpp` | `with(disc){ .volume = 0 }` |
| `kag/mod.rs:1045,1060,1075,1092` | `prop_get_int!/bool!`, `prop_set_bool!`, `prop_get_string!` | `instance` (`KAGParser`) | suspect | `KAGParserIntf.cpp` | `with(parser){ .enabled = false }` |
| `menu_item.rs:1002` | `bool_property!` | `instance` (`MenuItemInst`) | suspect | `MenuItemIntf.cpp` | `with(item){ .checked = true }` |
| `video.rs:370,400,430,449` | `slot_int_prop!`/`slot_real_prop!`/`slot_ro_int!` | `instance` (`VideoSlot`) | suspect | `VideoIntf.cpp` | `with(slot){ .loop = true }` |
| `rect.rs:608,591,233` | `rect_edge_setter!`/`edge_getter!`/`rect_component!` | `instance` (`RectInst`) | suspect | `RectIntf.cpp` | `with(r){ .left = 0 }` |
| `plugins/layer_ex/base.rs:185,249,300` | `layer_getter!`/`clip_getter!`/`clip_setter!` | `instance` (`BaseInst`) | suspect | `LayerIntf.cpp:4041-4079` | `with(layer){ .clipLeft = 0 }` (after the plugin attaches members to `Layer`) |
| `plugins/text_render/mod.rs:446,477,508,539` | `int_prop!`/`bool_prop!`/`real_prop!`/`string_prop!` | `instance` (`TextRenderInst`) | suspect | `TextRender.cpp` | `with(tr){ .fontSize = 20 }` |
| `plugins/psd_file/mod.rs:526` | `psd_header_prop!` | `instance` (`PsdInst`) | suspect (read) | `PsdFile.cpp` | `with(psd){ return .width; }` |
| `plugins/motion_player/macros.rs:10-310` | `mp_get_*`/`mp_set_*`/`mp_method_*` | `instance` (player payload) | suspect | `MotionPlayer` | `with(player){ .x = 0 }` |
| `plugins/motion_player/player_members.rs:692` | `scale_setter!` | `instance` | suspect | `MotionPlayer` | `with(player){ .scale = 2 }` |
| `plugins/motion_player/emote.rs:104,150` | `module_int_member!`/`module_bool_member!` | `instance` | suspect | `EmotePlayer` | `with(module){ .x = 0 }` |
| `plugins/motion_player/adaptors.rs:110,314` | `simple_ctor!`/`getter_void!` | `instance` | needs‑test | — | — |
| `input.rs:1158,1182,1199` | `pad_get_str!`/`pad_get_int!`/`pad_get_bool!` | `instance` (`Pad` payload) | suspect (read) | `PadIntf.cpp` | `with(pad){ return .visible; }` |
| `tree_owner.rs:843` | `fire_method!` | `instance` used to dispatch events | needs‑test | `LayerIntf.cpp` event methods | events, not plain writes |
| `visual/layer/events.rs:33` | `event_method!` | `instance` ignored (no‑op stubs) | **safe** (no receiver state) | `LayerIntf.cpp:9944-10472` | n/a |

### 3.2 Objthis families (raw `objthis` used) — suspect

| site (file:line) | family | receiver | risk | reference | repro plan |
|---|---|---|---|---|---|
| `plugins/layer_ex/mod.rs:577` (`draw.rs:2181`) | `draw_property!` (+ `layer_property_defs` `draw.rs:2446`) | `objthis` -> `layer_from_objthis` -> `native_instance_id("Layer")` (`layer_ex/mod.rs:127-139`) | **suspect** | `LayerIntf.cpp` `LayerExDraw` | `with(layer){ .smoothing = 8 }` |
| `plugins/layer_ex/draw.rs:2284+` | `layer_method_defs()` objthis methods | `objthis` (`draw.rs:683,998,1108,…`) | needs‑test | `LayerIntf.cpp` | `with(layer){ .fillRect(…) }` |
| `plugins/layer_ex/movie.rs:525` | `layer_method_defs()` objthis | `objthis` | needs‑test | movie hooks | — |
| `plugins/get_sample/mod.rs:518` | `getSample`/`setDefaultCounts`/`sampleValue`… | `objthis` (map keyed by `objthis as usize`, `:127`) | **suspect** | `WaveIntf.cpp` get‑sample | `with(buf){ .sampleCount = 4 }` |
| `plugins/save_struct/mod.rs:501,520` | `save2`/`saveStruct2`/`toStructString` on `Array`/`Dictionary` | `objthis` | needs‑test (method; arg is `this`) | `ncbin` `saveStruct` | `with(arr){ .saveStruct2(); }` |
| `plugins/window_ex/window.rs:961` | `disableResize`/`disableMove`/`enableNCMouseEvent`/`exSystemMenu` | `objthis` -> per‑objthis window flags | **suspect** | `WindowIntf.cpp` | `with(w){ .disableResize = true }` |
| `plugins/window_ex/menu_item.rs:319` | objthis properties | `objthis` | suspect | `MenuItemIntf.cpp` | `with(item){ .checked = true }` |
| `plugins/window_ex/pad.rs:36`, `console.rs:108` | objthis properties/methods | `objthis` | needs‑test | `PadIntf.cpp`/`ConsoleIntf.cpp` | — |
| `plugins/csv_parser/mod.rs:680` | instance class + objthis fallback (`:298-301`) | mixed | needs‑test | `CSVParserIntf.cpp` | — |

### 3.3 Static / constant families — safe (receiver dropped)

| site (file:line) | family | receiver | risk | reference | note |
|---|---|---|---|---|---|
| `sound.rs:2871` (`:2486`,`:2501`) | `sound_global_volume_get/set`, `globalVolume`, `globalFocusMode` | none (engine/global) | **safe** | `WaveIntf.cpp:1592-1607` `TJS_END_NATIVE_STATIC_PROP_DECL` | `WaveSoundBuffer.globalVolume = v` |
| `window_ex/util.rs:364` | `int_property_getter!` | none | safe | `WindowIntf.cpp` static consts | `Window.nchtError` |
| `visual/draw_device.rs:305` | `draw_device_constant_prop!` | none | safe | `TVP_REGISTER_PTDD_ENUM` | `Window.PassThroughDrawDevice.dtNone` |
| `plugins/motion_player/mod.rs:152`, `scripts_ex/mod.rs:1323`, `emote.rs:254` | `const_getter!`/`constant_getter!`/`module_const_getter!` | none | safe | plugin constants | — |
| `plugins/psd_file/mod.rs:105` | `psd_constant_properties!` | none | safe | `PsdFile.cpp` | — |
| `plugins/layer_ex/geom.rs:855`, `menu_item.rs:1671`, `cdda_midi.rs:958`, `image_function.rs:1223`, etc. | `register_static_members` sites | none | safe | `TJS_END_NATIVE_STATIC_*` | class‑qualified access only |

### 3.4 Static‑only classes (hand‑written)

`clipboard.rs:239`, `input.rs:1376,1439`, `scripts.rs:686`, `storages.rs:643`,
`system.rs:2222`, `debug.rs:1024`, `plugins/json/mod.rs:1109`,
`plugins/fstat/mod.rs:1689`, `plugins/add_font/mod.rs:102`,
`plugins/steam/mod.rs:529`, `plugins/window_ex/scripts.rs:114`,
`plugins/window_ex/system.rs:694`, `plugins/xp3_filter/mod.rs:97`,
`plugins/resource.rs:668`, `script_ex` constants — registered via
`register_class`/`register_static_members`; their callbacks have **no** `instance`/`objthis`
parameter and cannot be misrouted by the receiver at all. Classify **safe**.

---

## 4. What the static member / script override cases actually do

* **Static member (`WaveSoundBuffer.globalVolume`).** Registered with
  `TJS_STATICMEMBER` (`tjs2_abi.cpp:2123-2200`, registration `:2174`; reference
  `WaveIntf.cpp:1592-1607`). A class‑qualified write
  `WaveSoundBuffer.globalVolume = v` reaches `tjs2_dispatch_native_property_set`, which
  drops `objthis` (`tjs2_abi.cpp:718`) and writes engine state — **safe**. A write through an
  *instance* (`with(buf){ .globalVolume = v }`) is a different hazard: static members are
  skipped when `tTJSNativeClass::FuncCall` copies members onto instances
  (`reference/cpp/core/tjs2/tjsNative.cpp:344-366`), so `buf.globalVolume = v` is not the
  static write — with `TJS_MEMBERENSURE` it creates an own instance member that silently
  shadows the static. `needs-a-test` for any game that writes a static through an instance.
* **Script property override (`Sound.tjs`: `property volume { setter(v){ super.volume=v; } }`).**
  The script setter runs with `this` = the object the VM resolved. It then does
  `super.volume = v`, which re-enters native dispatch; the receiver is the script setter's
  `this`. If the script setter was invoked through the `with`‑proxy with the enclosing
  `this`, `super.volume = v` writes the enclosing object's native payload — the same bug one
  level up. So script overrides **inherit** the receiver problem; they do not shield against
  it. `suspect` for any object whose class defines such an override.

---

## 5. Ranked shortlist (most likely to affect real games)

1. **`visual/layer/mod.rs:1071` `layer_int_prop!` / `:1050` `layer_int_get!`** — every
   `Layer`/`Sprite` property a KAG game touches (`visible`, `left`, `top`, `width`,
   `height`, `absolute`, `cursor`, `face`, …). This is the measured bug; a wrong receiver
   that is *also* a Layer (e.g. the enclosing `Logo` layer) silently writes the wrong layer
   instead of erroring.
2. **`sound.rs:956/910/933` channel setters/getters + `WaveSoundBuffer` instance props** —
   the prompt's own `Sound.tjs` `volume` override calls `super.volume`, so this family is
   directly implicated. Per‑buffer `volume`/`pan`/`pos` writes misroute.
3. **`menu_item.rs:1002` and `plugins/window_ex/window.rs:961`/`menu_item.rs:319`** — dialog
   UI state (checked/visible/disableResize); wrong‑receiver writes are silent and
   user‑visible.
4. **`kag/mod.rs:1045-1099`** — `KAGParser` internal flags; a misrouted write corrupts the
   parser's own state (and `KAGParser` is one of the most sub‑classed native classes in real
   games).
5. **`plugins/layer_ex/draw.rs:2181` `draw_property!`** (objthis → `layer_from_objthis`) —
   plugin drawing state; attached to the builtin `Layer`, so it shares the exposure.
6. **`plugins/motion_player/*`** — many properties, plugin‑heavy games; same shape.
7. **`rect.rs`, `plugins/text_render`, `plugins/psd_file`, `visual/font`** — narrower but
   the same instance‑payload derivation.

Rationale for ranking: (a) breadth of game use, (b) whether the wrong receiver is likely to
be *the same native class* (silent corruption) rather than a different class (loud
`NativeInstanceSupport` error), and (c) whether the prompt already shows a real game scripts
this path.

---

## 6. What I could not determine without running code

1. **The exact call/‑write asymmetry.** The vendored VM's `SetPropertyDirect` /
   `CallFunctionDirect` / `tTJSVariantClosure` are byte‑identical to the reference
   (diff of `reference/cpp/core/tjs2/tjsInterCodeExec.cpp` vs
   `crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp` shows only the `memset` and null‑string
   patches). I could not derive from source alone why a *method call* in the same `with`
   block keeps the right receiver while the *write* does not. Candidates that need a run:
   (a) the member reference's `ObjThis` differs between the two opcodes; (b) the compat
   constructor member does not copy native members with `ChangeClosureObjThis`
   (`tjs2_abi.cpp:1392-1470` vs `reference/.../tjsNative.cpp:305-375`), so script‑subclass
   objects keep class‑chain members with no own `ObjThis`; (c) the Timer's retained callback
   carries a different `ObjThis` than the timer callback function expects
   (`tjs2_call_value`, `tjs2_abi.cpp:2676`, calls `FuncCall(..., nullptr)` and relies on the
   retained closure's `ObjThis`).
2. **Whether direct native instances are safe in practice.** `tTJSNativeClass::FuncCall`
   rebinds copied members to the object (`tjsNative.cpp:353`), which would make
   `new Layer()` instances immune to the `with` leak. I could not confirm from source that
   the game's `Sprite` is constructed through that path rather than a script‑subclass
   constructor member; this decides whether the bug is instance‑wide or script‑subclass‑wide.
3. **Whether `tjs2_call_value` preserves the callback's receiver.** It passes `nullptr` for
   the call's `objthis` and depends on `clo.ObjThis`. I could not verify (without a run)
   whether the Timer retains the callback with the correct `ObjThis`
   (`retain_object_arg` / `resolve_value_variant`, `tjs2_abi.cpp` around
   `resolve_value_variant`) or falls back to `last_object`.
4. **Static member shadowing via an instance.** Whether real games write a static member
   through an instance (creating a silent own member) needs a script search/run; the source
   analysis shows the *mechanism* but not whether it fires.
5. **Which suspect families real games actually exercise through `with`.** The classification
   is static; the actual exposure list needs the game corpus (and possibly an instrumentation
   pass over `native_instance_payload` / `_objthis` the way the measured bug was instrumented).

---

## 7. Evidence index

* ABI: `crates/tjs/cpp/tjs2_abi.h:429-446` (instance method/prop signatures, `instance` +
  trailing `objthis`), `:514-535` (objthis signatures).
* Dispatch: `crates/tjs/cpp/tjs2_abi.cpp:564-620` (static method), `:668-746` (static
  property, `objthis` dropped), `:796-965` (objthis method/property, `objthis` passed),
  `:1123-1197` (instance method, `NativeInstanceSupport` at `:1136`), `:1247-1360` (instance
  property, `NativeInstanceSupport` at `:1263`/`:1327`), `:1392-1470` (constructor member).
* Registration: `crates/tjs/cpp/tjs2_abi.cpp:2123-2200` (static), `:2222-2330` (instance),
  `:2332-2480` (static members after registration), `:2384-2460`
  (`tjs2_register_native_instance_members`).
* Rust mirror: `crates/tjs/src/lib.rs:109-208` (callback types), `:953`/`:1030`/`:1117`/`:1203`
  (registration entry points).
* VM receiver derivation: `crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp:1440` (`SetPropertyDirect`),
  `:2161`/`:2113` (`CallFunctionDirect`/`CallFunction`), `crates/tjs/cpp/tjs2/tjsVariant.h:261`
  (`PropSet`), `crates/tjs/cpp/tjs2/tjsObject.cpp:1447` (`PropSetByVS`),
  `crates/tjs/cpp/tjs2/tjsObject.h:82` (`TJS_SELECT_OBJTHIS`),
  `crates/tjs/cpp/tjs2/tjsObjectExtendable.cpp:81-91` (superclass chain keeps `objthis`).
* `with` compilation: `crates/tjs/cpp/tjs2/tjsInterCodeGen.cpp:2523-2534`,
  `:3309-3330`.
* Reference: `reference/cpp/core/tjs2/tjsNative.h:298-322` (`TJS_GET_NATIVE_INSTANCE`),
  `:422-444` (prop decl macros), `reference/cpp/core/tjs2/tjsNative.cpp:305-375`
  (member copy + `ChangeClosureObjThis`), `reference/cpp/core/visual/LayerIntf.cpp:10592-10610`
  (`visible` instance property), `reference/cpp/core/sound/WaveIntf.cpp:1592-1607`
  (static `globalVolume`), `reference/cpp/core/tjs2/tjsInterCodeExec.cpp:1430`
  (`SetPropertyDirect`).
