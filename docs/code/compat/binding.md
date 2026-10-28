# `binding.rs` — the one process-wide engine → context table

**Source:** `crates/compat/src/binding.rs`

The C++ native ABI hands a callback only the raw `tjs2_engine*`. The `tjs`
crate exposes **no user-data slot**, so there is nowhere to hang a context
pointer. `compat` keeps exactly **one** process-wide table that maps that raw
pointer back to the Rust engine, the L2 services and the plugin registry.

It is deliberately the *only* process-wide state in the crate:

* it lives in **L3**, never in L1, so the stateless-L1 rule is untouched;
* it holds **dispatch** state — a reference to the VM, a pointer to the L2
  service set, a pointer to the plugin registry — and **never game state**;
* the host installs it once, before any native callback can run, and clears it
  before the engine is dropped;
* everything else that outlives a callback lives in the `CompatServices`
  state map ([`ctx.md`](ctx.md)).

The layer rules that require this are in
[`../../rules/arch_rules.md`](../../rules/arch_rules.md) §3; the layer-level
description is in [`../../arch/l3.md`](../../arch/l3.md) §7.

---

## 1. The table

```rust
struct Binding {
    engine_raw: usize,  // the C++ engine pointer native callbacks receive
    engine:     usize,  // the address of the Rust host engine
    services:   usize,  // the address of the host-owned L2 services
    registry:   usize,  // the address of the host-owned plugin registry
}

static BINDING: LazyLock<Mutex<Option<Binding>>> = ...;
```

The table stores **plain `usize` addresses** so it needs no `unsafe impl
Send`/`Sync`: the addresses are only dereferenced on the single VM thread.
`lock()` recovers from a poisoned mutex rather than panicking — a poisoned
lock means a previous holder panicked, not that the table is unusable.

`Resolved` is the copy the callback works with: it is `Copy`, so **no lock is
held while a native callback runs**. It offers three accessors:

| Accessor | Returns |
|---|---|
| `engine()` | `&'static Tjs2Engine` — for safe method calls |
| `registry()` | `&'static PluginRegistry` |
| `ctx()` | a `CompatCtx` rebuilt from the two addresses |

Each dereference is a documented `unsafe` block: the invariant is that the
stored addresses belong to a live host engine and a live service set that
outlive the binding.

---

## 2. Install / uninstall

`install(engine, services, registry)`:

1. **Fails if a binding is already installed.** There is exactly one VM per
   process, so a second install is a bug, not a re-binding:
   `CompatError::Register("compat binding already installed")`.
2. Stores the four addresses.
3. Installs the **storage read seam** into the VM (see §3) and the **text
   decode seam** (see §4).

`uninstall()` clears both seams first (passing no user pointer) and then
empties the table. `CompatHost::Drop` calls it, and `CompatHost::uninstall`
calls it explicitly, so a host that re-installs against a different engine
starts from a clean table.

`resolve(engine: *mut Engine)` matches the raw pointer against
`engine_raw` and returns a `Resolved` copy. A pointer that is not the bound
engine yields `None` — the caller raises a typed error rather than dispatching
against an unbound context.

---

## 3. The storage seam

The VM's text and binary stream factories resolve a storage name
(`arc.xp3>member`) through a callback the binding installs:

```rust
tjs::set_storage_read_callback(Some(storage_read), services as *mut c_void);
```

`storage_read` is the C-ABI read:

| Situation | Result |
|---|---|
| the storage is attached and holds the name | `STORAGE_READ_OK` with a malloc'd copy of the bytes |
| the storage is attached and does not hold the name | `STORAGE_READ_NOT_FOUND` — the factory falls back to the local filesystem |
| any other storage error | `STORAGE_READ_ERROR` with the error message |
| no storage attached | `STORAGE_READ_NOT_FOUND` — the factory uses the local filesystem |
| the name is not UTF-8 | `STORAGE_READ_ERROR` |
| a null result slot | `STORAGE_READ_ERROR` |

The name is passed through
[`resource.md`](resource.md)'s `strip_file_media` first, so a script's
`file://.`-prefixed name reaches the L2 namespace as the bare VFS name.

Error messages are written into the caller's `out_error` slot with
`tjs2_malloc`, which is malloc-compatible; the C++ stream layer frees them
with `tjs2_free_string`. A failed allocation leaves the slot untouched and
still reports the error status.

---

## 4. The text decode seam

The VM's text read stream — the one `Array.load` opens — owns the `FE FE`
container and the byte-order-mark branches of a payload, but the **charset
of a BOM-less payload is the host's decision**, because the host owns the
detector ([`text.md`](text.md)):

```rust
tjs::set_text_decode_callback(Some(text_decode), ptr::null_mut());
```

`text_decode` decodes the raw payload with `crate::text`'s detector and hands
the stream UTF-16 code units:

| Situation | Result |
|---|---|
| the payload decodes | `TEXT_DECODE_OK` with a malloc'd buffer of UTF-16 code units and their count |
| the payload cannot be decoded | `TEXT_DECODE_ERROR` with the error message |
| a null result slot | `TEXT_DECODE_ERROR` |
| the allocation fails | `TEXT_DECODE_ERROR` |

The reference picks the charset of a BOM-less payload from the bytes with
uchardet and remaps `SHIFT_JIS` to `cp932`
(`reference/cpp/core/base/TextStream.cpp:47-57`), so a BOM-less Shift-JIS
member reads as Japanese instead of as mojibake. The seam carries no user
pointer: the detector is a pure function of the bytes.

---

## 5. Why the addresses are stable

The binding stores the **address** of the engine and of the services, so both
must stay at a stable address for as long as the binding is installed. Two
things guarantee that:

* `CompatHost` boxes its `CompatServices` and its `PluginRegistry`, so moving
  the host does not move them;
* the engine itself is owned by the caller and must stay behind a `Box` (as
  `crate::Host` and `vm`'s host do).

`CompatHost::install` documents this requirement on the `engine` argument.

---

## 6. Tests

The binding is not directly testable — it is process-global and single. It is
covered **indirectly** by every test that installs a compat host, and
structurally by the rules:

| Check | What it proves |
|---|---|
| `krkr-gate house-style` | the `// SAFETY:` notes on every dereference are present |
| `krkr-gate layering` | the module is L3 and names no L1 service |
| `crates/compat/tests/common/mod.rs::vm_lock` | the test suite serialises VM creation, because the binding is process-global and single |
| `crates/compat/src/native_util.rs::test_vm_lock` | the in-crate counterpart for unit tests |

The single-binding rule is exercised by the whole suite: a second
`CompatHost::install` in the same process fails with
`"compat binding already installed"`, and the integration tests rely on that
by never installing twice concurrently.

---

## See also

* [`ctx.md`](ctx.md) — the `CompatCtx` and `CompatServices` this table points at
* [`host.md`](host.md) — the install/uninstall lifecycle
* [`../../arch/l3.md`](../../arch/l3.md) §7 — the surface lock and the one binding
* [`../../../docs.old/migration/08-tjs-vm-and-native-plugins.md`](../../../docs.old/migration/08-tjs-vm-and-native-plugins.md) — the ABI constraint that forces the table
