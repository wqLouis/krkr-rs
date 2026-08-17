# krkr-rs — a KiriKiri2 (TVP) visual-novel engine in Rust + Bevy

krkr-rs is a from-scratch Rust rewrite of the **krkr2 / KiriKiri2** visual-novel
engine — the engine behind thousands of Japanese visual novels (`.ks` scenario
scripts, `.tjs` scripts, `.xp3` archives). The goal: run a real game end to end.

- **Rust** — all game logic, scripting integration, and asset handling.
- **Bevy 0.19** — rendering (window, GPU sprites, input), ECS, app lifecycle.
- **C++ TJS2 VM** — kept as-is (vendored + minimally patched), compiled by
  `build.rs` with `zig c++` (no cmake), statically linked via `tjs2-sys`.

## Status

| Area | State |
|---|---|
| `.xp3` mounting / storage | ✅ real archives (data.xp3: 23,572 entries) |
| `startup.tjs` execution | ✅ full init chain (k2compat, `system/*.tjs`, `begin.tjs`) |
| Plugin surface (`Plugins.link`) | ✅ emulated as built-in natives (csvParser, fstat, windowEx, KAGParser, menu, extrans, wuvorbis) |
| TLG5/TLG6 image decode | ✅ real KiriKiri formats (byte-identical to reference) |
| Logo → Title scene flow | 🔶 chain runs; stalled at the ATTENTION screen (see TODO.md) |
| Input bridge (mouse/key → script) | ✅ wired; click-through verification pending |
| Audio (BGM/SE/voice) | 🔶 rodio output wired (rodio 0.22, symphonia 0.6); Opus voice decode pending |
| ADV scenario loop | 🔶 KAGParser natives in; needs text + hit-test verification |

Details and the full investigation log are in [TODO.md](TODO.md).

## Quick start

```bash
cargo build --release

# Run a real game (windowed — needs a display/GPU):
./target/release/krkr-rs run "/path/to/game"

# Headless smoke test (no window; dumps the scene once):
./target/release/krkr-rs run "/path/to/game" --headless
# → must print "startup.tjs executed successfully"
```

The test game used during development is at `/mnt/DATA/Games/Others/test`
(not part of this repo).

## Development

```bash
cargo test --workspace          # unit + integration, full parallel speed
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

The TJS2 VM is **single-threaded**: crates that embed it serialize their own
tests with a per-crate process-wide lock (`VM_LOCK`), so the rest of the
workspace still runs fully parallel. `run_vm` in the render crate also holds a
global lock because Bevy's scheduler moves systems across worker threads —
without it the shared native state (sound streams, timer registry, continuous
handlers) deadlocks across threads.

## Repo layout

```
crates/
  xp3, tvp-archive      — XP3 archive parsing
  tjs2-sys              — vendored C++ TJS2 VM + FFI (zig c++ build)
  engine                — storage mount + game loader
  tvp-natives           — System / Debug / Plugins / MenuItem / Trans natives
  tvp-storages          — Storages natives (getFileList, stat, copy, delete)
  tvp-scripts           — Scripts natives
  tvp-kagparser         — KAGParser natives (scenario .ks parsing)
  tvp-visual            — Window / Layer / Bitmap / Font / Timer natives + Scene
  tvp-sound             — WaveSoundBuffer / SoundChannel natives + mixer + rodio output
  tvp-text              — text layout / rasterization for Layer.drawText
  tvp-input             — input state bridge
  render                — Bevy app: krkr-rs binary (windowed game runner)
  app                   — krkr-cli binary (headless load/run/list tools)
```

## Reference material

The upstream C++ sources live under `reference/` (krkr2 core + plugins) — the
porting source of truth for native semantics. `scripts/extract_xp3.py` pulls
files out of a game's `.xp3` to read its scripts:

```bash
./scripts/extract_xp3.py /path/to/game/data.xp3 system/Title.tjs
```
