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
| 5 | `layering` | check | layer vocabulary, unsafe placement, the L3↔L2 protocol |
| 6 | `storage-boundary` | check | script storage names do not reach L2 or the host unstripped |
| 7 | `native-construction` | check | native instances are constructed when they are created |
| 8 | `lifecycle-order` | check | scene removal runs the child finalizers in order |
| 9 | `storage-media` | check | reference storage-media domains are served or documented |
| 10 | `callback-objthis` | check | callback-invoking native members carry objthis |
| 11 | `native-arity` | check | reference zero-argument natives are not rejected |
| 12 | `void-getter` | check | a native getter does not return a raw void where the reference returns an object |
| 13 | `cargo clippy --workspace --all-targets -- -D warnings` | cargo | the host build |
| 14 | `cargo clippy -p platform --features platform-android --all-targets -- -D warnings` | cargo | the Android feature on the host |
| 15 | `android-target` | check | no desktop-only backend leaks into the Android graph, or vice versa |
| 16 | `surface-lock --update` | check | regenerate the live VM dispatch dump |
| 17 | `surface-lock` | check | the dump matches the reviewed baseline (D29) |
| 18 | `native-parity` | check | native class surface vs the reference |
| 19 | `plugin-parity --no-doc` | check | plugin surface vs the reference |
| 20 | `host-facts` | check | every host seam has a production installer |
| 21 | `ci` | check | the release workflow contract (v* → gate → Linux + Android → one release) |

The `cargo` steps are the main agent's; they are the reason a worker may run a
compiler-free check but may not run the build gate. `gate --no-cargo` runs the
checks only, and `gate --keep-going` reports every failure instead of stopping
at the first one.

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

---

## 4. The `ci` check

`krkr-gate ci` reads `.github/workflows/release.yml` as text and fails when:

* the tag trigger is not exactly `v*`, a branch push is configured, or
  `workflow_dispatch` is missing;
* any workflow still names an `android-*` tag, or the old Android-only
  workflow file exists;
* the `gate` job is missing, does not run
  `uv run --project scripts krkr-gate gate`, or does not install uv, Rust and
  the Linux build dependencies;
* the `linux` or `apk` job does not `needs: gate`;
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
gate  ──┬──> linux ──┐
        └──> apk   ──┴──> release
```

* **`gate`** installs the Linux build dependencies (FFmpeg development
  libraries, ALSA, libc++, pkg-config, clang/libclang for bindgen, cmake,
  bison, python3), uv, Rust with rustfmt/clippy, and Zig. It clones the
  reference checkout shallowly — `native-parity` and `plugin-parity` compare
  against `reference/cpp`, which is not vendored — then runs the full static
  gate. It does not run `krkr-gate test`; the gate already builds every
  target the host can build, and the release jobs build the two artifacts.
* **`linux`** runs `cargo build --release -p app` on `ubuntu-latest`
  (x86_64) and packages `target/release/krkr`, `LICENSE` and a `README.txt`
  run note as `krkr-rs-linux-x86_64.tar.gz`.
* **`apk`** is the Android build: NDK r27c, SDK platform/build-tools, JDK 21,
  Zig, `build-android.sh`, Gradle, the `check-jni-libs.sh` self-test and
  package verification. The APK is signed with the configured keystore when the
  secret exists, otherwise with the debug key.
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

## 7. Vendored TJS2 and the patch file

`crates/tjs/cpp/tjs2/` is a vendored copy of the reference C++ TJS2 VM. The
krkr-rs edits to it — including the undefined-behaviour fixes the compiler
would otherwise exploit at `-O1` and above — live in
`scripts/patches/tjs2-krkr-rs.patch`, a unified diff against the reference
tree.

`scripts/vendor-tjs2.sh` re-copies the reference sources, applies that patch
with `patch -p1 --fuzz=0`, and then fails if a patch marker is missing. This
is deliberate: a re-vendor that silently drops a fix is the failure mode the
script exists to prevent. **Any new change under `crates/tjs/cpp/tjs2/` must
be added to the patch file**, or the next re-vendor will remove it. The script
header explains how to regenerate the patch from a diff of the reference and
the vendored tree.

---

## 8. Related documents

| Topic | Document |
|---|---|
| the agent contract and the gate commands | [`AGENTS.md`](../../AGENTS.md) §0.4 |
| the Rust review checklist | [`code_styles.md`](code_styles.md) |
| the layer, state, frame and error rules | [`arch_rules.md`](arch_rules.md) |
| the Android build and packaging | [`../code/android/build.md`](../code/android/build.md) |
