# `config` — the launcher's settings → the validated runtime configuration

Source: `crates/android/src/config.rs` (the resolver) and
`crates/android/src/jni_fields.rs` (the JNI reads)

`KrkrGameActivity` persists the user's choices in its own settings file and
publishes them as the static string fields `pendingFrameRate`,
`pendingPreloadArchives`, `pendingDebugOverlay` and `pendingLogLevel` before
`super.onCreate` enters native code. This module is where those raw strings
become the one validated value the emulator receives, so **the launcher keeps
owning configuration loading and the core does no configuration I/O of its
own**.

---

## 1. Responsibility

* resolve the launcher's published strings into a validated `RuntimeConfig`;
* resolve the launcher's log level into the filter the process logger uses;
* read the launcher's static fields over JNI.

---

## 2. Layer and dependencies

| | |
|---|---|
| Layer | root (composition root) |
| Depends on | `api::config` (`RuntimeConfig`, `FrameRateLimit`, `CacheLimit`, `ConfigError`, `parse_bool`) |
| Must not depend on | `engine`, `compat`, `vm` |
| Forbidden | a settings file of its own, a silent default for an invalid value |
| Gate | `config` — `cfg(any(target_os = "android", test))`; `jni_fields` — the field names are always compiled, the read is Android-only |

`api` is a pure L1 type crate, so the resolver is **host-tested** as well as
compiled for Android.

---

## 3. The resolver

```rust
pub(crate) fn resolve_runtime_config(
    frame_rate: Option<&str>,
    preload_archives: Option<&str>,
    debug_overlay: Option<&str>,
) -> Result<RuntimeConfig, ConfigError>;

pub(crate) fn resolve_log_level(value: Option<&str>) -> Result<LogLevel, ConfigError>;

pub(crate) enum LogLevel { Off, Error, Warn, Info, Debug, Trace }
impl LogLevel {
    pub(crate) const DEFAULT: Self = Self::Info;
    pub(crate) const ACCEPTED: &'static str = "one of: off, error, warn, info, debug, trace";
    pub(crate) fn parse(option: &'static str, value: &str) -> Result<Self, ConfigError>;
    pub(crate) fn as_filter(&self) -> &'static str;
}
```

Each option is optional: an unset field means the launcher had **no
override**, and the built-in default applies. A present but unparseable or
out-of-range value is a typed `ConfigError` naming the option and the
accepted values; it is **never** silently replaced by the default.

The defaults are the shared built-ins:

| option | accepted values | default |
|---|---|---|
| the frame-rate limit | a decimal integer in `1..=1000` | `60` |
| the archive preload | `true`, `false` | `false` |
| the debug overlay | `true`, `false` | `false` |
| the log level | `off`, `error`, `warn`, `info`, `debug`, `trace` | `info` |

`info` omits the VM's diagnostic detail: the VM's per-exception disassembly,
register dump and console output reach the logger as `debug` records, so
**`debug` is the level that surfaces a script exception** in `krkr.log`.

The graphic cache limit is a desktop CLI option only; the Android launcher
leaves the engine's reference default (disabled), so `graphic_cache_limit` is
always `None` here.

Because the launcher stores each value as the exact string the user chose and
does not validate it itself, validation belongs to this read side — exactly
as it does for the desktop `key=value` file.

---

## 4. The JNI reads

```rust
pub(crate) const FIELD_GAME_DIR: &str = "pendingGameDir";
pub(crate) const FIELD_GAME_NAME: &str = "pendingGameName";
pub(crate) const FIELD_STATE_DIR: &str = "pendingStateDir";
pub(crate) const FIELD_FRAME_RATE: &str = "pendingFrameRate";
pub(crate) const FIELD_PRELOAD_ARCHIVES: &str = "pendingPreloadArchives";
pub(crate) const FIELD_DEBUG_OVERLAY: &str = "pendingDebugOverlay";
pub(crate) const FIELD_LOG_LEVEL: &str = "pendingLogLevel";
pub(crate) const SIG_STRING: &str = "Ljava/lang/String;";

pub(crate) fn activity_static_string(app: &AndroidApp, field: &str, signature: &str)
    -> Result<Option<String>, jni::errors::Error>;
```

The launcher starts the engine Activity with an Intent extra and, **before**
`super.onCreate` loads this library, copies that value onto a static field.
`android_main` therefore always observes the field already set, with no race
and no extra IPC. This is the only JNI in the engine; everything else goes
through the platform-neutral code the desktop build uses.

The field is read from the **Activity instance's class** rather than through
`FindClass`, because `FindClass` on a native-attached thread resolves against
the system class loader and would not find the application class.

### `Ok(None)` versus `Err`

The two must stay apart:

* `Ok(None)` means the field is null or empty — the launcher published **no
  value**, so the default applies;
* `Err` means a JNI step failed and the value, if present, could not be
  read. This is reported like a missing game directory, never silently
  defaulted.

Every failure is logged and any pending Java exception is cleared, so the
`android_main` thread is left clean for the loop's later JNI use.

---

## 5. State ownership

The resolver owns no state: it is a pure function of the published strings.
The `RuntimeConfig` it returns is passed to the composition root, which hands
the parts to the engine (`GameHost::open_with_preload`,
`HostConfig::with_debug_overlay`, the ticker's period) and the log level to
the logger's filter.

The launcher keeps the persisted copy; the emulator never writes it back.

---

## 6. Platform gating

| item | gate |
|---|---|
| `resolve_runtime_config`, `resolve_log_level`, `LogLevel` | `cfg(any(target_os = "android", test))` — host-tested |
| the field-name constants | always compiled, so a host test can pin them |
| `activity_static_string` | `cfg(target_os = "android")` — it needs the `jni` crate |

---

## 7. Failure modes

| failure | behaviour |
|---|---|
| the frame rate is unparseable or outside `1..=1000` | `ConfigError::InvalidValue` naming `frame_rate` and the accepted range |
| the preload or the overlay value is not `true`/`false` | `ConfigError::InvalidValue` naming the option and the accepted values |
| the log level is not one of the six names | `ConfigError::InvalidValue` naming `log_level` and every accepted value |
| a JNI read fails | the error is reported, the pending Java exception is cleared, and the caller treats it as a failure — never as "unset" |
| a field is null or empty | `Ok(None)` — the default applies |

At the entry point, an invalid value is echoed on stderr, written to the
state file as `failed`, and **the loop is never started**. A JNI read failure
is reported the same way.

---

## 8. Tests

`crates/android/src/config.rs` `mod tests`:

* the unset fields taking the Android defaults (60 fps, no preload, no
  overlay);
* the present values overriding the defaults;
* each field falling back to **its own** default independently of the options
  beside it;
* the debug overlay resolving on and off;
* the out-of-range values being rejected with the option name and the range
  (`frame_rate`, `1..=1000`, `preload_archives`, `true, false`,
  `debug_overlay`, `true, false`);
* an invalid field being rejected even when the others are valid;
* every log level name parsing to its filter;
* an unset log level taking the documented default;
* an unknown log level being rejected with the option name and every
  accepted value;
* the accepted-values text naming every level, so a level cannot be added
  without the error message naming it.

`crates/android/src/jni_fields.rs` `mod tests` pins the field names against
the `@JvmField` statics `KrkrGameActivity` publishes — a rename on one side
only would make the JNI read observe null and silently take the default.

---

## 9. Related documents

| topic | document |
|---|---|
| the Android composition root overview | [README.md](README.md) |
| the entry point that consumes the resolved values | [host.md](host.md) §3 |
| the shared configuration value types | [`../platform/api.md`](../platform/api.md) §4 |
| the configuration surface as a whole | [`runtime-config.md`](../../../docs.old/runtime-config.md) |
| the launcher's settings file and the JNI path | [`android.md`](../../../docs.old/android.md) §3.2 · snapshot [`docs.old/android.md`](../../../docs.old/android.md) |
| the runtime configuration rule | [`../../rules/arch_rules.md`](../../rules/arch_rules.md) §9 |
