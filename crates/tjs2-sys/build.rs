//! build.rs for tjs2-sys.
//!
//! Compiles the C++ TJS2 scripting VM from the krkr2 reference checkout
//! directly with `zig c++` (no cmake), plus the C ABI shim in
//! `cpp/tjs2_abi.cpp`, and statically links the result into the Rust binary.
//!
//! Pipeline:
//!   1. run `zig build` in `../../deps` (fetches fmt/spdlog/boost/oniguruma
//!      via the zig package manager and stages headers + libonig.a into
//!      `$OUT_DIR/zig-out`)
//!   2. locate the reference checkout (env `KRKR2_REFERENCE` or
//!      `../../reference` relative to this crate)
//!   3. run `bison` on `bison/tjs.y`, `bison/tjsdate.y`, `bison/tjspp.y`
//!      into `$OUT_DIR/gen`
//!   4. run `python3 script/create_world_map.py` to generate
//!      `tjsDateWordMap.inc`
//!   5. compile all tjs2 `.cpp` sources + the shim with `zig c++`
//!      (`-std=c++17 -fPIC`, defines matching the upstream CMake)
//!   6. archive into `libtjs2_core.a` and emit `cargo:rustc-link-*` for
//!      libonig.a and the C++ runtime (libc++).

use std::env;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

fn fail(msg: &str) -> ! {
    println!("cargo:warning={msg}");
    panic!("{msg}");
}

/// The tjs2 sources listed in the upstream `cpp/core/tjs2/CMakeLists.txt`.
const TJS2_SOURCES: &[&str] = &[
    "tjsLex.cpp",
    "tjsNative.cpp",
    "tjsRandomGenerator.cpp",
    "tjsDebug.cpp",
    "tjsRegExp.cpp",
    "tjsBinarySerializer.cpp",
    "tjsInterCodeGen.cpp",
    "tjsObject.cpp",
    "tjsConstArrayData.cpp",
    "tjsUtils.cpp",
    "tjsException.cpp",
    "tjsDictionary.cpp",
    "tjsInterCodeExec.cpp",
    "tjsVariantString.cpp",
    "tjsScriptBlock.cpp",
    "tjsMath.cpp",
    "tjs.cpp",
    "tjsMessage.cpp",
    "tjsDisassemble.cpp",
    "tjsDate.cpp",
    "tjsError.cpp",
    "tjsInterface.cpp",
    "tjsMT19937ar-cok.cpp",
    "tjsArray.cpp",
    "tjsByteCodeLoader.cpp",
    "tjsDateParser.cpp",
    "tjsGlobalStringMap.cpp",
    "tjsOctPack.cpp",
    "tjsNamespace.cpp",
    "tjsScriptCache.cpp",
    "tjsConfig.cpp",
    "tjsCompileControl.cpp",
    "tjsObjectExtendable.cpp",
    "tjsVariant.cpp",
    "tjsString.cpp",
];

const BISON_GRAMMARS: &[&str] = &["tjs.y", "tjsdate.y", "tjspp.y"];

fn run(cmd: &mut Command, what: &str) {
    let status = cmd.status().unwrap_or_else(|e| {
        fail(&format!("failed to run {what}: {e}"));
    });
    if !status.success() {
        fail(&format!("{what} failed with status {status}"));
    }
}

fn which(tool: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let cand = dir.join(tool);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());

    // -- zig ----------------------------------------------------------------
    let zig = env::var("ZIG")
        .ok()
        .or_else(|| which("zig").map(|p| p.display().to_string()));
    let Some(zig) = zig else {
        fail("zig not found (needed to build C++ deps and compile tjs2). Install it or set ZIG.");
    };
    println!("cargo:rerun-if-env-changed=ZIG");

    // Build C++ deps: fmt/spdlog/boost headers + libonig.a.
    let deps_dir = manifest_dir.join("../../deps");
    if !deps_dir.join("build.zig").is_file() {
        fail(&format!(
            "deps zig project not found at {}",
            deps_dir.display()
        ));
    }
    let deps_out = out_dir.join("zig-out");
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".into());
    let zig_optimize = if profile == "release" {
        "ReleaseFast"
    } else {
        "Debug"
    };
    let mut cmd = Command::new(&zig);
    cmd.arg("build")
        .arg("--prefix")
        .arg(&deps_out)
        .arg(format!("-Doptimize={zig_optimize}"))
        .current_dir(&deps_dir);
    run(&mut cmd, "zig build (deps)");
    println!(
        "cargo:rerun-if-changed={}",
        deps_dir.join("build.zig").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        deps_dir.join("build.zig.zon").display()
    );

    // -- locate the tjs2 sources (vendored copy, patched for krkr-rs) -------
    let tjs2_dir = manifest_dir.join("cpp/tjs2");
    if !tjs2_dir.join("tjs.cpp").is_file() {
        fail(&format!(
            "vendored tjs2 sources not found at {} — run `./scripts/vendor-tjs2.sh` to (re)vendor from the reference checkout",
            tjs2_dir.display()
        ));
    }
    println!("cargo:rerun-if-changed={}", tjs2_dir.display());

    // -- toolchain ----------------------------------------------------------
    let cc: Vec<String> = env::var("CXX")
        .map(|s| s.split_whitespace().map(str::to_string).collect())
        .ok()
        .or_else(|| Some(vec![zig.clone(), "c++".into()]))
        .unwrap();

    let bison = env::var("BISON")
        .ok()
        .or_else(|| which("bison").map(|p| p.display().to_string()));
    let Some(bison) = bison else {
        fail("bison not found (needed to generate the TJS2 parsers)");
    };
    let python = env::var("PYTHON").ok().or_else(|| {
        which("python3")
            .map(|p| p.display().to_string())
            .or_else(|| which("python").map(|p| p.display().to_string()))
    });
    let Some(python) = python else {
        fail("python3 not found (needed to generate tjsDateWordMap.inc)");
    };

    // -- generate parsers with bison ---------------------------------------
    let gen_dir = out_dir.join("gen");
    std::fs::create_dir_all(&gen_dir).unwrap();
    for y in BISON_GRAMMARS {
        let src = tjs2_dir.join("bison").join(y);
        let mut cmd = Command::new(&bison);
        cmd.arg(&src).current_dir(&gen_dir);
        run(&mut cmd, &format!("bison on {y}"));
    }
    println!(
        "cargo:rerun-if-changed={}",
        tjs2_dir.join("bison").display()
    );

    // -- generate date word map --------------------------------------------
    let script_dir = tjs2_dir.join("script");
    let word_map_out = gen_dir.join("tjsDateWordMap.inc");
    let mut cmd = Command::new(&python);
    cmd.arg("create_world_map.py")
        .arg(&word_map_out)
        .current_dir(&script_dir);
    run(&mut cmd, "create_world_map.py");
    println!("cargo:rerun-if-changed={}", script_dir.display());

    // -- compile ------------------------------------------------------------
    let obj_dir = out_dir.join("obj");
    std::fs::create_dir_all(&obj_dir).unwrap();

    let mut flags: Vec<String> = vec![
        "-std=c++17".into(),
        "-fPIC".into(),
        "-pthread".into(),
        // zig enables UBSan by default in Debug; we link via rustc and don't
        // want sanitizer runtime deps in the static archive.
        "-fno-sanitize=undefined".into(),
        format!("-I{}", tjs2_dir.display()),
        format!("-I{}", gen_dir.display()),
        format!("-I{}", manifest_dir.join("cpp").display()),
        format!("-I{}", deps_out.join("include").display()),
        "-DTJS_TEXT_OUT_CRLF".into(),
        "-D__STDC_CONSTANT_MACROS".into(),
        "-DUSE_UNICODE_FSTRING".into(),
        "-DFMT_HEADER_ONLY".into(),
        "-DSPDLOG_FMT_EXTERNAL".into(),
        // zig's release modes imply -Werror; silence upstream noise that
        // would otherwise break the build (non-reproducible date/time macro,
        // tautological size comparisons in tjsBinarySerializer.h).
        "-Wno-date-time".into(),
        "-Wno-tautological-constant-out-of-range-compare".into(),
    ];
    if profile == "release" {
        flags.push("-O2".into());
        flags.push("-DNDEBUG".into());
    } else {
        flags.push("-O0".into());
        flags.push("-g".into());
        flags.push("-D_DEBUG".into());
        flags.push("-DDEBUG".into());
    }

    // Collect all sources: upstream tjs2 + generated parsers + our shim.
    let mut sources: Vec<PathBuf> = TJS2_SOURCES.iter().map(|s| tjs2_dir.join(s)).collect();
    for y in BISON_GRAMMARS {
        let stem = y.strip_suffix(".y").unwrap();
        sources.push(gen_dir.join(format!("{stem}.tab.cpp")));
    }
    sources.push(manifest_dir.join("cpp/tjs2_abi.cpp"));

    let failed = std::sync::Arc::new(AtomicUsize::new(0));
    let lock = std::sync::Arc::new(Mutex::new(()));
    let handles: Vec<_> = sources
        .into_iter()
        .map(|src| {
            let cc = cc.clone();
            let flags = flags.clone();
            let obj_dir = obj_dir.clone();
            let failed = failed.clone();
            let lock = lock.clone();
            std::thread::spawn(move || {
                let obj = obj_dir.join(
                    src.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .replace(".cpp", ".o"),
                );
                let mut cmd = Command::new(&cc[0]);
                cmd.args(&cc[1..])
                    .args(&flags)
                    .arg("-c")
                    .arg(&src)
                    .arg("-o")
                    .arg(&obj);
                let ok = cmd.status().map(|s| s.success()).unwrap_or(false);
                if !ok {
                    failed.fetch_add(1, Ordering::SeqCst);
                    let _guard = lock.lock().unwrap();
                    println!("cargo:warning=compile failed: {}", src.display());
                    return;
                }
                println!("cargo:rerun-if-changed={}", src.display());
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    if failed.load(Ordering::SeqCst) > 0 {
        fail(&format!(
            "{} C++ file(s) failed to compile (see warnings above)",
            failed.load(Ordering::SeqCst)
        ));
    }

    // -- archive into a static lib ------------------------------------------
    let lib = out_dir.join("libtjs2_core.a");
    let objects: Vec<PathBuf> = std::fs::read_dir(&obj_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "o").unwrap_or(false))
        .collect();
    let mut ar = Command::new("ar");
    ar.arg("crus").arg(&lib).args(&objects);
    run(&mut ar, "ar");

    // -- link directives -----------------------------------------------------
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=tjs2_core");
    println!(
        "cargo:rustc-link-search=native={}",
        deps_out.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=onig");
    if env::consts::OS == "linux" {
        // C++ runtime: tjs2 was compiled with zig's clang/libc++.
        println!("cargo:rustc-link-lib=dylib=c++");
        println!("cargo:rustc-link-lib=dylib=c++abi");
        println!("cargo:rustc-link-arg=-pthread");
    }
}
