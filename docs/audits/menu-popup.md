# Audit and specification — the menu / popup surface (`MenuItem`, `popup`, `TVPShowPopMenu`)

Read-only specification task. No source files were changed except this document.
The oracle is `reference/cpp/**`; the vendored tree is the **cocos2d** branch of
TVP, and in that build there is **no native popup at all** — the Win32
`TrackPopupMenuEx` body is commented out and `popup` always calls
`TVPShowPopMenu(this); return 1;`. The menu is drawn by the engine itself.

There is deliberately no game data and no usage example in this exercise. The
goal is the reference's full behaviour, not the subset one title needs
(AGENTS D28). Pixel appearance may be approximated (D34); every value a script
can observe — which rows exist, what a click does, return values, raised
errors, event order — must match exactly.

---

## 0. Executive summary

| # | finding | severity for the port |
|---|---|---|
| E1 | `popup(flags,x,y)` **does** the reference work (`TVPShowPopMenu` → an engine-drawn form) and returns `1`; the port returns `1` and draws nothing (`crates/compat/src/natives/menu_item.rs:747-777`). This is the single missing behaviour. | high |
| E2 | `Window.menu` — the reference's script entry point for a menu tree — is **not registered** in the port (`reference/cpp/core/base/ScriptMgnIntf.cpp:537-542` vs. `crates/compat/src/natives/visual/window.rs`, and the `Window` member list in `docs/surface/runtime_surface.json`). `scripts/native_parity.py` cannot see it: it only parses `TJS_BEGIN_NATIVE_*` macros, not `windowclass->PropSet(TJS_W("menu"), …)`. | high |
| E3 | The port's logical tree (`MenuState`, `MenuNode`) and all non-popup members are already faithful and tested (`crates/compat/src/natives/menu_item.rs`, `crates/compat/tests/menu_item.rs`). The plan below only has to add the *presentation + activation* path. | — |
| E4 | Reference row classification, dismissal and event order are **not** the obvious ones; they are the spec's real content (§1.5, §2). The three traps: `checked` is never toggled by the form, a disabled *constant* row still dismisses the menu, and `visible=false` does not hide a row. | high |
| E5 | The reference is modal over the game for **keyboard** (the topmost UI form consumes every key; `reference/cpp/core/environ/cocos2d/MainScene.cpp:804-812`) and, by cocos scene ordering, for pointer/touch. A faithful port needs an input gate. | medium |
| E6 | Opening/closing the menu fires **no** script event. `Window.onPopupHide` is registered but its only trigger is commented out (`reference/cpp/core/environ/impl/TVPWindow.cpp:544`). | low |
| E7 | Linux native menus are unsupported (XDG portal has no menu interface). The engine-drawn form is the *reference*, not a workaround; the platform seam exists only so a future Android popup can be added without touching L3. | by decision |

**Recommended shape of the fix (detail in §3):** add a generic overlay model to
L2 (`engine`), let L3 translate `MenuState` into it and own the row→`onClick`
callback, project the overlay on top of the layer tree in `project_scene`, and
gate the platform→TJS input path while it is open. Ranked as four increments
(§3.5); increment 1 (a flat, text-only list that classifies and dismisses
exactly like the reference) is visibly useful on its own.

---

## 1. The surface

### 1.1 How a script reaches a menu: `Window.menu` and `MenuItem`

There is no `Menu` class. A menu is a `MenuItem` **tree**: the root is a
container whose `children` are the rows. The reference exposes the whole tree
through a per-window property:

* `Window.menu` (getter only) is registered on the `Window` class as a
  `WindowMenuProperty` (`reference/cpp/core/base/ScriptMgnIntf.cpp:537-542`;
  implementation `reference/cpp/core/visual/impl/MenuItemImpl.cpp:65-87`).
  `PropGet` reads the owning window's `HWND` and looks it up in the process map
  `MENU_LIST`; if absent it creates one with
  `TVPCreateMenuItemObject(objthis)` and caches it (`MenuItemImpl.cpp:24-63`,
  `:65-82`). `PropSet` always returns `TJS_E_ACCESSDENYED` (`:84-87`).
* `TVPCreateMenuItemObject(window)` constructs a `MenuItem` with **both**
  arguments equal to the window (`reference/cpp/core/visual/MenuItemIntf.cpp:559-575`),
  so the window is both the action owner and the attached window.
* `UpdateMenuList` (called from `PropGet`) drops map entries whose window no
  longer exists (`MenuItemImpl.cpp:49-63`); `MENU_LIST` is keyed by `HWND`.
* The global `registerObject(TJS_W("MenuItem"), TVPCreateNativeClass_MenuItem())`
  is in `ScriptMgnIntf.cpp:542`. The block inside `MenuItemImpl.cpp:451-474`
  that would have done the same is commented out; the live one is in
  `ScriptMgnIntf.cpp`.

So a script's canonical sequence is:

```tjs
var item = new MenuItem(Window, "Label");   // or Window.menu.add(new MenuItem(...))
Window.menu.add(item);
Window.menu.popup(0, x, y);                  // flags/x/y are ignored (see §1.4)
```

The port's entry points today: `new MenuItem(...)` works and the tree is
faithful, but `Window.menu` is absent (E2) and `popup` is inert (E1). Fixing
`Window.menu` is part of increment 1.

### 1.2 `tTJSNI_BaseMenuItem` — the shared base

Defined in `reference/cpp/core/visual/MenuItemIntf.h:50-116`. It owns:

* `Children` — an `ObjectVector<tTJSNI_BaseMenuItem>` (order-preserving,
  duplicate-rejecting, insertion at an index; `MenuItemIntf.h:24-48`).
* `Window` — the attached window, resolved from constructor argument 1
  (`MenuItemIntf.cpp:45-73`). It is set once and propagates down the ancestor
  chain for event delivery.
* `Owner` — the TJS object; `ActionOwner` — the closure that receives the
  default action (`MenuItemIntf.cpp:45-73`, `MenuItemIntf.h:104-116`).
* `Parent`, `ChildrenArrayValid`, `ChildrenArray`, `ArrayClearMethod`
  (`MenuItemIntf.h:64-70`).

Behaviour:

* `Construct` (`MenuItemIntf.cpp:45-73`): `numparams < 1` returns
  `TJS_E_BADPARAMCOUNT`; argument 0 is the action owner via
  `AsObjectClosure()` (throws on a non-object); argument 1, when it is an
  object, must be a `Window` native instance or it throws
  `TVPSpecifyWindow` (`:61-70`). An **omitted** first argument is tolerated in
  practice because the derived `tTJSNI_MenuItem::Construct` ignores the base's
  return value (`reference/cpp/core/visual/impl/MenuItemImpl.cpp:104-127`).
* `Invalidate` (`MenuItemIntf.cpp:75-108`): cancels source + input events,
  cascade-`Invalidate`s every child object and clears `Children`, then drops
  `Window`/`Parent`/`ChildrenArray`/`ActionOwner`.
* `CastFromVariant` (`MenuItemIntf.cpp:110-125`): a method argument must be a
  `MenuItem` object, else `TVPSpecifyMenuItem`.
* `AddChild`/`RemoveChild` (`MenuItemIntf.cpp:127-143`): maintain `Parent` and
  `ChildrenArrayValid`; this is the only place `Parent` is set.
* `GetChildrenArrayNoAddRef` (`MenuItemIntf.cpp:145-194`): lazily builds a TJS
  `Array` of the child objects and reuses it until dirty. The returned array is
  the **same object** across reads while valid.
* `GetRootMenuItem` (`MenuItemIntf.cpp:219-228`): walks to the topmost ancestor.
* `OnClick` (`MenuItemIntf.cpp:196-217`): the delivery gate — see §1.6.

The class registration (`add`, `insert`, `remove`, `popup`, `onClick`,
`fireClick`, all properties) is `MenuItemIntf.cpp:230-556`.

### 1.3 `tTJSNI_MenuItem` — the platform implementation

Defined in `reference/cpp/core/visual/impl/MenuItemImpl.h:35-88`. It keeps the
logical state the base does not:

| field | default | setter/getter |
|---|---|---|
| `Caption` | `""` (or constructor arg 1) | `SetCaption` `MenuItemImpl.cpp:214-219`; `GetCaption` `:221-224` |
| `Shortcut` | `""` | `SetShortcut` `:281-285` is a **no-op**; `GetShortcut` `:287-290` **always returns `""`** |
| `IsChecked` | `false` | `SetChecked`/`GetChecked` `:226-243` |
| `IsEnabled` | `true` | `SetEnabled`/`GetEnabled` `:245-255` |
| `IsRadio` | `false` | `SetRadio`/`GetRadio` `:269-279` |
| `IsVisible` | `true` | `SetVisible`/`GetVisible` `:292-301` |
| `GroupIndex` | `0` | `SetGroup`/`GetGroup` `:257-267` |
| `IsAttched` | `true` (member `Window` set) | internal (`:95-127`) |

Key non-obvious behaviour:

* **`checked` radio-group side effect** (`MenuItemImpl.cpp:226-243`): writing
  `checked = true` on an item whose `IsRadio` is set clears `IsChecked` on every
  *direct sibling* with `IsRadio && GroupIndex == this.GroupIndex`, then sets
  this item's `IsChecked`. Writing `false` has no group effect. This is the only
  cross-item mutation in the class, and it is script-observable.
* **`shortcut` is inert**: the setter discards the string, the getter always
  returns `""` (`:281-290`). (The `textToKeycode`/`keycodeToText` tables are a
  separate static convenience; `MenuItemImpl.cpp:348-393`, `:422-440`.)
* Every VCL/`TMenuItem*` call is commented out; the class no longer backs itself
  with an OS menu object (`MenuItemImpl.h:37`, throughout `MenuItemImpl.cpp`).
* `CanDeliverEvents` (`:143-157`): true only when the item and **every ancestor**
  is `enabled`.
* `GetIndex` (`:198-207`): the item's position in its parent's child vector, or
  `0` for a root. `SetIndex` (`:209-212`) is a no-op (the VCL `setMenuIndex` is
  commented out).
* `GetMenuItemHandleForPlugin` (`:193-196`) always returns `nullptr`, so the
  `HMENU` property (`:409-420`) reads `0`.

The `windowEx` plugin adds `rightJustify`, `bmpItem`, `bmpChecked`,
`bmpUnchecked`, the `bi*` class constants, and `popupEx`
(`reference/cpp/plugins/windowEx.cpp:997-1239`). `popupEx` is an empty
`TJS_S_OK` stub (`windowEx.cpp:1183-1188`). These are already ported
(`crates/compat/src/natives/plugins/window_ex/menu_item.rs`).

There is **no `Items` member** on `MenuItem` in this tree (nor anywhere in
`reference/cpp/core/visual` or the menu files). `Items` belongs to the
`WIN32Dialog` API noted in `TODO.md` §5, which is a different, still-open
surface. The complete `MenuItem` script surface is the 21 members listed in
`docs/surface/runtime_surface.json` (`MenuItem` array) plus the plugin members.

### 1.4 `popup` / `TrackPopup`: the live path versus the dead path

The `popup` native method (`reference/cpp/core/visual/MenuItemIntf.cpp:285-303`):

```cpp
TJS_BEGIN_NATIVE_METHOD_DECL(popup)
    TJS_GET_NATIVE_INSTANCE(_this, tTJSNI_MenuItem);
    if(numparams < 3) return TJS_E_BADPARAMCOUNT;
    tjs_uint32 flags = (tTVInteger)*param[0];
    tjs_int x = *param[1];
    tjs_int y = *param[2];
    tjs_int rv = _this->TrackPopup(flags, x, y);
    if(result) *result = rv;
    return TJS_S_OK;
```

So the arity check precedes any conversion, and all three arguments undergo
`AsInteger` coercions (the port already reproduces these; a `null`, octet or
object argument raises a variant-convert error — `crates/compat/src/natives/menu_item.rs:747-777`).

`TrackPopup` (`reference/cpp/core/visual/impl/MenuItemImpl.cpp:309-345`):

```cpp
tjs_int tTJSNI_MenuItem::TrackPopup(tjs_uint32 flags, tjs_int x, tjs_int y) const {
    TVPShowPopMenu((tTJSNI_MenuItem *)this);
    return 1;
    //  ... everything below is commented out ...
    //  HWND hWindow;
    //  if (GetRootMenuItem() && GetRootMenuItem()->GetWindow()) { ... }
    //  HMENU hMenuItem = GetMenuItemHandleForPlugin();
    //  POINT scrPoint; scrPoint.x = x; scrPoint.y = y;
    //  BOOL rvScr = ::ClientToScreen(hWindow, &scrPoint);
    //  BOOL rvPopup = TrackPopupMenuEx(hMenuItem, flags, scrPoint.x, scrPoint.y, hWindow, nullptr);
    //  return rvPopup;
}
```

Live versus dead in this tree:

| path | status |
|---|---|
| `TVPShowPopMenu(this); return 1;` | **live** (`MenuItemImpl.cpp:313-315`) |
| Win32 `ClientToScreen` + `TrackPopupMenuEx` | **dead** — commented out (`MenuItemImpl.cpp:316-345`) |
| `#include <Menus.hpp>` / `TMenuItem* MenuItem` member | dead/commented (`MenuItemImpl.h:13,18,37`) |
| `CreateShortCutKeyCodeTable`'s `MapVirtualKey`/`GetKeyNameText` loop | dead — `#if 0` (`MenuItemImpl.cpp:359-378`) |
| `MenuItemImpl.cpp`'s own `gWindowMenuProperty` + `registerObject("MenuItem")` | dead — commented (`:451-474`); the live one is `ScriptMgnIntf.cpp:537-542` |
| `TVPWindow.cpp:544` `DeliverPopupHide` (`onPopupHide`) | dead — commented |

Consequences a script observes:

* `flags` is converted to an integer and **discarded**. No flag changes
  anything.
* `x` and `y` are converted and **discarded**. `TVPShowPopMenu` does not
  receive them and `TVPInGameMenuForm` has no `x`/`y` input; the form is laid
  out by `iTVPBaseForm` (navi bar at top, body centred). The popup is a
  centred/full-scene form, **not** anchored to the click point.
* The return value is **always `1`**, for any item, with or without children,
  with or without a window. `0` is documented by the commented code but never
  returned in the live build.
* `popup` works on a detached item (no window): the form still opens; only
  event *delivery* needs a window (§1.6).

### 1.5 `TVPShowPopMenu` and `TVPInGameMenuForm`

`TVPShowPopMenu` is one line (`reference/cpp/core/environ/ui/InGameMenuForm.cpp:115-118`):

```cpp
void TVPShowPopMenu(tTJSNI_MenuItem *menu) {
    TVPMainScene::GetInstance()->pushUIForm(
        TVPInGameMenuForm::create(std::string(), menu));
}
```

It is declared in `MenuItemImpl.cpp:306` and defined only here. The form is
built by `TVPInGameMenuForm::create` (`InGameMenuForm.cpp:14-21`):
`initFromFile(Csd::createNaviBar(), Csd::createListView(), nullptr)` then
`initMenu(title, item)`. `iTVPBaseForm::initFromFile` (`reference/cpp/core/environ/ui/BaseForm.cpp:83-138`)
lays out a vertical navi bar at `TOP`, the body at `CENTER_VERTICAL`, and an
optional footer at `BOTTOM`; the popup passes no footer. `create` with an empty
title makes `initMenu` use the **root item's caption** as the navi-bar title
(`InGameMenuForm.cpp:40-49`).

**Row construction** (`InGameMenuForm.cpp:38-67`):

```cpp
int count = item->GetChildren().size();
int idx = 0;
ttstr seperator = TJS::TJSMapGlobalStringMap(TJS_W("-"));
for(int i = 0; i < count; ++i) {
    tTJSNI_MenuItem *subitem = static_cast<tTJSNI_MenuItem *>(item->GetChildren().at(i));
    ttstr caption; subitem->GetCaption(caption);
    if(caption.IsEmpty() || caption == TJS_W("+")) continue;
    _list->pushBackCustomItem(createMenuItem(idx, subitem, caption.AsStdString()));
    if(caption != seperator) ++idx;
}
```

`createMenuItem` (`InGameMenuForm.cpp:69-113`) chooses the widget. The order of
the tests is part of the spec:

| test (in order) | widget | click action |
|---|---|---|
| `!item->GetChildren().empty()` | `tPreferenceItemSubDir` (submenu row) | `pushUIForm(create(caption, item))` — opens a nested form |
| else `item->GetGroup() > 0 \|\| item->GetRadio()` | `tPreferenceItemCheckBox` | `item->OnClick(); popAllUIForm();` |
| else `item->GetChecked()` | `tPreferenceItemCheckBox` | `item->OnClick();` (no dismissal) |
| else `caption == "-"` | separator (`Csd::createSeperateItem`) | none |
| else | `tPreferenceItemConstant` | `scheduleOnce(popAllUIForm, 0, "close_menu"); item->OnClick();` |

Important consequences:

* **`visible` is ignored.** `initMenu` iterates `GetChildren()` with no
  `GetVisible` filter, so an item with `visible=false` is rendered like any
  other. `visible` is only a getter/setter value.
* **`enabled` is ignored by the row builder.** There is no disabled styling and
  no early return in `createMenuItem`. A disabled item is rendered normally;
  the `OnClick` gate (§1.6) is what suppresses the event. But the dismissal is
  **not** gated: a disabled *constant* row still schedules
  `popAllUIForm`, and a disabled *group/radio* row still calls it synchronously.
* **Classification precedence matters.** A checked item that also has children
  is a submenu, not a checkbox. A checked item with `group>0` is a checkbox
  (group branch wins). Only a checked, childless, non-group/radio item takes the
  plain-checked branch. A `"-"` caption is a separator only if it is childless
  and not checked/group/radio.
* **Checkbox rows reflect `checked` visually only at build time.**
  `tPreferenceItemCheckBox::initController` calls `checkbox->setSelected(_getter())`
  and wires the checkbox's SELECTED event to `_setter`
  (`reference/cpp/core/environ/ui/PreferenceForm.cpp:90-105`). The menu's
  `_setter` lambdas ignore the boolean argument and only call `item->OnClick()`
  (`InGameMenuForm.cpp:73-91`). **The form never writes `item.checked`.** The
  checkbox's own widget state toggles, but the `checked` property does not
  change unless the script's `onClick` handler changes it.
* Nested forms: the submenu branch calls `create(caption, item)` and pushes it,
  so nesting is supported. The parent form stays on the stack under it.
* The separator string is looked up through the KAG global string map
  (`TJSMapGlobalStringMap`, `InGameMenuForm.cpp:53`); with no remap it is `"-"`.

**Dismissal** (`reference/cpp/core/environ/cocos2d/MainScene.cpp`):

* `pushUIForm` (`:500-545`) adds the new form as a child of `UINode` and, for
  the default `eEnterAniOverFromRight`, slides the previous top form left and
  the new form in from the right.
* `popUIForm(this)` (`:547-600`) removes just the top form (used by the navi
  bar's left/back button, `InGameMenuForm.cpp:30-34`, and by Back).
* `popAllUIForm()` (`:951-966`) removes **every** child of `UINode`, i.e. all
  stacked forms at once — not only menus but any open UI form (preference
  dialogs etc. share `UINode`).
* `iTVPBaseForm::onKeyPressed` (`BaseForm.cpp:139-144`) pops the form only for
  `KEY_BACK`. There is **no arrow-key / Tab navigation** and no selection
  cursor in this form. `TVPMainScene::onKeyPressed` (`MainScene.cpp:804-812`)
  forwards every key to the topmost form and returns, so while a menu is open
  the game receives **no** keyboard events.

### 1.6 Events

`tTJSNI_BaseMenuItem::OnClick` (`reference/cpp/core/visual/MenuItemIntf.cpp:196-217`):

1. `if(!CanDeliverEvents()) return;` — suppressed when the item or any ancestor
   is disabled.
2. Walk `this` → `Parent` until an item with a `Window` is found; if none,
   return. Then require `Window->CanDeliverEvents()`, which in the window impl
   is `_form && GetVisible() && _form->GetFormEnabled()`
   (`reference/cpp/core/visual/impl/WindowImpl.cpp:1154-1159`).
3. Post the TJS event **`onClick`** to `Owner` with zero arguments and
   `TVP_EPT_IMMEDIATE` (`MenuItemIntf.cpp:215`), so it is delivered
   synchronously. `TVPPostEvent` drops it while events are disabled
   (`reference/cpp/core/base/EventIntf.cpp:207-224`).
4. Call `TVPDoSaveSystemVariables()` (`MenuItemIntf.cpp:216`).

What the script observes from `OnClick`:

* If the object defines its own `onClick` member, that method runs with **no
  arguments**. The member is the event target name, so TJS dispatches to it.
* If it does not, the registered native `onClick` runs
  (`MenuItemIntf.cpp:305-317`); it invokes `ActionOwner.action(event)` where
  `event` is a dictionary with `type = "onClick"` and `target = the MenuItem
  object` (`TVP_ACTION_INVOKE_BEGIN(0,"onClick",objthis)` +
  `TVP_ACTION_INVOKE_END`, `reference/cpp/core/base/EventIntf.h:208-237`;
  `TVPCreateEventObject`, `EventIntf.cpp:709-737`). A missing/failing `action`
  is ignored (the `FuncCall` result is unchecked).
* `fireClick()` (registered method, `MenuItemIntf.cpp:319-330`) calls the same
  `OnClick`. The form calls `item->OnClick()` directly.

**There is no open or close event.** `TVPShowPopMenu`/`pushUIForm` post
nothing, and `popUIForm`/`popAllUIForm` post nothing. `onPopupHide` exists on
`Window` (`reference/cpp/core/visual/WindowIntf.cpp:547-554`, native default
`:1211-1220`) but its only caller, `TVPWindowLayer::OnMouseDown`'s
`if (!CanSendPopupHide()) DeliverPopupHide();`, is commented out
(`reference/cpp/core/environ/impl/TVPWindow.cpp:544`); no other caller exists in
the tree. A script may define it; it never runs. `TVPControlAdDialog` in
`pushUIForm`/`popAllUIForm` is ad-banner control, not script.

### 1.7 Other users of the same form

* `TVPGameMainMenu` — the on-screen menu button
  (`reference/cpp/core/environ/ui/GameMainMenu.cpp:86-100`) looks up
  `TVPGetMenuDispatch((intptr_t)TVPGetActiveWindow())` and, when the window's
  menu root has children, pushes `TVPInGameMenuForm::create("GameMenu", menu)`.
  This is the second and only other caller of the form factory; it uses a fixed
  title.
* The **row widget classes** (`tPreferenceItemCheckBox`,
  `tPreferenceItemSubDir`, `tPreferenceItemConstant`,
  `tPreferenceItemWithHighlight`) are shared with the preference UI
  (`reference/cpp/core/environ/ui/PreferenceForm.{h,cpp}`), which is why the
  menu popup can be built without new widget code in the reference.
* `popAllUIForm` is global to `UINode`, so a menu activation can also dismiss
  an unrelated stacked form; and a form pushed while the menu is open stacks
  above it.

---

## 2. Script-visible outcomes (the contract to reproduce)

Notation: *observed* = what a script can read/do; *effect* = what mutates.

### 2.1 Members

| member | observed (reference) | port today |
|---|---|---|
| `new MenuItem()` | accepted (base returns `BADPARAMCOUNT` but derived ignores it); no action owner | matches (`menu_item.rs:508-616`) |
| `new MenuItem(owner)` | `owner` must be an object (else convert error); becomes `ActionOwner` | matches |
| `new MenuItem(owner, windowObj)` | window must be a live `Window` (else `TVPSpecifyWindow`); no caption | matches |
| `new MenuItem(owner, "cap")` | non-object arg 1 = initial `caption`; no window | matches |
| `add(item)` | appends; duplicate is a no-op; sets `item.parent`; returns `void` | matches |
| `insert(item, idx)` | inserts at/clamped index; duplicate rejected; returns `void` | matches |
| `remove(item)` | unlinks if a child; non-child is a no-op (the throw is commented out); returns `void` | matches |
| `popup(flags,x,y)` | `argc<3` raises; each arg `AsInteger` (raises on object/`null`/octet); otherwise **opens the form**, returns `1` | coerces + returns `1`; **does not open** |
| `onClick()` | native default: `ActionOwner.action({type:"onClick",target:this})`, no-args fallback path; returns `void` | matches (`:856-921`) |
| `fireClick()` | `OnClick` gate; returns `void` | matches (`:780-855`) |
| `caption` | get/set string; used for title (root) and row text | matches |
| `checked` | get/set bool; setting `true` on a radio clears same-group radio siblings | matches (`:1088-1158`) |
| `enabled` | get/set bool; default `true`; gates `OnClick`; **does not grey or hide the row** | matches (gate); row renderer not built |
| `group` | get/set int; default `0` | matches |
| `radio` | get/set bool; default `false` | matches |
| `shortcut` | set is a no-op; **get always `""`** | matches (`:971-1019`) |
| `visible` | get/set bool; default `true`; **does not hide the row** | matches |
| `parent` | get only; the parent object or `null` | matches |
| `children` | get only; a cached `Array` of child objects; same object while valid | matches |
| `root` | get only; topmost object | matches |
| `window` | get only; attached window or `null` | matches |
| `index` | get/set; get = position in parent (0 for root); **set is a no-op** | matches |
| `HMENU` | get only; always `0` | matches |
| `textToKeycode` / `keycodeToText` | get-only static tables (reference real maps are `#if 0`; the live ones are `BkSp=8`, `PgUp=33`, `PgDn=34`) | matches |
| plugin `rightJustify`, `bmp*`, `bi*`, `popupEx` | values per `windowEx.cpp`; `popupEx` is an empty `TJS_S_OK` stub | matches |
| `Window.menu` | get only; per-window `MenuItem` root; the set is denied | **missing** |

### 2.2 The popup presentation and activation

| observable | reference behaviour |
|---|---|
| return of `popup` | always `1` |
| `flags`, `x`, `y` effect | none beyond the `AsInteger` coercion |
| rows shown | root's children, in order, minus items whose caption is `""` or `"+"` |
| submenu | children present → a submenu row; click opens a nested form titled with that item's caption |
| checkbox | childless and (`group>0` or `radio` or `checked`) → checkbox row; group/radio click also closes **all** UI forms; checked-only click does not close |
| separator | childless, not checked/group/radio, caption `==` the global-map of `"-"` → separator; not clickable |
| constant | otherwise; click schedules `popAllUIForm` for the next frame and fires `onClick` immediately |
| `visible=false` | no effect on the row |
| `enabled=false` | row looks normal; `onClick` suppressed; constant/group-row **dismissal still happens** |
| click toggles `checked`? | **no** — only the script's `onClick` may change it |
| `onClick` arguments | none to a script override; the default action receives `{type:"onClick", target:item}` |
| order on constant click | `scheduleOnce(popAllUIForm)` first, then `OnClick()` synchronously; the close runs next frame |
| order on group/radio click | `OnClick()` then `popAllUIForm()` synchronously |
| open/close events | none |
| keyboard | only Back pops the top form; every other key is consumed by the form; no arrow navigation |
| modal over the game | keyboard: yes (topmost form consumes all keys). Pointer/touch: yes for the engine's own input (form is above the game and consumes); the exact swallow is cocos-internal |

---

## 3. Implementation plan for this codebase

### 3.1 What already exists (reuse, do not reinvent)

**Menu tree (L3).** `crates/compat/src/natives/menu_item.rs` holds the whole
logical tree in per-binding `CompatServices` state as `MenuState`
(`:96-116`) mapping id → `MenuNode` (`:52-89`), with `parent`/`children`,
flags, retained TJS objects (`object`, `action_owner`, `window`), a cached
`children` array, a `can_deliver` enabled-walk (`:291-305`), and
`window_chain`/`window_can_deliver` (`:307-338`). `menu_item_fire_click`
(`:780-855`) already implements the full `OnClick` gate and event dispatch
(including `System.eventDisabled` and `TVPDoSaveSystemVariables`). `popup`
(`:747-777`) is the only member that does not do its reference work. The
`Window.menu` property is not registered anywhere.

**Text.** L2 already owns a shaping/rasterizing stack in
`crates/engine/src/text.rs`: `TextEngine`, `TextStyle`, `TextLayout`,
`GlyphAtlas::new(&RenderPipeline, size)` (`:931`), `positioned` glyph
construction, `project_text(&mut RenderScene, parent, &atlas, &glyphs, z)`
(`:1186-1226`) and the buffer compositor `compose_text_into` (`:1966`). The
only production caller of `project_text` today is `crates/engine/tests/text.rs`;
the production game-text path is L3's `compat::resource::ResourceResolver`
(`crates/compat/src/resource.rs:36-53`), which draws into a layer bitmap via
`draw_text_into_bitmap` (`resource.rs:366-490`).

**Render tree.** `crates/platform/render/src/scene.rs` defines
`RenderNode`/`RenderScene` with `RenderContent::Solid`, `Texture`, `Glyphs`
(`:30-45`), per-node `z`, `opacity`, `color` and `clip` (`:107-150`). It is L1
structure but L2-owned. `crates/engine/src/projection.rs:560-572`
(`project_scene`) builds it from `Scene` and appends one node per drawn layer;
`crates/app/src/lib.rs:404-428` (`reproject_render_scene`) rebuilds it only when
`Scene::revision()` changes, then `Host::step` renders it (`lib.rs:373-393`).
There is no non-layer overlay node today.

**Input.** `crates/compat/src/event.rs` maps `PlatformEvent` to `TjsEvent`
(`:90-219`) and dispatches to the first window and to the hit/focused layer
(`:241-345`); `hit_layer` (`:508-537`) front-to-back hit-tests
`Scene::layer_at`; mouse/touch capture lives in `InputDispatchState`
(`:462-477`). The driver is `VmHost::tick`
(`crates/vm/src/host.rs:157-172`): for each event it calls
`adapter.apply_input_event`, then `adapter.map`, then `adapter.dispatch`. That
loop is the only place platform input becomes TJS events, so it is the natural
gate. `crates/app/src/game.rs:270-282` (`GameHost::step`) owns the per-frame
event slice and the audio/render order.

**Layer/scene.** `crates/engine/src/scene.rs` has `Scene::window_layer_order`
(`:2285`), `Scene::layer_at` (`:1565`), window focus domains, and a per-scene
`revision()` (`:527-533`). An overlay is **not** a `LayerState` and is not in
`window_layer_order`, so scripts never see it through the layer surface.

### 3.2 Design

Add a generic, TVP-free **popup overlay** to L2 and wire it with the smallest
possible L3 adapter.

1. **L2 model** — new module `crates/engine/src/overlay.rs` (or
   `engine::menu`; the name should stay generic because `WIN32Dialog` will want
   the same shape):

   ```rust
   pub struct Overlay {            // one open popup
       pub levels: Vec<OverlayLevel>,   // supports submenu nesting
       pub panel: RectF,                // computed layout, in logical pixels
   }
   pub struct OverlayLevel {
       pub title: String,
       pub rows: Vec<OverlayRow>,
   }
   pub struct OverlayRow {
       pub id: u32,                     // opaque; the caller's key
       pub caption: String,
       pub kind: OverlayKind,
       pub enabled: bool,
   }
   pub enum OverlayKind { Constant, Checkbox { checked: bool }, Separator, Submenu }
   ```

   The model carries no TJS and no TVP vocabulary. `Scene` gains an
   `overlay: Option<Overlay>` (or `Host` owns it and `project_scene` takes it
   as an argument). Keeping it on `Scene` lets `project_scene` append it
   automatically and lets the L3 adapter reach it through the already-attached
   scene pointer.

2. **L2 layout + hit-test** — `Overlay::layout(&mut self, logical_size, &FontMetrics)`
   computes the panel/row rects (centred, navi-bar-like title) and
   `Overlay::hit(&self, x, y) -> Option<u32>` returns the row id under a point
   (separators return `None`). Hit-testing is pure L2 geometry. A row's
   enabled/checked state is display data; it does not affect the hit.

3. **L2 render** — extend `project_scene` (`projection.rs:560`) to append the
   overlay after every window: a `Solid` panel/row background, a `Solid`
   highlight, and `project_text` glyph nodes for captions/title, all with a `z`
   above the highest layer node, parented to the scene root. The font is the
   `TextEngine` default face (or a face the app loads at startup). Pixel
   fidelity is explicitly allowed to differ (D34); the row set/order/behaviour
   is not.

4. **L3 translation + activation (thin)** — a new function in
   `crates/compat/src/natives/menu_item.rs` (or a sibling `overlay.rs`):
   `build_overlay(services, root_id) -> (engine::overlay::Overlay, RowMap)`,
   where `RowMap: HashMap<u32 /*row id*/, u64 /*node id*/>`. Classification
   mirrors `createMenuItem` exactly (children → `Submenu`; else
   `group>0 || radio` → `Checkbox` with a close-all dismissal; else `checked` →
   `Checkbox` with no dismissal; else caption `== mapped "-"` → `Separator`;
   else `Constant`), skipping captions `""` and `"+"`. Refactor the body of
   `menu_item_fire_click` into a reusable `deliver_click(services, host, id)`
   and have the activation callback call it.

   `menu_item_popup` then: perform the existing arity/coercion, build the
   overlay, install it, and return `1`. `Window.menu` is registered in
   `crates/compat/src/natives/visual/window.rs` as a per-window `MenuItem` root
   (a `HashMap<window_id, MenuNode root>` in `MenuState`), matching
   `WindowMenuProperty`'s read-only, per-window, lazily created root.

5. **Activation routing** — the overlay keeps only row ids. The L3 adapter
   registers a callback (stored in `CompatServices`, not in L2) that maps a row
   id to its node id and applies the reference sequence:
   * `Submenu` → push a level built from that node's children (title = the
     item's caption).
   * `Constant` → schedule a close-all for the next frame, then
     `deliver_click(node)` immediately. To match the reference exactly, the
     close-all must be scheduled **before** `deliver_click` so a click handler
     that opens another popup still has it dismissed next frame.
   * `Checkbox` (group/radio) → `deliver_click(node)` then close-all
     synchronously.
   * `Checkbox` (plain) → `deliver_click(node)`, no dismissal.
   * `Separator` → no activation.
   `deliver_click` already enforces the enabled/ancestor and window gates, so a
   disabled constant row dismisses without firing, matching §1.5.

6. **Input interception** — in `VmHost::tick`
   (`crates/vm/src/host.rs:157-172`) or immediately before it in
   `GameHost::step` (`crates/app/src/game.rs:270-282`), when the overlay is
   open: pointer move/press/release are hit-tested against the overlay and
   consumed (release on a row activates it); every key except Back/Escape is
   consumed; Back/Escape pops one level (and closes when the stack empties).
   Nothing is delivered to the game's window/layer while the overlay is open,
   reproducing the reference's keyboard modality and pointer consumption. If
   the event is not consumed, map+dispatch proceeds as today.

7. **Destruction safety** — `menu_item_destroy`/`menu_item_invalidate`
   (`menu_item.rs:372-506`) already remove nodes and cascade to children. The
   overlay must react to a node disappearing: on the next overlay access,
   drop or disable rows whose node id is gone. The reference dereferences a
   freed `tTJSNI_MenuItem*` here, which is undefined behaviour; the port must
   not, but it also must not invent a script event (there is none).

### 3.3 Files it would touch

| file | change |
|---|---|
| `crates/engine/src/overlay.rs` (new) | `Overlay`/`OverlayLevel`/`OverlayRow`/`OverlayKind`, layout, hit-test |
| `crates/engine/src/lib.rs` | re-export the overlay types |
| `crates/engine/src/scene.rs` | `Scene::overlay` field + `revision` bump on overlay change |
| `crates/engine/src/projection.rs` | append overlay nodes in `project_scene` |
| `crates/compat/src/natives/menu_item.rs` | `build_overlay`, `deliver_click` refactor, real `popup`, overlay activation/callback, per-window `Window.menu` root |
| `crates/compat/src/natives/visual/window.rs` | register the `menu` property |
| `crates/compat/src/event.rs` or `crates/vm/src/host.rs` | overlay input gate before map/dispatch |
| `crates/app/src/game.rs` / `crates/app/src/lib.rs` | load a default overlay font; force a reprojection when the overlay changes |
| `crates/compat/tests/menu_item.rs` | new tests: popup opens/closes, row classification, dismissal rules, no open/close event, `Window.menu` |
| `scripts/native_parity_allow.txt` | not needed for `Window.menu` unless the extractor is taught to read `PropSet` registrations (see E2) |

### 3.4 Where the platform seam goes (no backend built)

The seam is a **presenter** behind the L2 overlay, not a branch in L3. L2
exposes the overlay model and an activation callback; the default presenter is
the engine-drawn form. A future native backend is an L1 `platform` capability
(e.g. `platform::Menu` / `platform::Dialogs`) that `app` wires as the presenter
instead of the engine-drawn one; an Android implementation would open an
Activity-hosted popup and return the activated row id to the same L2 callback.
L3 never learns which presenter is active, because it only calls
`overlay::open(Overlay, activation_callback)` and receives row ids back. The
platform types live in L1 (`platform`), the decision in `app`; neither adds to
`compat`. Building the Android presenter is out of scope.

### 3.5 Ranked increments (smallest visible payoff first)

1. **Open, classify, dismiss, fire (flat).** L2 overlay + projection +
   hit-test; L3 `build_overlay` + `deliver_click` refactor; `popup` opens it;
   input gate. Ship with a plain panel and text rows, no submenu recursion yet
   (submenu rows dismissed like constants or not rendered), and `Window.menu`.
   This makes `MenuItem.popup` do the reference's job end to end for the common
   flat menu. **Visible, testable, and the bulk of the risk is here.**
2. **Submenu nesting + checkbox/separator visuals + exact dismissal matrix.**
   Push/pop levels, reflect `checked` in the row, render separators, and lock
   the group/radio vs checked vs constant dismissal differences with tests.
3. **Keyboard.** Back/Escape pops one level; consume all other keys while open;
   do **not** add arrow navigation (the reference has none).
4. **Fidelity audit of the odd cases.** `visible` ignored, disabled constant
   row still closes, global-string-map separator, `Window.onPopupHide`
   registered but never fired, `Window.menu` parity-tool blind spot.
5. **Deferred/separate.** Android native presenter; `WIN32Dialog` (same shape,
   `TODO.md` §5); Linux native menus (unsupported).

---

## 4. Unsupported / deferred, explicitly

* **Linux native menus and dialogs (XDG Desktop Portal, GTK/toolkit popovers)
  are out of scope and recorded as unsupported.** The XDG Desktop Portal's
  app-facing interface list is Account, Background, Camera, Clipboard,
  Documents, Dynamic Launcher, Email, File Chooser, File Transfer, Game Mode,
  Global Shortcuts, Inhibit, Input Capture, Location, Memory Monitor, Network
  Monitor, Notification, OpenURI, Power Profile Monitor, Print, Proxy Resolver,
  Realtime, Remote Desktop, Request, ScreenCast, Screenshot, Secret, Session,
  Settings, Trash, Usb, Wallpaper — **no menu or popup interface** (verified
  against the published API reference, `flatpak.github.io/xdg-desktop-portal/docs/api-reference.html`).
  The GTK/toolkit popover route is rejected because it wants to own the main
  loop. This does **not** affect fidelity: the reference itself does not use a
  native menu (`reference/cpp/core/visual/impl/MenuItemImpl.cpp:309-345`).
  The port currently has **no portal integration at all**; `Storages.selectFile`
  is a `warn!` + `false` today (`crates/compat/src/natives/storages.rs:537-563`),
  and the file chooser is the separate item that *should* use the portal later.
  Consequence for scripts: none in the reference; a native menu on Linux would
  be an intentional appearance divergence, not a behaviour one.
* **The Win32 `TrackPopupMenuEx` path** is dead in the reference and stays dead
  here. `HMENU`/`GetMenuItemHandleForPlugin` legitimately returns `0`/`nullptr`
  (`MenuItemImpl.cpp:193-196`), so no script can obtain a real menu handle.
* **`popupEx`** is a reference no-op (`windowEx.cpp:1183-1188`); nothing to
  reproduce beyond accepting the call and returning void (already done).
* **Native presenter backends** (Android Activity popup) are deferred. The L2
  seam exists so adding one does not touch L3 (§3.4).
* **`onPopupHide`** is registered but never fired by the reference; the port
  must keep it registered and never fire it (matching, not a gap).
* **Pixel fidelity** of the CSD skin, slide/pop animations and the checkbox
  artwork is approximated under D34. Row set, order, classification, dismissal
  and event behaviour are not.

## 5. Open questions (with best reading from the tree)

1. **Does the popup block game input? Is it modal?** Keyboard: **yes**. The
   topmost UI form consumes every key
   (`reference/cpp/core/environ/cocos2d/MainScene.cpp:804-812` forwards to
   `uiChild.back()` and returns), and `iTVPBaseForm::onKeyPressed` only handles
   Back (`BaseForm.cpp:139-144`). Pointer/touch: the form lives in `UINode`,
   which is ordered above `GameNode` (`MainScene.cpp:30-39`, `:443-456`), so it
   receives the touch first; the exact swallow depends on cocos internals not in
   this tree. **Best reading: effectively modal.** The port should consume
   pointer/keyboard while open.
2. **A second `popup` while one is open?** `TVPShowPopMenu` unconditionally
   pushes another form; forms stack, newest on top; Back pops one, a
   constant/group click closes all. **Best reading: stacking is the reference.**
3. **What if the menu item is destroyed while open?** `tTJSNI_BaseMenuItem::Invalidate`
   (`MenuItemIntf.cpp:75-108`) clears children and cancels events but does not
   touch any open form; `TVPInGameMenuForm`'s lambdas capture a raw
   `tTJSNI_MenuItem*` (`InGameMenuForm.cpp:69-113`). The reference therefore has
   a use-after-free here. **Cannot be settled as intended behaviour from the
   tree; it is UB.** The port must drop/close rows whose node vanished (D11)
   without inventing a script event.
4. **Does clicking a checked item toggle it?** **No.** The form's `_setter`
   ignores the checkbox's new value and only calls `OnClick`
   (`InGameMenuForm.cpp:73-91`); `checked` changes only if the script's handler
   changes it. This is settled.
5. **Is a disabled row visually distinct, and does a disabled row still
   dismiss?** No styling is applied (`createMenuItem` never reads `enabled`).
   Dismissal is outside the `OnClick` gate, so a disabled constant row still
   schedules `popAllUIForm`, and a disabled group/radio row still calls it
   synchronously. This is settled from the code.
6. **Does `visible=false` hide a row?** No — `initMenu` does not check it.
   Settled.
7. **Is there keyboard navigation (arrows/Tab)?** No; only Back. Settled.
8. **Is the popup anchored at `x,y`?** No; `x`/`y` are discarded and the form is
   laid out by `iTVPBaseForm`. Settled.
9. **Global string map for `"-"`.** The separator test uses
   `TJSMapGlobalStringMap(TJS_W("-"))` (`InGameMenuForm.cpp:53`). If a title
   remaps `"-"`, which captions count as separators can change. The port must
   read the same global map to match; **whether the port has that map wired is
   not settled from this file** (`grep` shows `TJSMapGlobalStringMap` elsewhere
   in KAG but the menu path has no port equivalent yet).
10. **Window `CanDeliverEvents` depth.** The reference window gate is
    `_form && visible && form_enabled` (`WindowImpl.cpp:1154-1159`); the port
    checks only `visible` (`menu_item.rs:322-338`). Multi-window/form-enabled
    titles could differ. **Best reading: extend the port gate to the full
    reference condition** when forms exist; currently a minor divergence.

---

## Appendix A — citation index

Reference (oracle):

* `reference/cpp/core/visual/MenuItemIntf.h:24-48` `ObjectVector`; `:50-116`
  base; `:155-163` click input event.
* `reference/cpp/core/visual/MenuItemIntf.cpp:45-125` base lifecycle;
  `:127-194` child links/array; `:196-217` `OnClick`; `:219-228` root;
  `:230-556` registration; `:285-303` `popup`; `:305-330` click methods;
  `:332-553` properties; `:559-575` `TVPCreateMenuItemObject`.
* `reference/cpp/core/visual/impl/MenuItemImpl.h:35-88` class;
  `reference/cpp/core/visual/impl/MenuItemImpl.cpp:24-63` `MENU_LIST`;
  `:65-87` `WindowMenuProperty`; `:95-157` ctor/Construct/CanDeliverEvents;
  `:159-196` tree + handle; `:198-301` properties; `:309-345` `TrackPopup`
  (dead Win32 body); `:348-393` shortcut table (`#if 0` block); `:409-440`
  outer props; `:451-474` dead registration.
* `reference/cpp/core/base/ScriptMgnIntf.cpp:537-542` live `Window.menu` +
  `MenuItem` registration.
* `reference/cpp/core/environ/ui/InGameMenuForm.cpp:14-118` the whole form;
  `reference/cpp/core/environ/ui/InGameMenuForm.h:1-21`.
* `reference/cpp/core/environ/ui/BaseForm.cpp:83-144` layout + Back;
  `reference/cpp/core/environ/ui/PreferenceForm.cpp:57-165` row widgets.
* `reference/cpp/core/environ/cocos2d/MainScene.cpp:30-39` order enum;
  `:443-483` node tree + key listener; `:500-600` push/pop form;
  `:804-812` key gate; `:951-966` `popAllUIForm`.
* `reference/cpp/core/base/EventIntf.cpp:207-245` `TVPPostEvent`;
  `:709-737` event object; `reference/cpp/core/base/EventIntf.h:74,208-237`
  immediate flag + action macros.
* `reference/cpp/core/visual/WindowIntf.cpp:547-554,1211-1220` `onPopupHide`;
  `reference/cpp/core/visual/impl/WindowImpl.cpp:1154-1159` window gate;
  `reference/cpp/core/environ/impl/TVPWindow.cpp:544` dead trigger.
* `reference/cpp/plugins/windowEx.cpp:997-1239` `MenuItemEx`.

Port:

* `crates/compat/src/natives/menu_item.rs:52-116` `MenuNode`/`MenuState`;
  `:291-338` gates; `:508-616` ctor; `:747-777` `popup`; `:780-921` click;
  `:1088-1158` checked; `:1600-1725` registration.
* `crates/compat/src/natives/plugins/window_ex/menu_item.rs` plugin members.
* `crates/compat/src/natives/visual/window.rs` `Window` surface (no `menu`).
* `crates/compat/tests/menu_item.rs` current tests (no popup presentation test).
* `crates/engine/src/text.rs:931,1073,1186,1634,1966` glyph atlas, rasterize,
  `project_text`, `compose_text_into`.
* `crates/engine/src/projection.rs:526-572` `project_window`/`project_scene`.
* `crates/engine/src/scene.rs:527-533,1565-1620,2285+` revision, hit-test,
  order.
* `crates/app/src/lib.rs:373-428` render + reprojection.
* `crates/compat/src/event.rs:241-345,462-537` dispatch + hit-test.
* `crates/vm/src/host.rs:157-172` per-frame event map/dispatch.
* `crates/app/src/game.rs:270-282` frame step.
* `crates/compat/src/resource.rs:36-53,366-490` L3 text engine + bitmap text.
* `docs/surface/runtime_surface.json` `MenuItem`/`Window` member lists.
* `docs/audits/stub-inventory.md:35,62` `MenuItem.popup` classified as the
  platform-dialog item; `TODO.md` §5 at `:115-141`.
