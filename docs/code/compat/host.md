# `host.rs` — `CompatHost`, the dispatch-state owner

**Source:** `crates/compat/src/host.rs`

`CompatHost` is what a composition root constructs. It owns the L3 dispatch
state, installs the compatibility surface on a VM, and removes it again. It is
the only public type a host needs to hold.

```rust
let mut host = CompatHost::new();
host.services_mut().attach_scene(scene.as_mut() as *mut Scene);
host.services_mut().attach_storage(storage.as_mut() as *mut Storage);
host.install(&engine)?;                 // register the classes, then bind
```

---

## 1. What it owns

| Field | Type | Lifetime |
|---|---|---|
| `services` | `Box<CompatServices>` | the L3 dispatch state — L2 service handles, the per-family state map, the scratch buffers |
| `registry` | `Box<PluginRegistry>` | the built-in plugin table and the linked-name list |
| `engine` | `usize` | the address of the bound engine |
| `services_ptr` | `usize` | the address of the boxed services |
| `bound` | `bool` | whether the binding is installed |

Both state values are **boxed** so their addresses stay stable when the host
is moved — the binding stores raw addresses and relies on that stability.

---

## 2. The lifecycle

### `install(&mut self, engine) -> Result<(), CompatError>`

1. **Refuses a second install** — `CompatError::Register("compat host is already installed")`.
2. Builds a `CompatCtx` from the engine and services addresses.
3. Calls the crate-internal `install(&mut ctx)`, which registers every
   native family, then the `Plugins` class. **Registration happens before the
   binding**, so a registration failure leaves no half-installed table.
4. Installs the process-wide binding ([`binding.md`](binding.md)). A failure
   here means a binding was already present.
5. Points the VM's stream factories at the attached storage's project
   directory, so a relative save name resolves under the game path rather than
   at the process working directory. **If the directory cannot be represented,
   the binding is rolled back** and the error is returned.
6. Records the addresses and sets `bound`.

The `engine` argument must stay at a **stable address** for as long as the
binding is installed — keep it behind a `Box` and do not move it afterwards.

### `uninstall(&mut self)`

**Crate-internal.** The public teardown is `shutdown`, which also drops
the dispatch state while the engine is alive.

Idempotent. Removes the binding and forgets the cached no-op closure id so a
later re-install against a different engine creates a fresh one. The service
state map itself stays until the host is dropped or `shutdown` is called.

### `shutdown(&mut self)`

`uninstall` plus replacing the service set with a fresh one. **This must run
while the engine is still alive**: the dispatch state holds values retained on
the VM, and their `Drop` releases the id on the engine. `uninstall` alone
leaves the state map in place until the host is dropped, which can be after
the engine has been destroyed. The `vm` host calls `shutdown` from its own
`Drop` before the engine field is dropped.

### `Drop`

Calls `uninstall`. A host that is dropped without `shutdown` leaves the
retained values in the state map to be released by the engine's own
destruction, which is safe but leaks the retention bookkeeping until then.

---

## 3. Accessors

| Method | Purpose |
|---|---|
| `services()` / `services_mut()` | the L2 service handles and the per-family state map — attach the scene, storage, input and timer **before** `install` |
| `registry()` | the plugin registry; a host that wants its own builds it with `PluginRegistry::new` and hands it to `with_registry` |
| `with_registry(registry)` | a host that uses a **given** registry instead of the built-in one; `new()` is `with_registry(PluginRegistry::builtin())` |
| `ctx()` | a dispatch context handle for the bound VM; `CompatError::Engine("compat host is not installed")` before `install` |
| `is_bound()` | whether the binding is currently installed |
| `set_video_source(spec, frames)` | install the host-decoded video source |
| `set_clipboard_backend(backend)` | install the platform clipboard |
| `set_desktop_rect(f)` | install the virtual-desktop rectangle (`System.desktop*`) |
| `set_image_codec(decode, probe, save)` | install the host image codec (defined in `host.rs`) |
| `set_system_font_dirs(dirs)` | install the host's system-font directories (defined in `host.rs`) |
| `set_window_ex_*` | install the `windowEx.dll` host facts (defined in `natives/plugins/window_ex/host.rs`) |

Every `set_*` seam installs a **closure**; a member whose closure is missing
raises a typed TJS error rather than fabricating a value.

---

## 4. Diagnostics

| Method | Counts |
|---|---|
| `menu_node_count()` | live `MenuItem` nodes the engine retains |
| `timer_count()` | live `Timer` bindings the engine retains |
| `sound_object_count()` | live `WaveSoundBuffer` objects the engine retains |
| `async_trigger_count()` | live `AsyncTrigger` bindings the engine retains |

Each is a **retention-leak probe**: the corresponding class removes its
binding in its destroy callback, so a count that grows across create/drop
cycles is a leak.

---

## 5. Startup timing and deferred plugins

### `begin_native_timing()` / `end_native_timing()`

Open and close the per-native dispatch-timing window. While it is open, every
instrumented native records its accumulated dispatch time; closing it emits
**one** report line listing the top ten members by total milliseconds with
their call counts — at `info` when a single member reached the documented
50 ms threshold, at `debug` otherwise. The window is closed before the first
frame, so the frame path is never timed.

### `activate_deferred_plugins() -> Result<(), CompatError>`

Completes plugin work that needed an L2 service **not yet attached** when
`install` ran. The `xp3filter` plugin reads `xp3filter.tjs` through the
storage while it registers; a composition root that attaches the storage after
installing leaves that read pending until here. The call also re-applies the
stream data directory (idempotent) and returns a read failure so the caller
can stop instead of running unfiltered. It is idempotent and a no-op once the
work has run.

---

## 6. Host facts

A handful of reference members describe genuine host facts with no L2
counterpart — currently the virtual-desktop geometry behind
`System.desktopLeft`/`desktopTop`/`desktopWidth`/`desktopHeight`. The host
installs one closure per fact:

```rust
host.set_desktop_rect(|| Ok((left, top, width, height)));
```

Until a fact is installed, its member raises a typed TJS error rather than
returning a fabricated value.

---

## 7. Errors

| Variant | Raised when |
|---|---|
| `CompatError::Register("compat host is already installed")` | `install` on an already-bound host |
| `CompatError::Register("compat binding already installed")` | the process-wide binding is taken by another host |
| `CompatError::Engine("compat host is not installed")` | `ctx()` before `install` |
| `CompatError::Engine("stream data dir is not valid UTF-8")` | the storage's game directory is not UTF-8; the binding is rolled back |
| `CompatError::Engine(...)` | a native class registration failed (`crate::install`) |
| `CompatError::Engine(...)` | a deferred plugin read failed |

---

## 8. Tests

The host is exercised by every integration test that needs a bound VM; the
harness lives in `crates/compat/tests/common/mod.rs` (`Env`, `vm_lock`).

| Test | What it asserts |
|---|---|
| `crates/compat/tests/common/mod.rs` | the shared harness: the engine, the host, the attached scene/storage/input/timer, and the serialisation lock |
| `crates/compat/tests/oop.rs::super_native_constructor_produces_a_fully_usable_instance` | a native class is constructible through `super.<Class>(…)` and is fully usable afterwards |
| `crates/compat/tests/window.rs::invalidating_a_window_releases_its_scene_tree_and_registered_objects` | the registered objects' `finalize` runs and the scene tree is released |
| `crates/compat/tests/window.rs::dropping_the_last_window_reference_releases_its_scene_tree` | the scene tree, its bitmaps and its fonts are released |
| `crates/compat/tests/plugin_autoload.rs` | `Plugins` autoload through the registry |
| `crates/compat/tests/xp3_filter_deferred.rs` | `activate_deferred_plugins` completes the pending `xp3filter` read |

---

## See also

* [`ctx.md`](ctx.md) — `CompatServices` and the state map
* [`binding.md`](binding.md) — the process-wide table the host installs
* [`plugin.md`](plugin.md) — the registry the host owns
* [`../../arch/l3.md`](../../arch/l3.md) §1 — the crate surface
