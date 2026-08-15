# AGENT.md

Guidance for AI coding agents working in this repository.

## Project overview

**krkr-rs** is a from-scratch Rust rewrite of the old open-source **krkr2 (KiriKiri2)** Android application — the visual-novel engine used by countless Japanese games (scenario `.ks` scripts, `.tjs` scripts, `.xp3` archives).

The legacy Android app relied on an old engine ("coco") that we are **dropping entirely**. The rewrite is a full migration to **Rust + Bevy**:

- **Rust** — all game logic, scripting, and asset handling.
- **Bevy** — rendering, ECS, windowing, input, and app lifecycle (not wired up yet; no rendering in the current milestone).
- **C++ TJS2 VM** — kept as-is (vendored + minimally patched), compiled by `build.rs` with `zig c++` (no cmake) and statically linked.

## Current milestone: the load module

Goal: **load a game** — mount storage (game dir + `.xp3` archives), discover `startup.tjs`, bootstrap the TJS2 VM, execute the script. No UI/rendering.

Status:
- [x] `crates/xp3` — pure-Rust XP3 archive reader (+ fixture tests, `dump` example)
- [x] `crates/tjs2-sys` — C++ TJS2 VM via `build.rs` (zig + bison, no cmake), C ABI shim, safe Rust wrapper
- [x] `crates/engine` — load module: `Storage` mount + name resolution, `load_game()`, `krkr-rs` CLI (`load` / `run` / `list`)
- [ ] TVP native classes (`Storages`, `System`, `Scripts`, `Debug`, ...) so real games' `startup.tjs` can run — the next milestone
- [ ] Bevy integration (rendering)

## Repository layout

```
crates/
  xp3/         # pure-Rust XP3 archive reader (no engine deps)
  tjs2-sys/    # C ABI bindings to the C++ TJS2 VM
    build.rs   # zig build (deps) + bison + zig c++ compile + static link
    cpp/
      tjs2/    # VENDORED tjs2 sources from the reference (patched — see below)
      tjs2_abi.{h,cpp}  # the C ABI boundary (keep in sync with src/lib.rs)
    src/lib.rs # safe wrapper over the C ABI
  engine/      # load module: storage, VM bootstrap, startup.tjs execution, CLI
deps/          # zig package project: fmt/spdlog/boost/oniguruma (fetch + build)
reference/     # shallow clone of the krkr2 emulator (gitignored; reference only)
scripts/       # vendor-tjs2.sh — re-vendor + re-apply patches to the tjs2 copy
```

## Build system (important — no cmake)

`crates/tjs2-sys/build.rs` does everything:

1. **C++ deps via the zig package manager** (`deps/build.zig` + `build.zig.zon`, populated with `zig fetch --save`):
   - `fmt` 10.2.1 — headers only (`FMT_HEADER_ONLY`); **fmt 11+ requires C++20, tjs2 is C++17, do not upgrade**
   - `spdlog` 1.14.1 — headers only (header-only mode, `SPDLOG_FMT_EXTERNAL`); 1.15 targets fmt 11
   - `boost` 1.87.0 — headers only (`boost::locale::conv::utf_to_utf` is header-only; no lib linked)
   - `oniguruma` 6.9.10 — compiled into `libonig.a` (config.h generated via `zig build`'s `addConfigHeader`; the `*_data.c` files are `#include`d by `unicode.c` and must NOT be compiled standalone)
   - output staged to `$OUT_DIR/zig-out/`
2. **bison** generates `tjs.tab.cpp/hpp`, `tjsdate.tab.cpp/hpp`, `tjspp.tab.cpp/hpp` from the vendored `bison/*.y` into `$OUT_DIR/gen`
3. **python3** `script/create_world_map.py` generates `tjsDateWordMap.inc`
4. **`zig c++`** compiles the ~35 tjs2 sources + 3 generated parsers + `tjs2_abi.cpp` with `-std=c++17 -fPIC` and defines `TJS_TEXT_OUT_CRLF`, `__STDC_CONSTANT_MACROS`, `USE_UNICODE_FSTRING`, `FMT_HEADER_ONLY`, `SPDLOG_FMT_EXTERNAL`; `-fno-sanitize=undefined` (zig enables UBSan in Debug; we link via rustc)
5. `ar` archives into `libtjs2_core.a`; links `libonig.a`, `-lc++ -lc++abi` (zig uses libc++; `-stdlib=libstdc++` is ignored by zig), `-pthread`

System tools required: `zig`, `bison` (3.8.2), `python3`. No system C++ packages needed.

### Vendored tjs2 + patches

`crates/tjs2-sys/cpp/tjs2/` is a copy of `reference/cpp/core/tjs2/` with krkr-rs patches. **Do not edit it by hand** — run `scripts/vendor-tjs2.sh` to re-vendor and re-apply patches (it asserts the patch anchors so upstream changes fail loudly).

Current patches (see the script for exact diffs):
- `tjsInterCodeGen.cpp` `parser::error`: upstream only logs and relies on grammar error recovery, which leaves half-parsed blocks that crash at execution. We throw `TJS_eTJSScriptError(msg, ptr, -1)` instead (the bison-generated parser exposes the script block as `ptr`).

### C ABI boundary

`cpp/tjs2_abi.h` defines the stable interface (engine create/destroy, exec/eval, log callback, `tjs2_value`). It is mirrored by hand in `crates/tjs2-sys/src/lib.rs` — keep both in sync. `tjs2_abi.cpp` also provides `TVPGetMessageByLocale` (stub; in the full engine it lives in the environ module) and registers the spdlog loggers the tjs2 code expects (`spdlog::get("tjs2")` etc. — without them the VM crashes on first log call).

## Commands

```sh
cargo build                       # native build (compiles C++ via build.rs)
cargo run -p engine --bin krkr-rs -- load <game-dir>   # load a game
cargo run -p engine --bin krkr-rs -- run <game-dir> <script>
cargo run -p engine --bin krkr-rs -- list <game-dir>
cargo run -p xp3 --example dump -- <file.xp3>          # inspect an archive
cargo test --workspace -- --test-threads=1             # tjs2 VM is not thread-safe
cargo clippy --all-targets -- -D warnings              # must stay clean
cargo fmt --check
```

## Architecture rules

- **Engine-agnostic crates** (`xp3`) must not depend on tjs2-sys/engine; only the integration layer imports Bevy (not yet present).
- **The TJS2 VM is not thread-safe** — tjs2 keeps global state; one engine per thread, no concurrent engines. `Tjs2Engine` deliberately does not implement `Send`/`Sync`.
- **Storage naming** follows the reference (`TVPSearchPlacedPath`): names normalized (lowercase, `/`), disk wins over archives, `arc.xp3>path` addresses archive members.
- **Errors**: `thiserror` for library errors; script errors surface as `String` messages across the FFI.
- **Formatting/lint**: `cargo fmt` + `cargo clippy -- -D warnings` before finishing changes.
- **Commits**: conventional-commit prefixes (`feat:`, `fix:`, `refactor:`, `test:`, `docs:`).

## Known quirks / gotchas

- `ExecScript` returns the value of an explicit top-level `return` only; expression-mode results come from `eval`. This matches the reference (games get results via `Scripts.execStorage`, implemented on top).
- Zig `fetch --save=<name>` is required for non-zig packages; `build.zig.zon` needs a `fingerprint` field (zig 0.16).
- The reference repo contains its own Rust-migration docs (`reference/docs/rust/`) — that plan is the *opposite* direction (C++ host + Corrosion). krkr-rs deliberately does the reverse: Rust host, C++ VM linked in via build.rs.
