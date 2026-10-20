# `app` — runtime configuration

Source: `crates/app/src/main.rs` (the desktop store), `crates/platform/api/src/config.rs`
(the value types)

The engine is configured with **one explicit value**,
`api::config::RuntimeConfig`, resolved with the precedence

```
launch argument  >  the launcher's configuration store  >  built-in default
```

**The launcher owns configuration loading.** The emulator core reads no
file, no environment variable and no platform settings store; it only acts
on the value it is handed. Where configuration comes from is a platform
concern, so the desktop CLI reads a `key=value` file, the Android launcher
reads its own store, and both call the same `app::run`.

---

## 1. The value

```rust
pub struct RuntimeConfig {
    pub frame_rate_limit: FrameRateLimit,        // default 60 fps
    pub preload_archives: bool,                  // default false
    pub debug_overlay: bool,                     // default false
    pub graphic_cache_limit: Option<CacheLimit>, // default: the engine's reference default
}
```

| field | meaning |
|---|---|
| `frame_rate_limit` | the number of frames the host tries to present per second; it paces the windowed loop and the headless step |
| `preload_archives` | read every mounted archive into memory in full at mount, trading the archive's size in resident memory for removing the per-entry reads a mounted archive otherwise does |
| `debug_overlay` | draw the debug panel; it is a render-only node, so it cannot change what a script observes |
| `graphic_cache_limit` | the decoded-image cache policy; `None` leaves the engine's reference default (disabled) in place, and a startup script may still change it through `System.graphicCacheLimit` |

`FrameRateLimit` cannot be constructed outside `1..=1000` frames per
second: 1000 fps is the point at which the frame period reaches the
platform ticker's 1 ms minimum interval, and a higher limit would have to
be silently clamped, so it is rejected instead. Its period is
`1 / fps`.

`CacheLimit` is `Disabled` (store nothing), `Unlimited` (keep every entry)
or `Bytes(n)` (keep at most that many decoded bytes, evicting
least-recently-used entries until the cached bytes fit). A byte count of
zero means unlimited.

`RuntimeConfigOverrides` is the **partial** shape a parser or launcher
holds before the defaults are filled in: a source with no value for a field
leaves it unset, and the launcher falls back to the built-in default. The
partial shape keeps the precedence explicit instead of hiding the default
inside the source.

---

## 2. The desktop store

The desktop launcher reads the `key=value` file named by the `KRKR_CONFIG`
environment variable. An unset variable means no configuration; a configured
file that cannot be read is a **typed error**, never an empty result.

The file grammar (`api::config::parse_overrides`):

* one `key = value` per line; blank lines and lines whose first non-space
  character is `#` are ignored;
* the keys are `frame_rate`, `preload_archives`, `debug_overlay` and
  `graphic_cache_limit`;
* `preload_archives` and `debug_overlay` take `true` or `false`;
* `graphic_cache_limit` takes `none` / `off` / `disabled`, `unlimited`, `0`,
  or a byte count;
* a line without `=`, an unknown key, or a value that fails validation is
  an error — **nothing is silently skipped**, and an unknown-key diagnostic
  lists every accepted key so a key cannot be added without the message
  naming it.

Nothing is applied until the whole file parses, and the launch arguments
are merged over the file **field by field**, so a file that sets only the
frame rate still lets the command line override the overlay.

---

## 3. Validation

Every value is validated, and an unparseable or out-of-range value is a
typed `ConfigError` naming the option and the accepted range — it is never
clamped and never defaulted:

| error | meaning |
|---|---|
| `ConfigError::InvalidValue { option, value, expected }` | the value is unparseable or out of range |
| `ConfigError::Source { message }` | the store could not be read, or a line is malformed |

The command line is validated the same way: `--fps` accepts `1..=1000`,
`--graphic-cache-limit` accepts the cache forms above, and a malformed
command line has no partial effect — the parse completes before anything is
applied.

---

## 4. The other launchers

The Android root resolves the same value from its own store: the launcher
publishes its choices as static string fields before the native code is
entered, and the Android configuration module turns them into a validated
`RuntimeConfig` with the same precedence and the same typed errors. A
present-but-invalid value is never replaced by the default; an unset field
means the launcher had no override, which is a different thing.

The full contract, with both launchers side by side, is in
[`../../runtime-config.md`](../../../docs.old/runtime-config.md).

---

## 5. Related documents

| topic | document |
|---|---|
| the crate overview and the wiring | [README.md](README.md) |
| the CLI and the merge | [main.md](main.md) |
| the game host and the cache policy | [game.md](game.md) |
| the debug panel the overlay flag drives | [overlay.md](overlay.md) |
| the runtime configuration contract | [`../../runtime-config.md`](../../../docs.old/runtime-config.md) |
| the Android root | [`../../android.md`](../../../docs.old/android.md) |
