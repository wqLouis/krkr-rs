# `compat::natives::menu_item` — the `MenuItem` class

Source: `crates/compat/src/natives/menu_item.rs` · layer **L3** · class
`MenuItem`

`MenuItem` is a node in a logical menu tree. The tree is real — a script can
build it, walk it, link items to an action owner and fire clicks — but there
is **no OS menu behind it**: `popup` returns the reference's success value
without opening anything, and `HMENU` is always `0`.

---

## 1. Registered surface

**Instance methods (7):** the constructor `MenuItem`, `add`, `insert`,
`remove`, `popup`, `onClick`, `fireClick`.

**Instance properties (13):**

* read/write (7): `caption`, `checked`, `enabled`, `radio`, `group`,
  `visible`, `shortcut`;
* read-only (6): `index`, `parent`, `root`, `children`, `window`, `HMENU`.

**Static properties (2), read-only:** `MenuItem.textToKeycode` and
`MenuItem.keycodeToText`.

Fifteen further members are attached to the same class object by the
**windowEx plugin**: `popupEx` (a method) and the properties `biClose`,
`biCloseDisabled`, `biMinimize`, `biMinimizeDisabled`, `biPopupClose`,
`biPopupMaximize`, `biPopupMinimize`, `biPopupRestore`, `biRestore`,
`biSystem`, `bmpChecked`, `bmpItem`, `bmpUnchecked` and `rightJustify`.

---

## 2. The tree

`new MenuItem([actionOwner[, windowOrCaption]])`.

* The first argument is the **action owner**: the object whose `action`
  member receives the click event. It may be `null`; omitting it is
  tolerated.
* The second argument is either a `Window` object — which becomes the
  item's `window` — or a string, which becomes the initial `caption`. A
  non-`Window` object raises `MenuItem: the second argument is not a Window`.

A node is created at **create** time, so a script subclass that writes
members before calling the base constructor keeps those writes.

`add(item)` appends a child, moving it from a previous parent. Adding a child
that is already a child of this parent is a no-op that keeps its position.
`insert(item, index)` inserts at an index (clamped to the child-list length);
a duplicate child is likewise a no-op. `remove(item)` detaches a child;
removing a non-child is a no-op.

The tree is authoritative: inserting a node under one of its own
descendants raises `MenuItem: inserting here would create a cycle`.

---

## 3. `fireClick` and `onClick`

`fireClick()` is the activation path. It checks, in order:

1. the enabled chain — a disabled item or a disabled **ancestor** blocks the
   click;
2. the window chain — the item's window must be in the ancestor chain and
   **visible**;
3. then it calls the object's `onClick` member, so a **script override wins**
   over the native default.

The native default `onClick` builds a `%[]` event with `type = "onClick"` and
`target = objthis` and calls `owner.action(event)`. A missing or failing
`action` is ignored.

After the click the reference's `TVPDoSaveSystemVariables` hack runs: the
script global `kag.saveSystemVariables()` is called when it exists. Every
failure there is swallowed.

When `System.eventDisabled` is set the event is dropped, but the save hack
still runs.

`popup(flags, x, y)` performs the reference's `AsInteger` coercion on all
three arguments and then returns **1** — the reference's `TrackPopup` success
value. No menu opens.

---

## 4. The properties

| member | behaviour |
|---|---|
| `caption` | the item's text |
| `checked` | a bool; writing true on a `radio` item with a parent clears the other siblings of the same `group` under that parent |
| `enabled` | a bool, default true |
| `radio` | a bool, default false |
| `group` | an integer, default 0 |
| `visible` | a bool, default true |
| `shortcut` | a string, default `""` |
| `index` | the position in the parent's child list (`0` for a root). The **setter is a no-op** — the reference's `SetIndex` has an empty body — but the argument still goes through the strict integer conversion |
| `parent` | the parent object, or `null` |
| `root` | the topmost ancestor (or itself) |
| `children` | a **cached** TJS array: the same array object is handed back and refreshed in place |
| `window` | the `Window` object passed to the constructor, or `null` |
| `HMENU` | always `0` — the reference's `GetMenuItemHandleForPlugin` returns `nullptr` and the getter casts it to an integer |

### The shortcut tables

`MenuItem.textToKeycode` is a dictionary with exactly three entries:
`'bksp'→8`, `'pgup'→33`, `'pgdn'→34`. Every other key is absent and reads
back as void.

`MenuItem.keycodeToText` is a sparse array indexed by virtual-key code with
`a[8]="BkSp"`, `a[33]="PgUp"`, `a[34]="PgDn"`; its `count` is 35.

Both are **read-only**: a class-qualified write raises.

---

## 5. State ownership

| state | owner |
|---|---|
| the node map, the next id, the instance map, the cached static tables | the **per-engine dispatch state** (`CompatServices::state::<MenuState>()`) |
| the node id and the constructed flag | the **per-object payload** (`MenuItemInst`) |
| the caption, the flags, the parent and children, the retained child script objects, the retained action owner, the retained window, the raw owner handle, the cached children array | the **per-node record** in the per-engine state |

The tree itself is the authoritative state — there is no OS menu object
behind it.

The VM's `invalidate` hook cascades invalidation to the children, clears the
parent, window, children array and action owner, and drops the retentions
**outside** the state borrow so a re-entrant call cannot panic.

There is **no L1 and no L2 seam**: `MenuItem` is pure logical state. The only
external dependency is the `Window` object (another L3 native) whose
`visible` member decides delivery.

---

## 6. Errors

| shape | example |
|---|---|
| `"MenuItem: compat context is not bound"` | the engine binding is missing |
| `"MenuItem: this item is already constructed"` | a second constructor call |
| `"MenuItem: this item is not constructed"` | a member on an unconstructed object |
| `"MenuItem: cannot retain the action owner: {error}"` | the owner could not be retained |
| `"MenuItem: the action owner is not an object"` | a non-object first argument |
| `"MenuItem: cannot retain the window: {error}"` | the window could not be retained |
| `"MenuItem: the second argument is not a Window"` | a non-`Window` second argument |
| `"MenuItem: window lookup: {error}"` | the window payload lookup failed |
| `"MenuItem.add requires a MenuItem"` / `"MenuItem.add expects a MenuItem"` | the argument is missing / not an item |
| `"MenuItem.insert requires an item and an index"` | the arguments are missing |
| `"MenuItem: the item no longer exists"` | the child record is gone |
| `"MenuItem: the parent no longer exists"` | the parent record is gone |
| `"MenuItem: inserting here would create a cycle"` | a cycle would be created |
| `"MenuItem.popup requires flags, x and y"` | the arguments are missing |
| `"MenuItem.children: could not create the array"` / `": the array is missing"` / `": {error}"` / `": clear failed: {error}"` / `": push failed: {error}"` | the children array could not be built or refreshed |

The strict integer conversion errors are reported unprefixed: `value is null,
not an integer`, `value is an object, not an integer`, `value is an octet,
not an integer`, `value cannot be converted to an integer`.

Diagnostics that are **logged rather than raised**: a retained object that
could not be duplicated, an `onClick` handler that failed, an action-owner
dispatch that failed, an unreadable `window.visible`, a failing
`kag.saveSystemVariables`, and a child invalidation that failed.

---

## 7. Tests

`crates/compat/tests/menu_item.rs` covers the tree links and the default
flags, the radio-group side effect, `insert`/`remove` and the `index`
no-op, the cached children array, `fireClick` honouring the enabled state and
a script `onClick`, the disabled-ancestor block, the visible-window
requirement, the action owner receiving the `onClick` event, the `window`
property and the non-`Window` rejection, the shortcut tables and their
read-only-ness, `HMENU` being zero and `popup` reporting success (including
the argument coercion), the subclass members written before the base
constructor, and the node leak check on dropped items.

`crates/compat/tests/dispatch_ui.rs` pins the receiver routing for the
instance and static members. `crates/compat/tests/menu_dll_surface.rs`
compares the class against the recovered DLL surface fixture.

---

## 8. Known gaps

1. **`HMENU` is `0`** for every item. No plugin can address a real menu
   handle.
2. **`popup` opens no menu.** It performs the reference's argument coercions
   and returns the reference's success value.
3. **The `index` setter does not reorder** — matching the reference's empty
   `SetIndex` body.
4. **There is no OS menu, no accelerators and no `Window.menu` realization.**
   The tree is logical only.
5. **`menu.dll` is marked as a built-in** but its `register` is a no-op: the
   class is registered by the core module, not by the plugin.

---

## 9. Related documents

| topic | document |
|---|---|
| the L3 layer and the plugin model | [`../../arch/l3.md`](../../arch/l3.md) |
| the Win32 emulation rule | [`../../rules/arch_rules.md`](../../rules/arch_rules.md) §7 |
| the surface lock | [`../../arch/l3.md`](../../arch/l3.md) §7 |
