# `compat::natives::rect` — the `Rect` class

Source: `crates/compat/src/natives/rect.rs` · layer **L3** · class `Rect`

`Rect` is the script's rectangle: four integer edges with the reference's
geometry operations. It is a **pure value object** — it composes no engine
or scene state and touches no L1 or L2 service.

---

## 1. Registered surface

**Methods (13, the constructor included):** `Rect` (the constructor),
`isEmpty`, `setSize`, `setOffset`, `addOffset`, `clear`, `set`, `clip`,
`union`, `intersects`, `included`, `includedPos`, `equal`.

**Properties (7):** `width`, `height`, `left`, `top`, `right`, `bottom` (all
read/write) and `nativeArray` (read-only).

---

## 2. Construction

The constructor accepts **zero**, **one** or **four** arguments; any other
count is **ignored** and the rectangle is left zeroed.

| call | result |
|---|---|
| `new Rect()` | all edges zero |
| `new Rect(src)` | a copy of another `Rect` |
| `new Rect(left, top, right, bottom)` | the four edges |

The copy constructor raises `Rect: argument is not a Rect object` for a
`null` or a non-`Rect` argument.

---

## 3. The operations

| member | behaviour |
|---|---|
| `isEmpty()` | `left >= right \|\| top >= bottom` |
| `setSize(w, h)` | keeps `(left, top)` fixed; `right = left + w`, `bottom = top + h` |
| `setOffset(x, y)` | moves the rectangle to `(x, y)`, preserving the size |
| `addOffset(x, y)` | translates by `(x, y)` |
| `clear()` | resets all four edges to zero |
| `set(l, t, r, b)` | assigns all four edges |
| `clip(src)` | intersects in place; returns whether the intersection is non-empty. On an empty intersection the receiver is **left unchanged** |
| `union(src)` | unions in place; returns whether the union is non-empty. On an empty union (both empty) the receiver is **left unchanged** |
| `intersects(src)` | the overlap test; false if either rectangle is empty |
| `included(src)` | whether `src` fully contains this rectangle; false if either is empty |
| `includedPos(x, y)` | half-open point containment: `left <= x < right && top <= y < bottom` |
| `equal(src)` | whether all four edges are equal |

The `width` setter keeps `left` fixed (`right = left + width`); the `height`
setter keeps `top` fixed.

The arithmetic wraps on overflow, matching the reference's 32-bit `tjs_int`.

---

## 4. The binary-method argument contract

This is the part a script can trip over:

| argument | result |
|---|---|
| `null` | a **silent void** result; the receiver is untouched |
| a non-object (a number, a string) | **raises** — the reference's object conversion throw |
| a plain script object, even one with `left`/`top`/`right`/`bottom` | **raises** `Rect: argument is not a Rect object` — the reference's `TJS_E_INVALIDPARAM` |
| a native `Rect` | the operation runs |

`null` is skipped because the reference guards with `if(clo.Object)`.

---

## 5. `nativeArray`

`nativeArray` returns the **raw address** of the rectangle's first edge as an
integer — the reference's `tTVPRect::array` pointer. It is read-only; a
write raises.

This is an opaque address, never a handle: nothing reads it back through a
sign test, and the address is never used to key state.

---

## 6. State ownership

The four edges are the **per-object payload** (`RectData`, a `#[repr(C)]`
struct of four `i32`s), allocated by the create callback and freed by the
destroy callback.

There is no per-engine dispatch state, no L2 state and no L1 seam. The only
thing the class uses from the VM is the native-instance lookup that resolves
another `Rect` object.

---

## 7. Errors

| shape | example |
|---|---|
| `"Rect: compat context is not bound"` | the engine binding is missing |
| `"Rect: argument is not a Rect object"` | a null or non-`Rect` argument |
| `"Rect: argument cannot be retained: {error}"` | the argument could not be retained |
| `"Rect: argument lookup failed: {error}"` | the native-instance lookup failed |
| `"Rect: edge <i>: {error}"` | a non-numeric edge: `Rect: edge 2: value is an object, not an integer` |
| `"Rect: method requires 1 argument"` | a binary method called with no argument |
| `"Rect.setSize: 2 arguments required"` | the arity check |
| `"Rect.setOffset: 2 arguments required"` | the same |
| `"Rect.addOffset: 2 arguments required"` | the same |
| `"Rect.set: 4 arguments required"` | the same |
| `"Rect.includedPos: 2 arguments required"` | the same |

The property setters and the numeric method arguments use the same
converter, so the error is prefixed with the member or property name:
`Rect.width: value is an object, not an integer`.

---

## 8. Tests

`crates/compat/tests/rect.rs` covers the default constructor, the four-argument
constructor, the copy constructor and `equal`, `setSize` / `setOffset` /
`addOffset`, `set` / `clear` and the property writes, `intersects` / `clip` /
`union`, `included` and the half-open `includedPos`, the rule that a
non-object raises while `null` is a void no-op, the ignored argument counts,
the typed errors, and `nativeArray` being a read-only pointer value.

Unit tests inside the module check the geometry and the relation operators
against the reference's `tTVPRect`.

---

## 9. Known gaps

1. **`nativeArray` leaks a raw address** into script. It matches the
   reference, but the value is meaningless outside the process.
2. **`clip` and `union` suppress the mutation on an empty result** — a
   rule a caller has to know: the rectangle is left unchanged rather than
   being set to an empty rectangle.
3. **`width` and `height` wrap on overflow** rather than saturating.

---

## 10. Related documents

| topic | document |
|---|---|
| the L3 layer and the surface rules | [`../../arch/l3.md`](../../arch/l3.md), [`../../rules/api_rules.md`](../../rules/api_rules.md) §2 |
| the pointer/handle rule | [`../../rules/arch_rules.md`](../../rules/arch_rules.md) |
