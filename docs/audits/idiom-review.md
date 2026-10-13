# Idiom / house-style audit — `535deae..f73fbb2`

Scope: the new and changed code in commits `535deae` and `f73fbb2`
(`git diff f0bd13a..HEAD`), reviewed against `AGENTS.md` §5 (idiomatic Rust)
and §6 (comments, docs, naming), plus the D-rules those sections name
(D9, D11, D16, D30, D31, D32, D36, D37). This is a reading review, not a
clippy re-run. The two guard scripts were run after every edit; both are
clean, and `rustfmt --edition 2024` is clean on every file touched.

Method: read every added/changed non-test source line, checked the named
hot spots (the `VmHost` constructors and attach seam, the plugin
`activate`/`autoload` surface, the `xp3_filter` state types, the `.tpm`
scan, the ABI length hand-off, the `FE FE` arithmetic, the `xp3_filter`
test helpers), then checked the new tests for citations.

**Status: all findings below are fixed.** A first pass fixed the four
clear-cut comment/log/duplication violations; the follow-up fixed the
lifecycle failure, the naming, the allocation, and the minors.

---

## Findings and their resolution

### 1. Decision number in a logged warning and in a doc comment — FIXED

- `crates/compat/src/plugin.rs` — `log::warn!("… null plugin path (D14)")`
  and a doc comment naming "the documented D14 divergence".

Rule: §6 (no decision numbers in printed/logged text) and D31 (no decision
numbers in comments). Fix: dropped `(D14)` from the warning and reworded the
doc to describe the behaviour.

### 2. Internal jargon (`L2`) in logged messages — FIXED

- `crates/compat/src/natives/plugins/xp3_filter/mod.rs` — the two deferred
  activation `debug!` messages said "L2 storage …".

Rule: §6 — `L1`–`L4` must not appear in printed/logged text. Fix: "storage
not attached". The `docs/audits/xp3-filter.md` §5 sample lines were updated to
match.

### 3. History and rationale in a production doc comment — FIXED

- `crates/android/src/log_file.rs` — the `LOG_ROTATED_FILES` doc narrated
  "the old single-rotation policy" and "the observed cascade was 87% repeated
  dumps"; the module doc compared to "the single rotation".

Rule: D31. Fix: the docs now state what the constant is and the disk-use bound.

### 4. Duplicated XP3 builder in a new test — FIXED

- `crates/compat/tests/xp3_filter_deferred.rs` re-implemented
  `testkit::build_xp3`.

Rule: §8/§10 cohesion. Fix: uses `testkit::build_xp3` with `zlib = false`;
local `xor`/`GameDir` remain (see below).

### 5. `begin_native_timing` ran non-timing work — FIXED

- `crates/compat/src/host.rs` — `begin_native_timing` called
  `activate_deferred_plugins` before opening the timing window.

Rule: §5 — a function must be named for the work it does. Fix (follow-up):
`begin_native_timing`/`end_native_timing` are timing-only again, and the
deferred activation is the explicit, named
`CompatHost::activate_deferred_plugins`. The composition root calls it in
`GameHost::open` immediately before `begin_native_timing` — the same point in
the startup sequence the old hook occupied (after `storage.startup_plan()`,
before `run_startup`), so no ordering changes. The harness
(`crates/compat/tests/common/mod.rs`) and the deferred-path test
(`crates/compat/tests/xp3_filter_deferred.rs`) call the named step explicitly;
the `natives/scripts.rs` timing test is unaffected because it never needed
activation. Doc links in `natives/plugins/mod.rs` and `xp3_filter/mod.rs`
point at the new method.

### 6. Deferred-activation error was swallowed — FIXED

- `crates/compat/src/host.rs` — `begin_native_timing` returned `()`, so a
  late-attach `xp3filter.tjs` read failure was only `log::warn!`ed and the
  game ran unfiltered.

Rule: D11 — no silent fallback; a failure must be a typed error the caller
handles. Fix (follow-up): `activate_deferred_plugins() -> Result<(), CompatError>`
returns the read failure; `GameHost::open` propagates it via
`GameError::Plugin`, so the open stops instead of running unfiltered. The step
stays idempotent (no-op once `script_read`/`seam_installed` are set) and keeps
the `debug` line distinguishing "storage not attached yet" from "absent".

### 7. `decode_text` built a diagnostic label on the common path — FIXED

- `crates/compat/src/text.rs` — `decode_text` called `decode_text_inner(...).0`
  while `decode_text_inner` unconditionally built the encoding-decision
  `String` (`format!`/`to_string`).

Rule: §5 — avoid needless `to_string`/allocation. Fix (follow-up): the
decision is the `Copy` `DecodePath` enum (no allocation for a BOM, the
container mode, an explicit name, or the detector's `&'static str`); only
`decode_text_reported` calls `DecodePath::label()`. The label strings are
unchanged byte-for-byte, so the `script read '<name>' bytes=… encoding=<decision>
…` device-log line is identical.

### 8. `autoload` took `&[String]` — FIXED

- `crates/compat/src/plugin.rs` — signature was `names: &[String]`.

Rule: §5 parameter guidance. Fix (follow-up):
`autoload<I, S>(&self, names: I, …) where I: IntoIterator<Item = S>, S: AsRef<str>`,
so the caller can pass `&[&str]` or `&Vec<String>` without owning a slice of
`String`. Both call sites are unchanged.

### 9. `as` narrowing in the log timestamp — FIXED

- `crates/vm/src/host.rs` — `civil_from_days((secs / 86_400) as i64)` and
  `month as u32` / `day as u32`; `duration_since(...).map_or(0, …)`.

Rule: §5/D11 — prefer `TryFrom`/`checked_*`, no silent fallback. Fix
(follow-up): `i64::try_from` with a `let-else` (and `civil_from_days` now
returns `(i64, i64, i64)`, so the month/day casts are gone entirely). The
pre-epoch `map_or(0, …)` is replaced by an explicit `"unknown"` return for a
clock that cannot be represented; this only affects an impossible clock, and
`"unknown"` is already the caller's fallback string.

### 10. New behaviour tests lacked `reference/cpp` citations — FIXED

- `crates/compat/tests/layer_ex.rs` — the resize-button test now cites
  `reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:1392` and the
  method registration `…/main.cpp:902`; the reject test cites the
  `GetNativeInstance` adaptor (`…/main.cpp:105-113,902`).
- `crates/compat/tests/plugin_autoload.rs` — the `Plugins.link` test cites
  `reference/cpp/core/plugin/PluginImpl.cpp:294-307` and marks the unported-name
  null object as the port's divergence (the reference only logs a failure,
  `:48-56`), rather than inventing a reference behaviour.
- `crates/compat/tests/xp3_filter_deferred.rs` — cites the reference's
  post-registration read, `reference/cpp/plugins/xp3filter.cpp:550-564`.

Rule: D30. The internal `log_dedup_*` / `auto_path_trace_*` unit tests assert
the port's own logging, not reference behaviour, and need no citation.

### 11. `discover_tpm_plugins` allocated a lowercase copy per entry — FIXED

- `crates/app/src/game.rs` — `entry.name.to_ascii_lowercase().ends_with(".tpm")`.

Rule: §5 — needless allocation. Fix (follow-up): a `has_tpm_extension` helper
compares the last four bytes with `eq_ignore_ascii_case`, citing
`reference/cpp/core/plugin/PluginImpl.cpp:76-88`.

---

## Remaining open items

None in code. One documentation file could not be edited from this review's
write scope (a verified replacement is staged at
`/tmp/xp3-filter-updated.md`; `cp` it into place, or widen this session's
scope to include it and send a follow-up and I will apply it):

- `docs/audits/xp3-filter.md` is outside the allowed edit scope
  (`crates/**` + this file). It still says the deferred hook runs inside
  `begin_native_timing`, still shows the old `L2` log text in its §5 sample
  lines, and still describes the deferred read failure as a `warn!`. The
  replacements are prepared and should be applied under a widened scope; they
  are: intro "deferred hook in `begin_native_timing`" →
  "deferred hook (`CompatHost::activate_deferred_plugins`)"; §2.3's
  `begin_native_timing` bullet → the named, fallible
  `activate_deferred_plugins`; §2.4's "deferred `activate` in
  `begin_native_timing`" paragraph → the named step and its returned failure;
  §5's two `deferred: L2 storage …` lines → `deferred: storage …`; §5's
  "check that `CompatHost::begin_native_timing` is reached" →
  `activate_deferred_plugins`; and §5's "reported as `deferred plugin
  activation: …` at `warn`" → both paths return `CompatError::Register`.

The main agent's new WIP `scripts/check-house-style.sh` (§5/§6 guard) flagged
three pre-existing decision-number comments; this review fixed them rather than
allowlisting (the guard's own notes say a decision number never belongs in the
allowlist):

- `crates/android/src/config.rs` — dropped `(D11)` from the unparseable-value
  doc.
- `crates/compat/src/natives/system.rs` — dropped `(D18)` from the
  `exeName` fallback doc.
- `crates/engine/src/storage.rs` — reworded the `discover_archives` doc to
  drop `(D15 / doc 09)`.

---

## Clean list — checked and found compliant

**Bean accessors (§5).** No new `get_x`/`set_x` field-mover. The new state
(`Xp3FilterState`, `DecodedText`, `FramePacing`, `FrameTimings`,
`FramePhases`) uses public fields or direct field assignment; the new
constructors are not bean setters. `CompatHost` getters return live internal
objects that cannot be a public field.

**Parameters / ownership (§5).** No `&String`, `&Vec<T>`, or `&Box<T>` in the
new production signatures. `Box<LogDedup>` and `Box<Tjs2Engine>` are
load-bearing (stable addresses for the log callback and the VM binding), and
`&mut CompatCtx` in `autoload` is genuinely mutated by `activate`.

**Narrowing / size math (D11, §5).** The ABI length hand-off is clean: the
Rust wrapper passes `script.as_ptr().cast::<c_char>()` with `script.len()`
(usize), matching C++ `size_t`. The `FE FE` cipher path uses
`bytes.get(4..).unwrap_or_default()` (slice, not arithmetic), `chunks_exact(2)`,
and `u16::from_le_bytes`. The `.tpm` scan does no arithmetic casts. The only
`as` on pointer-derived values are the intentional opaque `as usize` hand-offs
(`read_instance`, the filter closure's engine address), which D37 governs and
the pointer guard accepts.

**Naming (§5/§6).** `read_instance`, `log_script_read`,
`discover_tpm_plugins`, `has_tpm_extension`, `tpm_to_dll_name`,
`AutoPathTrace` / `ShapeEntries`, `FALLBACK_FACE`,
`FrameAttribution::slow_frame_line`, `step_with_pacing`,
`activate` / `activate_deferred_plugins` all describe what they do;
`begin_native_timing` is timing-only again.

**Error types and states (D11).** `ScriptLookup` distinguishes
`StorageNotAttached` / `Absent` / `Found`, and `SeamInstall` distinguishes
`Installed` / `StorageNotAttached`; neither folds "no read was possible" into
"not found". `read_filter_script` returns `Err(String)` for a real read
failure and `Ok(Absent)` only for `StorageReadError::NotFound`. No failure is
swallowed on any path now.

**Layering (§2).** No `std::fs`, `#[cfg(target_os)]`, `winit`,
`android-activity`, or `jni` added to `compat` source. The `.tpm` scan is in
`app` and uses L1 `fs::list`; `compat` receives ordered names and links them.
The `vm` attach seam stays L4 → L3/L2. The new L2 code
(`engine::storage::AutoPathTrace`) uses generic vocabulary.

**Comments / D31 (§6).** The new module docs describe current behaviour. The
`L1`/`L2` references in `compat` comments are allowed by §6 (the ban is on
printed/logged text); only the log strings were fixed. The `vm::host`
`LogDedup`/`PendingDump` docs describe how the deduplicator works and its
invariants, with no history and no decision numbers.

**Guards.** `bash scripts/check-stateless.sh`,
`bash scripts/check-error-handling.sh`, and the new (work-in-progress)
`bash scripts/check-house-style.sh` all exit 0 on the tree after the
follow-up edits.

---

## Files changed by this audit

| File | Change |
|---|---|
| `crates/compat/src/plugin.rs` | `(D14)` removed from the log and doc; `autoload` takes `impl IntoIterator<Item = impl AsRef<str>>`-style generic bounds. |
| `crates/compat/src/natives/plugins/xp3_filter/mod.rs` | `L2` removed from the two deferred log lines; doc links point at `activate_deferred_plugins`. |
| `crates/compat/src/host.rs` | `begin_native_timing` is timing-only; `activate_deferred_plugins` is `pub` and returns `Result<(), CompatError>` instead of `warn!`-ing. |
| `crates/compat/src/natives/plugins/mod.rs` | Doc points at `activate_deferred_plugins`. |
| `crates/compat/src/text.rs` | `DecodePath` (`Copy`) replaces the eager label string; `decode_text` no longer allocates. |
| `crates/app/src/game.rs` | Calls `activate_deferred_plugins()?` before `begin_native_timing`; `has_tpm_extension` replaces the `to_ascii_lowercase()` allocation. |
| `crates/vm/src/host.rs` | UTC logger uses `i64::try_from`; `civil_from_days` returns `(i64, i64, i64)`; pre-epoch clock returns `"unknown"`. |
| `crates/android/src/log_file.rs` | History/rationale removed from the `LOG_ROTATED_FILES` and module docs. |
| `crates/compat/tests/common/mod.rs` | Harness calls the named deferred step before `begin_native_timing`. |
| `crates/compat/tests/xp3_filter_deferred.rs` | Local `build_xp3` deleted (uses `testkit`); calls `activate_deferred_plugins`; reference citation added. |
| `crates/compat/tests/layer_ex.rs`, `crates/compat/tests/plugin_autoload.rs` | `reference/cpp` citations added; the port's null-plugin divergence marked as such. |
| `crates/android/src/config.rs`, `crates/compat/src/natives/system.rs`, `crates/engine/src/storage.rs` | Pre-existing decision-number comments removed (flagged by the new WIP `check-house-style.sh`). |

Pending (outside scope): `docs/audits/xp3-filter.md`.
