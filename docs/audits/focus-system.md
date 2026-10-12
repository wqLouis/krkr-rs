# Focus subsystem audit — `onFocus` / `onBlur`

Read-only audit. Oracle: `reference/cpp/**`. Port snapshot: working tree at
`HEAD fc013a0` (`migration/middle-layer-ash`), 2026-10-12. The working tree was
being edited by other workers while this was written, so **port line numbers are
approximate** — every port claim also names the symbol/function so it can be
re-located if a line drifts.

## 0. Executive summary

* The reference focus subsystem is a single field,
  `tTVPLayerManager::FocusedLayer`, mutated only through
  `tTVPLayerManager::SetFocusTo` (`reference/cpp/core/visual/LayerManager.cpp:760-817`).
  Every focus change goes through it and it is what fires `onBlur`/`onFocus`.
* `SetFocusTo` is **synchronous**: it fires `onBlur` on the outgoing layer, then
  `onFocus` on the incoming layer, inside the call (`TVP_EPT_IMMEDIATE`,
  `reference/cpp/core/base/EventIntf.cpp:207-233`).
* The outgoing layer's `onBlur` argument is the **newly** focused layer (or
  null); the incoming layer's `onFocus` first argument is the **previously**
  focused layer (or null). This is the opposite of how the task statement
  describes `onBlur`; the reference is unambiguous (see §3).
* The reference does **not** move focus on mouse click or hit test. Focus moves
  only from explicit `Layer.focus()`, `focusNext()`/`focusPrev()`, the default
  keyboard handler, and the "focused layer became unusable / was modal / was
  removed" paths (see the trigger table, §4).
* The port already has all the L2 state and search machinery
  (`engine::Scene::focused_layer`, `set_focus`, `clear_focus`,
  `node_focusable`, `next_focusable`, `prev_focusable`) and the L3 native
  members, but **no focus transition ever fires an event**, and the
  `onFocus`/`onBlur`/`onBeforeFocus`/`onSearch*Focusable` members are inert
  no-op defaults (`crates/compat/src/natives/visual/layer/events.rs`).
* The port also has a **non-reference** behaviour: it focuses the clicked layer
  (`crates/compat/src/event.rs` `focus_clicked_layer`, called from
  `resolve_pointer`). The reference's `PrimaryMouseDown` sets only the mouse
  capture, never focus (`reference/cpp/core/visual/LayerManager.cpp:374-399`).
* Smallest correct change: one L3 helper around `Scene::set_focus`/`clear_focus`
  that fires the two events, wired into `Layer.focus`, `focusNext`/`focusPrev`
  and the `focusedLayer` setters. That covers the cases the test game actually
  uses (`layer.focus()`, `primaryLayer.focus()`).

## 1. The focus model (reference)

### 1.1 State

| State | Where | Citation |
|---|---|---|
| `FocusedLayer` — the one focused layer of a layer manager | `tTVPLayerManager::FocusedLayer`, one per `tTVPLayerManager` (one per primary layer) | `reference/cpp/core/visual/LayerManager.h:281`; init `LayerManager.cpp:38` |
| `Focusable` (script `focusable`), default `false` | `tTJSNI_BaseLayer::Focusable` | `LayerIntf.cpp:355`; `LayerIntf.h:526` |
| `JoinFocusChain`, default `true` | `tTJSNI_BaseLayer::JoinFocusChain` | `LayerIntf.cpp:356`; `LayerIntf.h:527` |
| `FocusWork` — transient redirect target | `tTJSNI_BaseLayer::FocusWork` | `LayerIntf.h:528`, `651` |
| `FocusChangeLock` — re-entrancy guard | `tTVPLayerManager::FocusChangeLock` | `LayerManager.h:289`; `LayerManager.cpp:775` |
| `ModalLayerVector` — modal stack | `tTVPLayerManager::ModalLayerVector` | `LayerManager.h`; `LayerManager.cpp:822-935` |

`GetFocusedLayer` is a plain read of `FocusedLayer` (`LayerManager.h:483`).
`SetFocusedLayer(layer)` is `SetFocusTo(layer, false)` (`LayerManager.h:488-490`)
and is the backing of `Window.focusedLayer` and
`BitmapLayerTreeOwner.focusedLayer` (`WindowIntf.cpp:1753-1787`;
`BitmapLayerTreeOwner.cpp:579-615`).

### 1.2 What makes a layer focusable

The effective predicate is `GetNodeFocusable()` (`LayerIntf.h:665-667`):

```
CheckFocusable()      == Focusable && Visible && Enabled          (LayerIntf.h:663)
ParentFocusable()     == every ancestor Visible && Enabled        (LayerIntf.cpp:3547-3560)
GetNodeFocusable()    == CheckFocusable() && ParentFocusable()
                         && !IsDisabledByMode()                   (LayerIntf.h:666)
```

`IsDisabledByMode()` is false when there is no modal layer, and otherwise
requires the layer to be an ancestor-or-self of the current modal layer
(`LayerIntf.cpp` `IsDisabledByMode`, `LayerManager.cpp:929-935`). Note
`ParentFocusable` deliberately does **not** check an ancestor's own `Focusable`.

`SetFocusable(b)` (`LayerIntf.cpp:3683-3696`) only acts when the *node*
focusability flips: turning focusability **off** while this layer is the focused
one calls `SetFocusTo(GetNextFocusable(), true)` — i.e. it blurs to the next
focusable layer and therefore fires `onBlur`/`onFocus`. Turning focusability
**on** does nothing (no auto-focus).

`CheckTreeFocusableState(root)` (`LayerManager.cpp:713-721`) exists to
auto-focus the first focusable layer of a newly added / newly enabled / newly
visible subtree. **Its body is commented out**, so it is inert: no auto-focus
happens on creation, `visible = true`, or `enabled = true`.

### 1.3 `BlurTree` and the disabled / hidden / removed paths

`BlurTree(root)` (`LayerManager.cpp:690-711`) removes focus from any focused
layer that is a descendant-or-self of `root`, moving focus to
`root->GetNextFocusable()` (or clearing it) via `SetFocusTo` — so it fires the
events. Its callers:

| Cause | Citation |
|---|---|
| `SetVisible(false)` (this layer) | `LayerIntf.cpp:1183-1206` (the `!Visible` branch calls `Manager->BlurTree(this)`) |
| `SetEnabled(false)` (this layer) | `LayerIntf.cpp:3801-3832` (`Manager->BlurTree(this)`) |
| `SeverChild(child)` — child detached | `LayerIntf.cpp:655-663` |
| `NotifyPart(lay)` — a layer tree parts from its parent | `LayerIntf.cpp:628`; `LayerManager.cpp:197-201` |
| `DetachPrimary()` — the primary layer is detached | `LayerManager.cpp:175-183` (calls `SetFocusTo(nullptr)`, not `BlurTree`) |

`SetVisible(false)` on the **primary** layer throws
`TVPCannotSetPrimaryInvisible` (`LayerIntf.cpp:1185-1186`); `SetOpacity` on the
primary with anything but 255 throws the same (`LayerIntf.cpp:1209-1210`).
`AttachPrimary` forces the primary visible and full opacity
(`LayerManager.cpp:161-173`).

The reverse transitions (`SetVisible(true)`, `SetEnabled(true)`, `AddChild`)
call the inert `CheckTreeFocusableState`.

### 1.4 Modal layers

`setMode()` / `removeMode()` and the modal stack also drive focus
(`LayerManager.cpp:834-916`):

* `SetModeTo(layer)` focuses `layer->SearchFirstFocusable()` (chain-ignoring by
  default `ignore_chain_focusable = true`, so the modal layer's own subtree is
  searched) with `direction = true` (`:855`), then pushes it on the modal stack.
* `RemoveModeFrom(layer)` / `RemoveTreeModalState(root)` focus
  `layer->GetNextFocusable()` / `root->GetNextFocusable()` with `direction =
  true` (`:883`, `:913`) as the modal entry is popped.

### 1.5 No click-focus, no window-focus effect on layers

* `PrimaryMouseDown` (`LayerManager.cpp:374-399`) hit-tests, fires
  `onMouseDown`, and sets `CaptureOwner`. It never calls `SetFocusTo`.
  `PrimaryClick`/`PrimaryDoubleClick` (`:358-372`) only fire `onClick`/
  `onDoubleClick`.
* Window activation only fires `Window.onActivate`/`onDeactivate`
  (`WindowIntf.cpp:556-570`, `FireOnActivate` `:247`). It does not touch
  `FocusedLayer`. `Window.focusable` is the OS form focusability, not layer
  focus (`WindowIntf.cpp:1455`; `WindowImpl.cpp:1834-1843`).

## 2. Event arguments and ordering

### 2.1 Which layer receives what

`SetFocusTo` (`LayerManager.cpp:782-790`):

```cpp
tTJSNI_BaseLayer *org = FocusedLayer;   // outgoing
FocusedLayer = layer;                   // incoming, set BEFORE the events
if(org && !org->Shutdown)     org->FireBlur(layer);
if(FocusedLayer && ...)       FocusedLayer->FireFocus(org, direction);
```

| Event | Delivered to | Argument(s) | Citation |
|---|---|---|---|
| `onBlur` | the **outgoing** layer | one: the **incoming** layer, or `null` | `LayerIntf.cpp:3702-3709`; native member name `"focused"` at `:10197-10205` |
| `onFocus` | the **incoming** layer | two: the **outgoing** layer or `null`, then `direction` | `LayerIntf.cpp:3715-3722`; native members `"blurred"`, `"direction"` at `:10212-10220` |
| `onBeforeFocus` | the **candidate** layer, before any state change | three: `layer` (the candidate), `blurred` (current `FocusedLayer`), `direction` | `LayerIntf.cpp:3731-3744`; native members `"layer"`, `"blurred"`, `"direction"` at `:10413-10449` |

The argument names inside `FireBlur` (`prevfocused`) are misleading: the only
call site passes the **new** layer (`LayerManager.cpp:787`). The event member is
named `"focused"` specifically because the argument is the layer that is now
focused. Conversely `FireFocus`'s `prevfocused` is genuinely the old layer and
its member is `"blurred"`. An implementation that copies the parameter names
without reading the call site will emit the arguments backwards.

`onBlur` is delivered to the outgoing layer **after** `FocusedLayer` has been
updated, so a handler that reads `layer.focused` sees the new state.

### 2.2 Ordering

One call to `SetFocusTo` produces, in order:

1. `onBeforeFocus` on the candidate (if it is not shutdown);
2. a re-check of `GetNodeFocusable()` (the handler may have changed it);
3. early return if the candidate is already `FocusedLayer`;
4. `onBlur` on the outgoing layer (arg = incoming);
5. `onFocus` on the incoming layer (arg = outgoing, direction);
6. IME mode / attention point updates.

There is no "focus-then-blur" case: `onBlur` always precedes `onFocus`.

When focus is cleared (`layer == nullptr`): only `onBlur(null)` fires on the
outgoing layer; when focus is established from nothing (`org == nullptr`): only
`onFocus(null, direction)` fires.

### 2.3 Timing and direction

* **Synchronous.** `FireBlur`/`FireFocus` use `TVP_EPT_IMMEDIATE`
  (`LayerIntf.cpp:3707`, `:3720`), and `TVPPostEvent` delivers that flag
  in-line (`reference/cpp/core/base/EventIntf.cpp:215-233`; flag
  `EventIntf.h:74`). The events must run before `Layer.focus()` returns, not on
  the next frame.
* **`direction`** is a `bool`: `true` = forward, `false` = backward
  (`LayerManager.cpp:762-764`). It reaches the event object as an integer 0/1
  (`FireFocus(..., bool direction)` → `param[1] = direction`).
  * `focus()` default `true`, overridable by its optional argument
    (`LayerIntf.cpp:9731-9733`).
  * `focusNext()` → `true`; `focusPrev()` → `false`
    (`LayerManager.cpp:732`, `:745`).
  * `focusedLayer = x` (Window / tree owner) → `false`
    (`LayerManager.h:489` `SetFocusedLayer`).
  * `BlurTree`, `SetFocusable(false)`, `SetModeTo`, `RemoveModeFrom` → `true`.
  * `DetachPrimary` → `SetFocusTo(nullptr)` → default `true`.

### 2.4 Re-entrancy

If a handler changes focus again while `SetFocusTo` is running, the reference
throws `TVPCannotChangeFocusInProcessingFocus`
(`LayerManager.cpp:774-776`). `onBeforeFocus` can redirect the target: it runs
before the state change and `SetFocusTo` re-reads the returned
`FocusWork` (`LayerManager.cpp:770`). The exact mechanism by which a script
handler rewrites `FocusWork` (the native reads `param[0]` after the invoke,
which appears to be the candidate layer, `LayerIntf.cpp:10426-10445`) is not
fully legible in this vendored source and is **not** determined here; a
reference test that returns a different layer from `onBeforeFocus` would settle
it. Treat the redirect as unimplemented/needs-verification.

### 2.5 Related focus-chain events

`GetNextFocusable` / `GetPrevFocusable` (`LayerIntf.cpp:3615-3660`) post
`onSearchNextFocusable` / `onSearchPrevFocusable` with the candidate layer (or
`null`) and let the handler redirect through `FocusWork`
(`LayerIntf.cpp:3621-3627`, `:3657-3663`; natives `:10340-10411`). These fire
from every focus search: `FocusNext`/`FocusPrev`, `SetFocusable(false)`,
`BlurTree`, `SetModeTo`, `RemoveModeFrom`, `RemoveTreeModalState`.

## 3. The port's declared handlers

`crates/compat/src/natives/visual/layer/events.rs:79-83,114-118` declares:

```
event_method!(on_before_focus, "onBeforeFocus", 3);
event_method!(on_blur, "onBlur", 1);
event_method!(on_focus, "onFocus", 1);
```

Correction to the task premise: the declared counts **match the reference's
native-method gate**, not a divergence. The reference natives use
`TVP_ACTION_INVOKE_BEGIN(1, "onBlur"/"onFocus", …)` and `(3, "onBeforeFocus",
…)` (`LayerIntf.cpp:10197-10220`, `:10417-10421`), i.e. the same 1/1/3. The
divergence is not the count — it is that (a) nothing calls these handlers and
(b) the port has no event-object/action-owner forwarding, so the object-valued
arguments (`focused`/`blurred`) and the `onFocus.direction` member do not exist
(the port's `EventValue` enum cannot even carry an object — see §5).

## 4. Trigger table

RP = reference `LayerManager.cpp` / `LayerIntf.cpp`. "Outgoing" = the layer
that loses focus; "Incoming" = the layer that gains it. Every row is
`SetFocusTo` and therefore fires synchronously.

| # | Trigger | Outgoing event | Incoming event | Arguments | Citation |
|---|---|---|---|---|---|
| 1 | `layer.focus([dir])`, default `dir=true` | `onBlur(incoming)` if there was an outgoing layer | `onFocus(outgoing, dir)` | blur arg = new layer/null; focus args = old layer/null, dir | `LayerIntf.cpp:3561-3566`, `:9726-9742`; `LayerManager.cpp:760-817` |
| 2 | `layer.focusNext()` | `onBlur(new)` | `onFocus(old, true)` | dir = true | `LayerManager.cpp:736-747`; `LayerIntf.cpp:9762-9778` |
| 3 | `layer.focusPrev()` | `onBlur(new)` | `onFocus(old, false)` | dir = false | `LayerManager.cpp:723-734`; `LayerIntf.cpp:9744-9760` |
| 4 | keyboard default: Tab / Right / Down, no Shift/Alt/Ctrl → `FocusNext` | as #2 | as #2 | dir = true | `LayerIntf.cpp:3892-3912` (called from the `onKeyDown` native, `:10264-10268`) |
| 5 | keyboard default: Shift+Tab / Left / Up → `FocusPrev` | as #3 | as #3 | dir = false | `LayerIntf.cpp:3892-3912` |
| 6 | `layer.focusable = false` while it is focused | `onBlur(next)` where `next = GetNextFocusable()` (or null) | `onFocus(old, true)` on `next` | dir = true | `LayerIntf.cpp:3683-3696` |
| 7 | `layer.visible = false` (self or ancestor) | `onBlur(next or null)` via `BlurTree` | `onFocus(old, true)` on next | dir = true | `LayerIntf.cpp:1183-1206`; `LayerManager.cpp:690-711` |
| 8 | `layer.enabled = false` (self or ancestor) | `onBlur(next or null)` via `BlurTree` | `onFocus(old, true)` on next | dir = true | `LayerIntf.cpp:3801-3832`; `LayerManager.cpp:690-711` |
| 9 | layer detached / removed / `SeverChild` / `NotifyPart` | `onBlur(next or null)` via `BlurTree` | `onFocus(old, true)` on next | dir = true | `LayerIntf.cpp:628`, `:655-663`; `LayerManager.cpp:197-201`, `:690-711` |
| 10 | `setMode()` | `onBlur(modeFirst)` | `onFocus(old, true)` | dir = true | `LayerManager.cpp:834-862` |
| 11 | `removeMode()` / modal pop | `onBlur(next or null)` | `onFocus(old, true)` | dir = true | `LayerManager.cpp:869-916` |
| 12 | `window.focusedLayer = l` (or null) | `onBlur(l or null)` | `onFocus(old, false)` | dir = **false** | `WindowIntf.cpp:1753-1787`; `LayerManager.h:488-490` |
| 13 | `bitmapLayerTreeOwner.focusedLayer = l` | as #12 | as #12 | dir = **false** | `BitmapLayerTreeOwner.cpp:579-615`; `LayerTreeOwnerImpl.cpp:379-390` |
| 14 | `DetachPrimary()` (primary detached / window teardown) | `onBlur(null)` | none | — | `LayerManager.cpp:175-183` |
| 15 | any `SetFocusTo` candidate | **`onBeforeFocus(layer, blurred, dir)` first**, on the candidate | (then re-check; may abort) | candidate, current, dir | `LayerManager.cpp:770-772`; `LayerIntf.cpp:3731-3744` |
| 16 | any focus search (rows 2,3,6,7,8,9,10,11) | `onSearchNextFocusable(candidate)` / `onSearchPrevFocusable(candidate)` | — | candidate or null | `LayerIntf.cpp:3615-3660`, `:10340-10411` |

**Inert in the reference (do not fire the focus events):**

| Trigger | Reference behaviour | Citation |
|---|---|---|
| mouse click / hit test | sets `CaptureOwner` only; no focus change | `LayerManager.cpp:358-399` |
| window activate / deactivate | `Window.onActivate`/`onDeactivate` only; no layer focus change | `WindowIntf.cpp:247`, `:556-570` |
| add child / `visible=true` / `enabled=true` | `CheckTreeFocusableState` is a no-op | `LayerManager.cpp:713-721` |
| `layer.focusable = true` | no auto-focus | `LayerIntf.cpp:3683-3696` |

## 5. What the port has vs what is missing

### 5.1 Exists

| Piece | Location | Notes |
|---|---|---|
| L2 focused-layer state | `crates/engine/src/scene.rs:36` (`WindowState::focused_layer`) | one per window, same model as `FocusedLayer` |
| L2 transition primitive | `scene.rs:1167-1200` (`Scene::set_focus`, `clear_focus`) | sets/clears and returns `(prev, new)`; **fires nothing**; `set_focus` does **not** validate `node_focusable` |
| L2 predicate | `scene.rs:1255-1265` (`node_focusable`) | equals `GetNodeFocusable` (modal state not modelled) |
| L2 search | `scene.rs:1276-1310` (`next_focusable`, `prev_focusable`) | wrapping, excludes self |
| `Layer.focus()` | `crates/compat/src/natives/visual/layer/mod.rs` `layer_focus` (≈:954-977) | calls `scene.set_focus(id)`, returns void, ignores its optional `direction` argument, no `node_focusable` guard, fires nothing |
| `focusNext` / `focusPrev` | `crates/compat/src/natives/visual/layer/focus.rs` `focus_neighbor` | returns the layer object; fires nothing |
| `focusable`, `focused`, `nodeFocusable`, `nextFocusable`, `prevFocusable`, `joinFocusChain` | `events.rs`/`mod.rs` (≈:1411, 1576, 1598, 2154; focus.rs) | read/write the L2 fields; setters do not blur |
| `Window.focusedLayer` setter | `crates/compat/src/natives/visual/window.rs` `window_focused_layer_set` (≈:976-1012) | `scene.set_focus`/`clear_focus`; fires nothing |
| `BitmapLayerTreeOwner.focusedLayer` setter | `crates/compat/src/natives/tree_owner.rs` `focused_layer_set` (≈:655-739) | same |
| `setMode` / `removeMode` | `crates/compat/src/natives/visual/layer/order.rs` (≈:264-380) | move focus via `scene.set_focus`; fire nothing |
| Focus state on removal | `scene.rs:1156-1157`, `:986`, `:1187` | clears `focused_layer` when the focused layer is removed; fires nothing |
| L2 `focus_work` field | `scene.rs:135-137` | declared for the `onSearch*`/`onBeforeFocus` redirect; unused |
| Declared handlers | `events.rs:79-83,114-118` | no-op defaults; never invoked by the engine |
| Window action-object machinery | `crates/compat/src/natives/visual/window.rs` `dispatch_action_event` (≈:1244), `window_action_event!` (≈:1357-1447) | builds the TJS event dict and calls `owner.action(event)`; **has no `onFocus`/`onBlur` registrations** (the reference `Window` class has none either) |
| Layer-object dispatch in the tree owner | `crates/compat/src/natives/tree_owner.rs` `dispatch_event` (≈:313) | the only existing path that passes retained objects to a handler |

### 5.2 Missing

1. **Any firing of `onFocus`/`onBlur`.** Grep for call sites outside
   `events.rs` finds none (verified).
2. **`onBeforeFocus` firing and the `FocusWork` redirect**; **`onSearchNextFocusable`/
   `onSearchPrevFocusable`** firing (`focus_work` is dead state).
3. **Blur on unusable/removed focused layers:** the `focusable`, `visible` and
   `enabled` setters (`mod.rs` ≈:1411, 1576, 1598) only flip the flag; there is
   no `BlurTree` equivalent. Removal clears the L2 field silently.
4. **`node_focusable` validation** in `Layer.focus()` and the two `focusedLayer`
   setters: the reference `SetFocusTo` returns early on a non-focusable target.
5. **`direction` handling** in `Layer.focus()`: the optional argument is
   ignored (`layer_focus` matches `_argc`).
6. **Object/null event arguments.** `EventValue`
   (`crates/compat/src/event.rs:57`) is only `Int|Real|Str|Bool`, and `TjsValue`
   (`crates/tjs/src/lib.rs:620`) has no `Null` variant; `value_to_ffi` rejects
   `TjsValue::Object` (`crates/tjs/src/lib.rs:2225-2230`). The reference
   `onBlur`/`onFocus` arguments are Layer objects or TJS `null`, so the current
   adapter path cannot deliver them. It needs `TjsValue::Retained(id)` for a
   layer (as `dispatch_action_event` already does for `target`) plus a null
   representation (a `TjsValue::Null`/`VAL_NULL` path or a dedicated call).
7. **Action-owner forwarding.** The reference Layer native `onFocus`/`onBlur`
   builds the event object and calls `ActionOwner.action(event)`
   (`EventIntf.h:208-238`; members `focused` / `blurred`+`direction`). The port's
   Layer natives are no-ops, and `LayerInst` retains no action owner, so
   `super.onFocus(...)` in a script does nothing on the action side.
8. **Re-entrancy guard** (`FocusChangeLock` equivalent).

### 5.3 Divergence found (not an omission)

`crates/compat/src/event.rs` `resolve_pointer` calls `focus_clicked_layer` on
`onMouseDown` (`event.rs:482-500`, `677-688`), which focuses any hit
`node_focusable` layer. The reference does **not** focus on click
(`LayerManager.cpp:374-399`). Once events fire, this would emit `onFocus`/
`onBlur` on every click, in addition to whatever the script does. The test game
confirms click-focus is script-driven: `system/editlayer.tjs` calls `focus()`
from `onMouseDown` explicitly (see §7). This divergence should be removed as
part of aligning focus, but it is a separate script-observable change
(`window.focusedLayer` after a click) from "make the events fire".

### 5.4 Adjacent divergences (found, out of scope for this task)

* `Layer.focus()` returns `void`; the reference returns the boolean success
  result (`LayerIntf.cpp:9736-9740`).
* `Layer.focus()` on a non-focusable layer currently focuses it.
* Primary-layer invariants (`SetVisible(false)`/`SetOpacity != 255` must throw
  `TVPCannotSetPrimaryInvisible`) are not checked in the setters.
* `onNodeEnabled`/`onNodeDisabled` are declared but never fired
  (`events.rs`), the same shape of gap as focus.

## 6. Ranked work plan (smallest first)

### Step 1 — fire the two events on the explicit transitions (the bounded fix)

Add one L3 helper (e.g. `compat::focus::set_focus(ctx, host, target:
Option<u32>, direction: bool)`) that reproduces `SetFocusTo`'s tail:

1. read `prev = window.focused_layer`;
2. if `target == prev`, return without events;
3. `scene.set_focus(target)` / `clear_focus(window)`;
4. fire `onBlur` on `prev` (arg = `target` object or null), then `onFocus` on
   `target` (args = `prev` object or null, `direction`);
5. route the events to the **layer object** (the script override path);
6. deliver synchronously (no queue).

Wire it into: `layer_focus` (reading the optional `direction` argument),
`focus_neighbor` (`focusNext` → `true`, `focusPrev` → `false`),
`window_focused_layer_set`, `focused_layer_set` (tree owner), `layer_set_mode`/
`layer_remove_mode`. Add the `node_focusable` guard that `SetFocusTo` has, so
invalid targets do not fire events. This is what makes `system/editlayer.tjs`
(and any `layer.onFocus`/`layer.onBlur` override) work.

Prerequisite plumbing: extend the dispatch so an object/null argument can be
passed (item 6 in §5.2). The simplest is a new `TjsValue::Null` mapped to
`VAL_NULL` in `value_to_ffi`, plus `TjsValue::Retained` for the layer object
obtained with `duplicate_object`.

Tests: cite `LayerIntf.cpp:3702-3722`, `LayerManager.cpp:760-817`, and assert
blur-before-focus, the argument identities, and the direction values.

### Step 2 — the "focused layer became unusable" paths

Port `BlurTree`/`SetFocusable(false)`/`SetEnabled(false)`/`SetVisible(false)`
and removal blur to L3: when the focused layer (or an ancestor of it) loses
`focusable`/`visible`/`enabled` or is detached, move focus to the next
focusable (or clear) and fire the events with `direction = true`. This needs an
L3 hook around the property setters and the layer-removal call, because L2
`set_focus` cannot call the VM. Tests cite `LayerIntf.cpp:3683-3696`,
`:1183-1206`, `:3801-3832`, `LayerManager.cpp:690-711`.

### Step 3 — `onBeforeFocus`, `onSearch*Focusable`, re-entrancy

Fire `onBeforeFocus` on the candidate before the transition and honour the
returned `FocusWork`; fire `onSearchNextFocusable`/`onSearchPrevFocusable` from
the L2 search. Add the `FocusChangeLock`-equivalent typed error
(`TVPCannotChangeFocusInProcessingFocus`). Verify the redirect semantics against
a reference test first (§2.4 is unresolved). Tests cite `LayerIntf.cpp:3731-3744`,
`:3615-3660`, `LayerManager.cpp:770-776`.

### Step 4 — action-owner forwarding (fidelity, unused by the test game)

Retain the Layer's action owner (the first `new Layer(...)` argument) and make
the Layer native `onFocus`/`onBlur` build the event dictionary and call
`ActionOwner.action(event)` with members `focused` / `blurred`+`direction`, as
`dispatch_action_event` already does for the Window. Needed for a script's
`super.onFocus(...)` to reach a global `action`, but the test game's action
dispatcher does not handle `onFocus` (§7), so this is not on the critical path.
Do **not** add `onFocus`/`onBlur` to the `Window` native class: the reference
`Window` has no such members.

### Step 5 — remove the non-reference click-focus

Delete the `focus_clicked_layer` call so mouse click matches
`LayerManager.cpp:374-399`. Separate from the event work and script-observable;
get it reviewed on its own.

### Genuinely separate (do not bundle)

* **Window focus.** Already correct: `Focused(true/false)` maps to
  `onActivate`/`onDeactivate` (`crates/compat/src/event.rs`), matching
  `WindowIntf.cpp:556-570`. No layer-focus change is needed.
* **Controller input.** Controller → key mapping is the existing input path; the
  layer focus effect is exactly row 4/5 once the keyboard path is wired.
* **Hint / attention / IME system.** `SetFocusTo` also drives
  `SetImeModeOf`, `SetAttentionPointOf`, `DisableAttentionPoint`
  (`LayerManager.cpp:809-815`). The port has `setAttentionPos` storage but no
  attention/hint wiring; this is its own subsystem and is not required for
  `onFocus`/`onBlur`.
* **Modal layers beyond focus.** `setMode`/`removeMode` already move focus in
  L2; only the event firing (Step 1) and the modal-disabled predicate in
  `node_focusable` are relevant here.

## 7. Secondary evidence from the test game (not the oracle)

`/mnt/DATA/Games/Others/test/data.xp3` was read in memory (no files written) via
`scripts/extract_xp3.py`'s parser:

* `system/editlayer.tjs:35` sets `focusable = true`; `:250` calls `focus()` from
  `onMouseDown` (so click-focus is script-driven, confirming §5.3); `:254` and
  `:335` call `window.primaryLayer.focus()` to return focus; `:347-358` override
  `onFocus(focused, direction)` / `onBlur(focused)` and call `super.onFocus(...)`
  / `super.onBlur(...)` — i.e. the **layer-object positional** path, not the
  global `action`.
* `system/window.tjs:54-59` makes `primaryLayer` `focusable = true` and
  `joinFocusChain = false`, exactly the reference's "primary is focusable but
  not in the chain" pattern.
* A whole-archive grep found `onFocus` only in `system/editlayer.tjs`; no other
  `.tjs`/`.ks` file references it. So this game does not exercise the
  action-owner forwarding path for focus (Step 4).

This is corroborating evidence about one game's script, not a behavioural
oracle; the C++ references in §1–§4 remain authoritative.

## 8. Open questions

1. **`onBeforeFocus` redirect mechanism** (§2.4): the native reads `param[0]`
   after invoking the action owner, which looks unchanged; the vendored source
   may be a reconstruction (AGENTS §9.9). Settle with a reference test.
2. **`onSearch*Focusable` redirect** has the same shape
   (`LayerIntf.cpp:10340-10411`); same caveat.
3. **KAG3 `Button`/`Menu` exact hooks** could not be checked: no KAG3 source is
   vendored and the test game's KAG-equivalent only uses `editlayer.tjs`.
   Settle by unpacking a KAG3 title and grepping for `onFocus`. The engine-side
   contract does not depend on it.
4. The port tree was being modified during this audit; re-check line numbers
   before implementing.

## 9. One-line correctness checklist for the implementation

* Fire only when the focused layer actually changes.
* `onBlur` first, then `onFocus`, synchronously.
* `onBlur(arg)` = the **new** layer (or null).
* `onFocus(arg0, arg1)` = the **old** layer (or null), `direction` (0/1).
* `direction`: `focusNext`/blur-to-next = `true`; `focusPrev` = `false`;
  `focusedLayer = x` = `false`; `focus()` = its argument, default `true`.
* Never validate-focus and fire for a layer that is not `node_focusable`.
* Do not fire anything on mouse click or window activate.
