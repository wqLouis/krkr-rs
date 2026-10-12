# Audit: layer focus events on `invalidate`, and the number of focus managers per window

Read-only audit of `reference/cpp/**` against the committed focus-event
implementation (`crates/compat/src/natives/visual/layer/focus.rs`,
`crates/compat/src/natives/visual/layer/mod.rs` `layer_invalidate`,
`crates/engine/src/scene.rs`, `crates/compat/tests/focus_behaviour.rs`).
The reference is the only oracle. Both flagged questions are answered
definitively; the second carries one explicitly-labelled residual doubt.

Key reference facts used throughout (all line numbers from the vendored tree):

- `tTJSNI_BaseLayer::Invalidate()` (`reference/cpp/core/visual/LayerIntf.cpp:515`)
  runs, in order: `Shutdown = true` (`:516`); transition/event cancellation;
  for a **primary** layer only, `Manager->DetachPrimary()` (`:533`) and
  `Manager->UnregisterSelfFromWindow()` (`:535`); then
  `Manager->Release(); Manager = nullptr;` (`:538-541`); then `Part()`
  (`:544`); then `child->Part()` for each direct child (`:546-548`).
- `Part()` (`:624-631`): `Update();` then `if(Manager) Manager->NotifyPart(this);`
  then `Parent->SeverChild(this)`. `SeverChild` (`:657-665`) calls
  `if(Manager) Manager->BlurTree(child)` on the **parent's** manager.
- `tTVPLayerManager::NotifyPart` → `BlurTree(lay)`
  (`reference/cpp/core/visual/LayerManager.cpp:197-202`).
- `BlurTree(root)` (`LayerManager.cpp:690-711`): early-out unless the current
  `FocusedLayer` is root-or-below (`FocusedLayer->IsAncestorOrSelf(root)`,
  `:698`; `IsAncestorOrSelf` at `LayerIntf.h:250-253` reads "root is
  ancestor-or-self of FocusedLayer"); then `next = root->GetNextFocusable()`
  (`:702`) and `SetFocusTo(next, true)` or `SetFocusTo(nullptr, true)`
  (`:704-708`).
- `SetFocusTo` (`LayerManager.cpp:760-817`): after moving `FocusedLayer`,
  `if(org && !org->Shutdown) org->FireBlur(layer);`
  (`:786-787`) and `if(FocusedLayer && !FocusedLayer->Shutdown)
  FocusedLayer->FireFocus(org, direction);` (`:789-790`).
  `FireBlur`/`FireFocus` re-guard on `Owner && !Shutdown`
  (`LayerIntf.cpp:3702-3713`, `:3715-3727`); both post with
  `TVP_EPT_IMMEDIATE` (synchronous).
- **`GetNextFocusable` walks the manager's node list through
  `GetNeighborBelow`, which returns `nullptr` when the receiver's `Manager`
  is null** (`LayerIntf.cpp:3631-3649` via `:1260-1263` —
  `if(!Manager) return nullptr;`). This is the pivot of question 1: after
  `Invalidate` nulls `Manager` (`:538-541`), any `BlurTree(root=this)`
  triggered by the subsequent `Part()` can only compute `next == nullptr`.
- `DetachPrimary` (`LayerManager.cpp:175-185`): `SetFocusTo(nullptr)` first
  (`:178`), then `NotifyPart(Primary)` (`:182`), so the primary path fires
  the `SetFocusTo` blur guards before any `BlurTree` runs.

---

## Question 1 — does `invalidate` fire `onFocus`/`onBlur` at all?

**Verdict: essentially no. `invalidate` (the single teardown path — see
(d)) fires *no* focus events in three of the four cases, and exactly one
`onBlur(layer, null)` in the fourth. It never hops focus to another layer,
and `onFocus` never fires from it.** The two guards the implementing agent
found are decisive, and the released `Manager` additionally destroys the
`GetNextFocusable` search, so even the surviving `BlurTree` calls can only
clear focus, never redirect it.

### (a) Invalidating the focused (non-primary) layer `a` — **no events**

Call chain:

1. `Invalidate()` sets `Shutdown = true` on `a` (`LayerIntf.cpp:516`).
2. Not primary → `DetachPrimary` skipped (`:531-537`).
3. `a->Manager` released and nulled (`:538-541`).
4. `a->Part()` (`:544` → `:624-631`): `NotifyPart` is skipped (`a->Manager`
   is null); `Parent->SeverChild(a)` (`LayerIntf.cpp:657-665`) runs on the
   **parent's** live manager → `BlurTree(a)` (`LayerManager.cpp:197-202` →
   `:690-711`).
   - `FocusedLayer == a`; `a->IsAncestorOrSelf(a)` is true → proceeds.
   - `next = a->GetNextFocusable()` — **`a->Manager` is null**, so
     `GetNeighborBelow` returns null (`LayerIntf.cpp:1262-1263`) and
     `next == nullptr` (`:3631-3649`).
   - `SetFocusTo(nullptr, true)` (`LayerManager.cpp:704-708`): `org == a`,
     `org->Shutdown` is true → `FireBlur` **skipped** (`:786-787`); new
     `FocusedLayer` is null → no `FireFocus` (`:789-790`).
5. The children loop (`:546-548`): each child's `Part()` reaches a live
   manager, but `FocusedLayer` is already null, so `BlurTree` early-returns
   (`LayerManager.cpp:695-696`).

Result: **no `onBlur`, no `onFocus`; focus ends null** (and
`w.focusedLayer` — manager index 0 — reads null).

### (b) Invalidating a non-focused, non-primary ancestor `X` of focused `F` — **one `onBlur(F, null)`**

Call chain:

1. `Shutdown = true` on `X` only (`LayerIntf.cpp:516`); `F` stays alive.
2. `X->Manager` nulled (`:538-541`).
3. `X->Part()` → `Parent->SeverChild(X)` → `BlurTree(X)` on the grandparent's
   live manager: `F->IsAncestorOrSelf(X)` is true (X is F's ancestor);
   `next = X->GetNextFocusable()` is **null** (X's manager nulled, same
   `GetNeighborBelow` short-circuit); → `SetFocusTo(nullptr, true)`: `F` is
   not shutdown, so **`F.onBlur(null)` fires** (`LayerManager.cpp:786-787`,
   args at `LayerIntf.cpp:3702-3713`); no `onFocus` (target null).
4. The children loop parts each child `C`; `C`'s own manager is still live,
   so `NotifyPart(C) → BlurTree(C)` runs — but `FocusedLayer` is already
   null (`:695-696` early-out). No second event.

Result: **exactly one event — `F.onBlur(null)`, synchronous, argument
`null`; focus ends null.** Notably the reference does *not* move focus to
the next focusable outside the destroyed subtree, because the search
origin's `Manager` was already released. (Edge case, not verified on a
live engine: if a **modal** layer sits inside `X`'s subtree,
`RemoveTreeModalState` (`LayerManager.cpp:899-925`) also calls
`SetFocusTo(root->GetNextFocusable(), …)` (`:913`) — with the manager
nulled that is `SetFocusTo(nullptr)`, i.e. the same null-blur, not a hop.)

### (c) Invalidating a primary (parentless) layer `P` — **no events if `P` itself is focused; one `onBlur(F, null)` if focus is on a live descendant**

Call chain: `Invalidate()` → `Shutdown = true` (`:516`) →
`Manager->DetachPrimary()` (`:533`; `LayerManager.cpp:175-185`) →
`SetFocusTo(nullptr)` (`:178`):

- If `FocusedLayer == P`: `P->Shutdown` is true → `FireBlur` skipped; no
  target → no `FireFocus`. **No events.**
- If `FocusedLayer == F` (live descendant): `F->Shutdown` is false →
  **`F.onBlur(null)` fires**; no `onFocus`.

Then `NotifyPart(Primary)` → `BlurTree(P)` (`:182`) early-returns
(`FocusedLayer` already null). `UnregisterSelfFromWindow` (`:535`), manager
release (`:538-541`), `Part()`/children loop — all inert for focus. So:
**events are `nothing` (P focused) or `F.onBlur(null)` (F focused); focus
ends null either way.**

### (d) Destroying/finalizing a layer vs `invalidate` — **nothing differs**

`invalidate scriptObj`, scope-exit/GC finalization, and native teardown all
run the same function: `tTJSCustomObject::Finalize` calls
`ClassInstances[i]->Invalidate()` (`reference/cpp/core/tjs2/tjsObject.cpp:390-400`)
→ `tTJSNI_Layer::Invalidate()` (`reference/cpp/core/visual/impl/LayerImpl.cpp:51`)
→ `tTJSNI_BaseLayer::Invalidate()` (`LayerIntf.cpp:515`). The C++
destructor does no focus work (`LayerIntf.cpp:413`). There is no second
teardown path with different focus semantics.

### Trigger table (reference)

| Action | Path (citations) | Events fired | Focus after |
|---|---|---|---|
| `invalidate a`, `a` focused, non-primary | `Invalidate` `:515-544` → `Part` `:624` → `SeverChild` `:657` → `BlurTree` `LayerManager.cpp:690` → `SetFocusTo(nullptr)` `:708` | **none** (blur suppressed by `a.Shutdown`, `:786-787`; `next` null because `a.Manager` was nulled, `LayerIntf.cpp:1262`) | null |
| `invalidate X`, `X` ancestor of focused `F` | same chain, `BlurTree(X)` sees `F` below `X` | **`F.onBlur(null)`** only | null |
| `invalidate P` (primary), `P` focused | `DetachPrimary` `LayerManager.cpp:175` → `SetFocusTo(nullptr)` `:178` | **none** (`P.Shutdown`) | null |
| `invalidate P` (primary), focus on descendant `F` | same | **`F.onBlur(null)`** | null |
| GC finalize / destruction of any of the above | same `Invalidate()` (`tjsObject.cpp:390-400`) | as above | as above |
| `a.visible = false` / `a.enabled = false` / `a.focusable = false` (manager **alive**) | `LayerIntf.cpp:1183-1206` / `:3801-3820` / `:3683-3697` → `BlurTree` with live manager → real `GetNextFocusable` | **`onBlur(F, next)` + `onFocus(next, F, true)`** (or `onBlur(F, null)` if no next) | next / null |
| re-parent within one manager (`a.parent = q`) | `Join` `:611-620` → `Part` → `NotifyPart` → `BlurTree` (manager alive) | **both events, real next hop** | next / null |
| re-parent across managers | `Join` throws `TVPCannotMoveToUnderOtherPrimaryLayer` **before** `Part` (`:614-615`) | **none** — error raised, tree unchanged | unchanged |

### What the port does today, and what changes

`layer_invalidate` (`crates/compat/src/natives/visual/layer/mod.rs:151-172`)
calls `blur_tree(&ctx, id)` **while the layer is still alive and before the
shutdown flag is set**, then `Scene::invalidate_layer`
(`crates/engine/src/scene.rs:1131-1143`). `blur_tree`
(`crates/compat/src/natives/visual/layer/focus.rs`) computes a real
`Scene::next_focusable(root)` and calls the full `set_focus_to`, which has
**no shutdown guard anywhere**. Consequences versus the reference:

- `removing_the_focused_layer_blurs_to_the_next_focusable`
  (`crates/compat/tests/focus_behaviour.rs`) asserts `a.blur:b;b.focus:a:1;`
  and `w.focusedLayer === b` for `invalidate a`. The reference fires
  **nothing** and leaves focus **null**. The port is wrong on both the
  events and the resulting focus.
- `invalidate X` (ancestor of focused `F`, another focusable outside `X`'s
  subtree): the port fires `F.onBlur(next)` + `onFocus(next, F)`; the
  reference fires only `F.onBlur(null)` and clears focus.
- `invalidate P` (primary, focus on descendant): port hops to next;
  reference fires only `F.onBlur(null)`.

**Yes, `Shutdown` has a port counterpart** — `LayerState::shutdown`
(`crates/engine/src/scene.rs:218`, set in `Scene::invalidate_layer`
`:1138`, already consulted by `node_visible`/`node_enabled` and
`window_layer_order`). It exists but is unused by the focus helpers and set
at the wrong point: the reference flips it **first** (`LayerIntf.cpp:516`),
before any detach/blur, and that ordering is what suppresses the events.

---

## Question 2 — how many focus managers does a window's layer tree have?

**Verdict: in the reference, exactly one `tTVPLayerManager` per *parentless*
(root) layer — not one per window. A layer inherits its parent's manager at
construction and keeps it for life; a window accumulates every root's
manager in a vector and treats index 0 as *the* interactive manager.** The
port's single per-window `WindowState::focused_layer`
(`crates/engine/src/scene.rs:37`) merges all roots into one focus domain —
a divergence from the vendored oracle.

### Evidence from the vendored code

1. **Assignment** — `Construct` (`LayerIntf.cpp:416-477`): a layer with a
   parent takes the parent's manager (`:449-455`); a **parentless** layer
   gets `Manager = new tTVPLayerManager(lto); Manager->AttachPrimary(this);
   Manager->RegisterSelfToWindow();` (`:461-473`; the alternate
   `ConstructResolvedTreeOwnerLike_0x800438` `:482-505` is identical in
   shape). `Manager =` appears nowhere else (`:315, 453, 465, 492, 499,
   540` — the last is the teardown null), so the manager is fixed for life.
2. **Cross-root moves are impossible** — `Join` throws
   `TVPCannotMoveToUnderOtherPrimaryLayer` when `parent->Manager != Manager`
   (`:614-615`). The error's very name presumes several "primary layers"
   (manager domains) inside one window.
3. **The window registry is a list** — `RegisterLayerManager` pushes into a
   `Managers` vector and AddRefs (`LayerTreeOwnerImpl.cpp:93-98`);
   `PrimaryLayerManagerIndex` (`LayerTreeOwnerImpl.h:21`, init 0 at
   `LayerTreeOwnerImpl.cpp:20`, never reassigned) selects manager 0 for
   everything window-level: `primaryLayer` (`WindowIntf.cpp:1789-1806` via
   `LayerTreeOwnerImpl.cpp:373-378`), `focusedLayer` get/set
   (`WindowIntf.cpp:1756`, `:1781`), and **all input** — key press/char,
   mouse, wheel, touch (`LayerTreeOwnerImpl.cpp:344-359` and the
   `FireMouse*`/`FireTouch*` family; the draw-device paths do the same,
   `impl/DrawDevice.cpp:273-312`). Painting, by contrast, iterates every
   manager (`LayerTreeOwnerImpl.cpp:175`).
4. **Teardown** — window invalidation invalidates every registered object
   (`WindowIntf.cpp:211-222`); each root's `Invalidate` then runs its own
   `DetachPrimary` + `UnregisterSelfFromWindow` (`LayerIntf.cpp:531-537`).
   The window holds no direct manager pointer any more (the
   `DetachPrimary` call in `tTJSNI_BaseWindow::Invalidate` is commented
   out, `WindowIntf.cpp:187`).

Consequences for sibling parentless layers `a`, `b` on one window in the
reference: `a.focus()` and `b.focus()` move focus in **separate** managers;
`b.focus()` after `a.focus()` fires only `b.onFocus(null, true)` (its
manager had no focus) — **no `a.onBlur`**; `w.focusedLayer` keeps reporting
`a` (manager 0); a click never reaches `b` (input is routed to manager 0
only); `focusNext()` from `a` cannot see `b`.

### Is the vendored Construct real, or a reconstruction contradicting the engine (trap 9)?

The reconstruction is **internally coherent across at least six independent
subsystems** — constructor, the `Join` guard, the tree-owner registry, the
input pumps, the window property layer, and the draw-device manager list —
plus a purpose-built error message. A partial reconstruction that misreads
one constructor rarely fabricates a matching multi-manager architecture in
all of them, and nothing in the repository's shipped data contradicts it.
I therefore treat one-manager-per-root as the reference behaviour, not as a
reconstruction artifact. (What I could *not* do in this audit: run a real
engine build to observe it; see the closing section.)

### What this implies for the port, and which tests are wrong

The port model — every `parent == None` layer attaches to the window, one
`focused_layer` per window, `window_layer_order` flattening all roots into
one wrap-around focus chain (`crates/engine/src/scene.rs:2159+`,
`focusable_neighbor` `:1410-1436`) — coincides with the reference **only
while a window has a single parentless layer**. With two or more roots:

Wrong tests in `crates/compat/tests/focus_behaviour.rs` (all create
parentless siblings `a`/`b`):

- `moving_focus_fires_blur_on_the_outgoing_then_focus_on_the_incoming` —
  reference: `b.focus()` fires `b.onFocus(null, 1)` only; no `a.blur`,
  `a.focused` stays 1, `w.focusedLayer` stays `a`.
- `focus_directions_reach_the_event_as_reference_booleans` —
  `a.focusNext()`/`b.focusPrev()` cannot cross managers; reference returns
  null and fires nothing.
- `hiding_the_focused_layer_blurs_to_the_next_focusable`,
  `disabling_the_focused_layer_blurs_to_the_next_focusable`,
  `unfocusing_the_focused_layer_blurs_to_the_next_focusable` — mechanism
  (both events, real hop) is right *within a manager*, but with `a`/`b` as
  separate roots the hop is impossible: reference gives `a.onBlur(null)`
  and null focus.
- `removing_the_focused_layer_blurs_to_the_next_focusable` — wrong for both
  reasons (question 1 and question 2): reference fires nothing, focus null.
- `reparenting_the_focused_layer_blurs_it_out_of_the_tree` — moving `a`
  under a parentless `q` crosses managers: the reference **throws**
  `TVPCannotMoveToUnderOtherPrimaryLayer` before any blur; the port
  blurs and moves silently.
- `a_mouse_click_never_moves_layer_focus` — its conclusion holds, but for a
  different reason in the reference: with `a`/`b` as separate roots the
  click is routed to `a`'s tree only and **`bClicks` would stay 0**.

Still valid (single-domain behaviour): `focus_from_nothing_fires_only_on_focus_with_null_and_direction`,
`focus_on_a_non_focusable_layer_fires_nothing_and_moves_nothing`,
`hiding_an_ancestor_blurs_the_focused_descendant` (its assertions happen to
hold under both models), `enabling_and_showing_layers_never_auto_focuses`.

Blast radius beyond the focus file: 15 test files use
`new Layer(w, null)` (`crates/compat/tests/`), mostly once per window
(the coinciding case), but any file creating a second parentless layer and
then relying on input routing, `w.focusedLayer`, or z/hit order across
roots would need re-checking under the reference model.

**Divergence size:** real engines' script layer (KAG and typical games)
creates one parentless layer per window and parents everything else under
it, so the two models are observationally identical for the overwhelming
majority of titles — the port's model is a *latent* divergence, exercised
only by games that create two or more parentless layers on one window.
Because it changes script-observable semantics (`onBlur`/`onFocus`,
`w.focusedLayer`, which layer receives clicks/keys, and a raised error
name), it is not covered by the D34 efficiency allowance; the
rule-compliant destination is per-root focus domains in L2 with the window
pumping only domain 0.

---

## Changes needed, smallest first

1. **`layer_invalidate` ordering** (`crates/compat/src/natives/visual/layer/mod.rs:151-172`):
   set the layer's `shutdown` flag *before* the blur (add a small `Scene`
   helper that flips the flag without removing the record yet), mirroring
   `LayerIntf.cpp:516`. Keep the record alive through the blur so the event
   arguments still resolve.
2. **Shutdown guards in `set_focus_to`**
   (`crates/compat/src/natives/visual/layer/focus.rs`): skip the outgoing
   layer's `onBlur` when it is `shutdown` (`LayerManager.cpp:786-787`, and
   the `FireBlur` re-guard `LayerIntf.cpp:3703`); skip the incoming
   `onFocus` when it is `shutdown` (`LayerManager.cpp:789-790`). This is
   the port counterpart of the two guards the implementing agent flagged.
3. **Manager-release mirror in `blur_tree`**: when `root` is `shutdown`,
   the reference's `GetNextFocusable` search is dead (`Manager` nulled,
   `LayerIntf.cpp:1262-1263`), so `blur_tree` must force `next = None` and
   go straight to the clear-focus call instead of searching. With 1–3:
   invalidate-focused → no events + null focus; invalidate-live-ancestor →
   `onBlur(F, null)` only; invalidate-primary-with-focused-descendant →
   `onBlur(F, null)`.
4. **Test corrections** (`crates/compat/tests/focus_behaviour.rs`):
   rewrite `removing_the_focused_layer_blurs_to_the_next_focusable` to
   assert an empty event log and `w.focusedLayer === null`; add explicit
   ancestor-invalidate and primary-invalidate cases with the citations from
   the trigger table.
5. **Decide and implement the manager-domain question** (large, L2+L3):
   either introduce per-root focus domains (manager id on `LayerState`,
   scoped `focused_layer`/`next_focusable`/`focusNext`, window
   `focusedLayer`/`primaryLayer`/input bound to domain 0, `Join`
   cross-domain → `TVPCannotMoveToUnderOtherPrimaryLayer`), or record a
   deliberate, reviewed divergence decision — the latter conflicts with the
   strict script-observable-semantics rule, so the per-domain model is the
   compliant end state. Either way the focus test fixtures (and any other
   test with ≥2 parentless layers per window) must be re-based.

## What could not be determined / what would settle it

- **Live confirmation of question 2.** The vendored code is unambiguous and
  self-consistent, but this audit could not execute a real engine. A
  five-line script on a real engine (or on Kirikiroid2, whose APK sits next
  to the test game): create `a`, `b` with `new Layer(w, null)`, log
  `a.onBlur`/`b.onFocus`, call `a.focus(); b.focus();`, read
  `w.focusedLayer` — one run settles it. Binary disassembly of
  `RegisterLayerManager` in a shipped engine library would also confirm the
  vector registry.
- **Modal-layer interplay on invalidate** (`RemoveTreeModalState`,
  `LayerManager.cpp:899-925`): traced statically to produce the same
  null-blur, not verified on a live engine.
- **Re-entrancy during invalidate**: if a script `onBlur` handler runs
  during the teardown `SetFocusTo`, the reference's `FocusChangeLock`
  (`LayerManager.cpp:781-782`) throws `TVPCannotChangeFocusInProcessingFocus`
  on a nested focus change; the port has no lock. Low priority (handlers
  that re-focus during `invalidate` are pathological), but it is an
  unmodelled script-observable.
