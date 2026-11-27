# Gates and CI — the static gate, the tests, the release pipeline

Status: **authoritative** for the quality gate and the release workflow. The
agent process rules are in [`AGENTS.md`](../../AGENTS.md) §0; this page records
the ordered gate, what each check protects, how to run a single check, and what
a `v*` tag builds.

The gate is one uv project in `scripts/` (`krkr-gates`), whose entry point is
`krkr-gate`. Its checks are Python and standard-library only; they inspect the
tree, they do not build it (except `android-target`, which resolves the
dependency graph with `cargo tree`).

---

## 1. The ordered static gate

The main agent runs:

```bash
uv run --project scripts krkr-gate gate
```

It runs the steps below in this order and stops at the first failure:

| # | Step | Kind | What it protects |
|---|---|---|---|
| 1 | `cargo fmt --all -- --check` | cargo | formatting |
| 2 | `stateless` | check | no process-global state in the bottom-core crates (D9) |
| 3 | `error-handling` | check | no silent fallbacks or dropped errors (AGENTS §4) |
| 4 | `house-style` | check | bean accessors, parameter shapes, comment rules (AGENTS §5/§6) |
| 5 | `log-format` | check | every log record names its subsystem (`docs/rules/logging.md`) |
| 6 | `layering` | check | layer vocabulary, unsafe placement, the L3↔L2 protocol |
| 7 | `storage-boundary` | check | script storage names do not reach L2 or the host unstripped |
| 8 | `native-construction` | check | native instances are constructed when they are created |
| 9 | `lifecycle-order` | check | a native teardown must not take another script object's Scene record |
| 10 | `storage-media` | check | the reference's storage-media domains are served or documented; the checkout is required |
| 11 | `callback-objthis` | check | callback-invoking native members carry objthis |
| 12 | `native-arity` | check | the reference's zero-argument natives are not rejected |
| 13 | `void-getter` | check | a native getter does not return a raw void where the reference returns an object |
| 14 | `native-instance-lookup` | check | invalidated instances resolve by class; dispatch keeps the invalidation guard |
| 15 | `cargo clippy --workspace --all-targets -- -D warnings` | cargo | the host build |
| 16 | `cargo clippy -p platform --features platform-android --all-targets -- -D warnings` | cargo | the Android feature on the host |
| 17 | `android-target` | check | no desktop-only backend leaks into the Android graph, or vice versa |
| 18 | `apk-surface` | check | the Android keep rules name every JNI field the engine reads, keep only classes the manifest starts, and the release script it cannot run resolves |
| 19 | `surface-lock` | check | the live VM dispatch dump matches the committed baseline (D29) |
| 20 | `native-parity --no-doc` | check | the native class surface vs the reference |
| 21 | `plugin-parity --no-doc` | check | the plugin surface vs the reference |
| 22 | `host-facts` | check | every host seam has a production installer |
| 23 | `ci` | check | the release workflow contract (v* → Linux + Android in parallel → one release) |
| 24 | `doc-lists` | check | the pages that list the gate's steps match the gate |

The `cargo` steps are the integrator's, and a worker may not run cargo at all,
so a worker may run a compiler-free check but never the build gate.
`gate --no-cargo` runs the checks only, and `gate --keep-going` reports every
failure instead of stopping at the first one.

Steps 10, 12, 19 and 20 — `storage-media`, `native-arity`, `native-parity` and
`plugin-parity` — read the reference checkout, which is not part of this
repository. It is `<repo>/reference` unless the run names another one:
`krkr-gate --reference PATH <check>` sets `KRKR_REFERENCE` for the run, and
`krkr-gate --reference PATH gate` hands it to every step. Those four checks
fail with the paths they tried rather than scanning nothing, so a clone kept
somewhere else needs the option (or the variable) on every run.

The gate **compares**; it never rewrites the tree. `surface-lock` reads the
live VM dispatch dump and compares it with the committed baseline, so a
surface change fails the gate; accepting one is the integrator's separate
action (`krkr-gate surface-lock --update`), reviewed as a diff in
`docs/surface/`. Both parity checks run with `--no-doc`, so a gate run does
not rewrite `docs/native_parity.md` or `docs/plugin_parity.md`; a report is
regenerated deliberately, when the reference or the surface has moved.

The gate does **not** run the test suites. A change is done only when both the
static gate and the tests are green.

---

## 2. The test suites

```bash
uv run --project scripts krkr-gate test
```

This runs, in order:

1. `cargo test --workspace --no-fail-fast`;
2. `cargo test -p platform --features platform-linux`;
3. `cargo test -p platform --features platform-android`.

The third suite builds the platform-independent helpers and the typed stub
**on the host**; the Android target itself is never built or tested on the
development host. CI builds it on a runner with an NDK, and code that has not
run on a device is still described as unverified (D36).

---

## 3. Running one check

Every check runs on its own and takes `--quiet`:

```bash
uv run --project scripts krkr-gate stateless
uv run --project scripts krkr-gate error-handling
uv run --project scripts krkr-gate house-style
uv run --project scripts krkr-gate layering
uv run --project scripts krkr-gate storage-boundary
uv run --project scripts krkr-gate native-construction
uv run --project scripts krkr-gate lifecycle-order
uv run --project scripts krkr-gate storage-media
uv run --project scripts krkr-gate callback-objthis
uv run --project scripts krkr-gate native-arity
uv run --project scripts krkr-gate void-getter
uv run --project scripts krkr-gate native-instance-lookup
uv run --project scripts krkr-gate host-facts
uv run --project scripts krkr-gate android-target
uv run --project scripts krkr-gate ci
uv run --project scripts krkr-gate surface-lock --no-regen
uv run --project scripts krkr-gate native-parity
uv run --project scripts krkr-gate plugin-parity --no-doc
```

Every check also accepts `--self-test`, which runs its fixture/extractor test
and exits. A check that cannot fail is not a check: the self-test is what keeps
a regex from silently matching nothing.

`surface-lock --update` is **main-agent only** and is only for a reviewed
surface change. The `ci` check is described in §4 and validates the workflow
without running it.

`native-instance-lookup` guards the native-instance ABI seam in
`crates/tjs/cpp/tjs2_abi.cpp`: `tjs2_get_native_instance` must resolve a
live-but-invalidated instance through `GetNativePtrForLookup()`, that accessor
must stay the valid-only class lookup, and the method/property/constructor
dispatch bodies must keep using the invalidation-guarded `GetNativePtr()`.
Its fixture self-test applies four mutations (lookup uses the dispatch
accessor, lookup accessor grows the dispatch guard, a dispatch body drops the
guarded accessor, and the dispatch accessor loses its invalidation guard) and
requires the matching rule to fire.

---

## 4. The `ci` check

`krkr-gate ci` reads `.github/workflows/release.yml` as text and fails when:

* the tag trigger is not exactly `v*`, a branch push is configured, or
  `workflow_dispatch` is missing;
* any workflow still names an `android-*` tag, or the old Android-only
  workflow file exists;
* the workflow runs the repository static gate (`krkr-gate gate`), which is a
  local pre-tag check rather than a release job;
* the `linux` and `apk` jobs are not independent;
* the Linux job does not run `cargo build --release -p app` or does not package
  `krkr-rs-linux-x86_64.tar.gz` with the binary, `LICENSE` and a run note;
* the Android job does not invoke `crates/android/build-android.sh` and
  `crates/android/check-jni-libs.sh`;
* the final `release` job does not need both platform jobs, download both
  artifacts and attach both to the release;
* more than one job creates or uploads a GitHub Release.

Its fixtures live in `scripts/fixtures/ci/`: a small valid workflow plus one
mutation per rule. The self-test applies each mutation and asserts the matching
rule fires, so adding a rule without a fixture fails the check.

---

## 5. The release pipeline

`.github/workflows/release.yml` runs on **`v*` tags** and on
`workflow_dispatch`. A branch push triggers nothing. The jobs are:

```
linux ──┐
        ├──> release
apk   ──┘
```

The workflow is deliberately **build-only**. It does not install uv, clone the
reference checkout or rerun `krkr-gate gate`; that gate is the local pre-tag
check and repeats compilation the two build jobs already do. The tag must
therefore point at a commit whose static gate and test suites are already
green (section 6).

* **`linux`** runs `cargo build --release -p app` on `ubuntu-latest`
  (x86_64) and packages `target/release/krkr`, `LICENSE` and a `README.txt`
  run note as `krkr-rs-linux-x86_64.tar.gz`. Zig 0.17 fetches the C++
  dependencies, but the VM itself is compiled with the runner's
  `clang++ -stdlib=libc++`: Zig's bundled libc++ is newer than Ubuntu's
  `libc++` runtime, and the Rust link step uses that runtime, so compiling
  with Zig's headers would leave `std::__1::__hash_memory` unresolved.
* **`apk`** is the Android build: NDK r27c, SDK platform/build-tools, JDK 21,
  Zig, `build-android.sh`, Gradle, the `check-jni-libs.sh` self-test and
  package verification. The APK is signed with the configured keystore when the
  secret exists, otherwise with the debug key.
* **`apk`** ends with `scripts/apk_surface_check.sh`, before the upload. It
  checks the names the native code resolves *by name*: the static fields the
  engine reads off its Activity with `GetStaticFieldID`, and the
  game-activity glue's own by-name lookup of the native entry point.
  A shrinker that renames or drops one of them leaves a package that installs,
  launches and then dies on the device, with nothing in the build log to show
  for it, so the names are read out of the package and checked where no device
  is needed.
* **`release`** downloads both artifacts and is the only job that runs
  `gh release create`/`gh release upload`. It needs both platform jobs, so the
  two builds cannot race to create or update the release. On a manual run there
  is no tag, so the job is skipped and the artifacts remain on the run.

**The Linux binary is dynamically linked, not static.** It links the system
FFmpeg runtime libraries, ALSA, `libc++`/`libc++abi` and the C library, and
loads Vulkan/X11/Wayland at run time. The archive's `README.txt` names the
Debian/Ubuntu runtime packages; do not describe it as a static build.

The workflow's `permissions` are read-only at the top level; only the
`release` job raises `contents: write`. The keystore is decoded from a secret
and never written to the repository.

---

## 6. Release tags are metered

One `v*` tag builds **both** Linux and Android in one workflow. That is a
whole pipeline of CI minutes, so:

* commit locally, keep the gate and tests green;
* **hold the tag until the user explicitly approves the release**;
* do not tag on every green gate; batch the work and report what is ready.

The tag must be created on the commit that passed the gate. The version is the
tag, not a file: there is no separate version bump step.

---

## 7. The TJS2 tree

`crates/tjs/cpp/tjs2/` is the C++ TJS2 VM, maintained in this repository and
edited in place — including the undefined-behaviour fixes the compiler would
otherwise exploit at `-O1` and above. The tree is never regenerated from the
reference checkout, so a change to it is an ordinary change here and needs
no separate record. The reference checkout (§1.3 of `AGENTS.md`) is read-only
material — the behaviour oracle. What still enters the tree, and the order that
deletes it, is [`../code/tjs/engine.md`](../code/tjs/engine.md) §4.

---

## 8. Related documents

| Topic | Document |
|---|---|
| the agent contract and the gate commands | [`AGENTS.md`](../../AGENTS.md) §0.4 |
| the Rust review checklist | [`code_styles.md`](code_styles.md) |
| the layer, state, frame and error rules | [`arch_rules.md`](arch_rules.md) |
| the Android build and packaging | [`../code/android/build.md`](../code/android/build.md) |
