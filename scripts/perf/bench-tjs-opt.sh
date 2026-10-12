#!/usr/bin/env bash
#
# bench-tjs-opt.sh — A/B the vendored TJS2 C++ VM at different optimisation
# levels, without touching the Rust build.
#
# It compiles the same sources crates/tjs/build.rs compiles (the vendored
# tjs2 *.cpp, the generated bison parsers, tjs2_abi.cpp and streams.cpp) plus
# the standalone driver in this directory, once per configuration, and then
# runs two measurements with each binary:
#
#   compile  the 48 real-game .tjs scripts from the game XP3, repeated
#            --iters times in one process; prints the average per pass
#   exec     scripts/perf/workload.tjs, a dispatch/object/array/string
#            workload; prints the wall time of one execution
#
# The `exec` result value is printed (`check total = ...`) so a wrong-code
# regression from an aggressive flag is visible.
#
# Usage:
#   scripts/perf/bench-tjs-opt.sh [--game DIR] [--corpus DIR] [--runs N]
#                                 [--iters N] [--configs "N|flags;N|flags"]
#                                 [--work-dir DIR] [--keep]
#
# Defaults: game /mnt/DATA/Games/Others/test, runs 7, iters 20, and a config
# set of -O2, -O3 and -O2 -flto. `WORK` and `ZIGOUT` are honoured from the
# environment. Nothing here runs cargo.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TJS2="$ROOT/crates/tjs/cpp/tjs2"
ABI="$ROOT/crates/tjs/cpp"
DRIVER="$ROOT/scripts/perf/tjs2_bench.cpp"
WORKLOAD="$ROOT/scripts/perf/workload.tjs"

GAME="${GAME:-/mnt/DATA/Games/Others/test}"
CORPUS=""
RUNS="${RUNS:-7}"
ITERS="${ITERS:-20}"
WORK="${WORK:-$ROOT/target/perf/tjs-opt}"
CONFIGS_DEFAULT='O2|-O2;O3|-O3;O2full|-O2 -flto'
CONFIGS="$CONFIGS_DEFAULT"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --game) GAME="$2"; shift 2 ;;
        --corpus) CORPUS="$2"; shift 2 ;;
        --runs) RUNS="$2"; shift 2 ;;
        --iters) ITERS="$2"; shift 2 ;;
        --configs) CONFIGS="$2"; shift 2 ;;
        --work-dir) WORK="$2"; shift 2 ;;
        -h|--help) sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "bench-tjs-opt.sh: unknown argument '$1'" >&2; exit 2 ;;
    esac
done

mkdir -p "$WORK"
GEN="$WORK/gen"

# --- headers and libonig.a ------------------------------------------------
ZIGOUT="${ZIGOUT:-}"
if [[ -z "$ZIGOUT" ]]; then
    if [[ -d "$ROOT/deps/zig-out/include" ]]; then
        ZIGOUT="$ROOT/deps/zig-out"
    else
        ZIGOUT="$(ls -d "$ROOT"/target/release/build/tjs-*/out/zig-out 2>/dev/null | head -1 || true)"
    fi
fi
if [[ -z "$ZIGOUT" || ! -f "$ZIGOUT/lib/libonig.a" ]]; then
    echo "bench-tjs-opt.sh: no staged deps (expected \$ZIGOUT/lib/libonig.a)." >&2
    echo "  run 'zig build' in $ROOT/deps once, or set ZIGOUT." >&2
    exit 1
fi

# --- script corpus ---------------------------------------------------------
if [[ -z "$CORPUS" ]]; then
    CORPUS="$WORK/corpus"
    if [[ ! -d "$CORPUS" ]]; then
        if [[ ! -f "$GAME/data.xp3" ]]; then
            echo "bench-tjs-opt.sh: no $GAME/data.xp3 (pass --game or --corpus)." >&2
            exit 1
        fi
        python3 "$ROOT/scripts/perf/extract-scripts.py" "$GAME/data.xp3" "$CORPUS"
    fi
fi
n_tjs="$(find "$CORPUS" -name '*.tjs' | wc -l)"
echo "bench-tjs-opt.sh: corpus=$CORPUS ($n_tjs .tjs files)" >&2

# --- generated bison parsers ----------------------------------------------
if [[ ! -f "$GEN/tjs.tab.cpp" ]]; then
    mkdir -p "$GEN"
    ( cd "$GEN" && for y in "$TJS2"/bison/tjs.y "$TJS2"/bison/tjsdate.y "$TJS2"/bison/tjspp.y; do bison "$y"; done ) >&2
    ( cd "$TJS2/script" && python3 create_world_map.py "$GEN/tjsDateWordMap.inc" ) >&2
fi

SOURCES=( "$TJS2"/*.cpp "$GEN/tjs.tab.cpp" "$GEN/tjsdate.tab.cpp" "$GEN/tjspp.tab.cpp" "$ABI/tjs2_abi.cpp" "$ABI/streams.cpp" )

CFLAGS=(
    -std=c++17 -fPIC -pthread -fno-sanitize=undefined -DNDEBUG
    -DTJS_TEXT_OUT_CRLF -D__STDC_CONSTANT_MACROS -DUSE_UNICODE_FSTRING
    -DFMT_HEADER_ONLY -DSPDLOG_FMT_EXTERNAL -Wno-date-time
    -Wno-tautological-constant-out-of-range-compare
    -I"$TJS2" -I"$GEN" -I"$ABI" -I"$ZIGOUT/include"
)
export CFLAGS_STR="${CFLAGS[*]}"

build_config() {
    local name="$1"; shift
    local dir="$WORK/$name"
    local obj="$dir/obj"
    rm -rf "$dir"; mkdir -p "$obj"
    export OPT_STR="$*" OBJ_DIR="$obj"
    echo "  compile: zig c++ $CFLAGS_STR $OPT_STR -c <each source>" >&2
    printf '%s\0' "${SOURCES[@]}" | xargs -0 -P"$(nproc)" -I{} bash -c '
        src="$1"; obj="$OBJ_DIR/$(basename "$src" .cpp).o"
        zig c++ $CFLAGS_STR $OPT_STR -c "$src" -o "$obj"' _ {}
    zig c++ "${CFLAGS[@]}" "$@" -c "$DRIVER" -o "$obj/bench_driver.o"
    echo "  link:    zig c++ $* <objects> $ZIGOUT/lib/libonig.a -lz -o $dir/bench" >&2
    zig c++ "$@" "$obj"/*.o "$ZIGOUT/lib/libonig.a" -lz -o "$dir/bench"
}

printf '%-10s %-34s %14s   %14s   %s\n' config flags compile_ms exec_ms check
IFS=';' read -ra CFG_LIST <<< "$CONFIGS"
for cfg in "${CFG_LIST[@]}"; do
    name="${cfg%%|*}"
    flags="${cfg#*|}"
    read -ra flag_arr <<< "$flags"
    echo "=== $name (${flag_arr[*]}) ===" >&2
    build_config "$name" "${flag_arr[@]}"

    cvals=""
    for _ in $(seq 1 "$RUNS"); do
        v="$("$WORK/$name/bench" compile "$CORPUS" "$ITERS" --tjs-only 2>/dev/null \
             | sed -E 's/.*avg_ms=([0-9.]+).*/\1/')"
        cvals+="$v "
    done
    evals=""
    check=""
    for _ in $(seq 1 "$RUNS"); do
        out="$("$WORK/$name/bench" exec "$WORKLOAD" 1 total 2>/dev/null)"
        v="$(sed -nE 's/exec iters=1 avg_ms=([0-9.]+).*/\1/p' <<< "$out")"
        evals+="$v "
        check="$(sed -nE 's/^check total = //p' <<< "$out")"
    done
    printf '%-10s %-34s %14s   %14s   %s\n' "$name" "$flags" "$cvals" "$evals" "$check"
done
