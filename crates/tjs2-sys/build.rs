//! build.rs for tjs2-sys.
//!
//! Compiles the C++ TJS2 scripting VM from the krkr2 reference checkout
//! directly with a C++ compiler (default: `zig c++`, no cmake), plus the C ABI
//! shim in `cpp/tjs2_abi.cpp`, and statically links the result into the Rust
//! binary.
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
//!   5. compile all tjs2 `.cpp` sources + the shim with the C++ compiler
//!      (`-std=c++17 -fPIC`, defines matching the upstream CMake)
//!   6. archive into `libtjs2_core.a` and emit `cargo:rustc-link-*` for
//!      libonig.a and the C++ runtime (libc++).
//!
//! Cross-compilation
//! -----------------
//! Everything the build needs to target another OS/CPU is taken from the
//! environment; no target-specific paths are hard-coded. These environment
//! variables are honoured (each resolved through the conventional Cargo forms
//! `<VAR>_<target>`, `TARGET_<VAR>`, `<VAR>`, where `<target>` is Cargo's
//! `TARGET` with `-` replaced by `_`, e.g. `CC_aarch64_linux_android`):
//!
//! * `CC` — C compiler used to build oniguruma from source. Supplying `CC`
//!   switches oniguruma off the zig path: `zig build` is still run to fetch
//!   dependencies and stage the header-only ones (fmt/spdlog/boost) plus
//!   `oniguruma.h`, but with `-Dskip-onig` so zig does not try to compile C
//!   for a target it cannot provide a libc for. The oniguruma C sources are
//!   compiled directly with `CC` into `$OUT_DIR/zig-out/lib/libonig.a` and a
//!   target-aware `config.h` is generated into `$OUT_DIR/zig-out/include`.
//! * `CXX` — C++ compiler for the TJS2 sources (default: `zig c++`).
//! * `AR` — archiver used for `libonig.a` and `libtjs2_core.a` (default:
//!   `ar`).
//!
//! When `CC` is not set the original `zig build` path is used unchanged.
//!
//! Finally, after compiling the C++ the resulting objects are checked against
//! the Cargo target's ELF architecture. A compiler whose default target is the
//! build host (i.e. one that was never told the target) is rejected with an
//! actionable error instead of silently producing host objects.

use std::env;
use std::path::{Path, PathBuf};
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

/// The oniguruma C sources, mirroring the list in `deps/build.zig`. The
/// `*_data.c` files are `#include`d by `unicode.c` and must not be compiled
/// standalone.
const ONIG_SOURCES: &[&str] = &[
    // core
    "regcomp.c",
    "regenc.c",
    "regerror.c",
    "regexec.c",
    "regext.c",
    "regparse.c",
    "regposerr.c",
    "regposix.c",
    "regsyntax.c",
    "regtrav.c",
    "regversion.c",
    "st.c",
    "unicode.c",
    // encodings
    "ascii.c",
    "big5.c",
    "cp1251.c",
    "euc_jp.c",
    "euc_kr.c",
    "euc_tw.c",
    "gb18030.c",
    "koi8.c",
    "koi8_r.c",
    "sjis.c",
    "utf16_be.c",
    "utf16_le.c",
    "utf32_be.c",
    "utf32_le.c",
    "utf8.c",
    "iso8859_1.c",
    "iso8859_2.c",
    "iso8859_3.c",
    "iso8859_4.c",
    "iso8859_5.c",
    "iso8859_6.c",
    "iso8859_7.c",
    "iso8859_8.c",
    "iso8859_9.c",
    "iso8859_10.c",
    "iso8859_11.c",
    "iso8859_13.c",
    "iso8859_14.c",
    "iso8859_15.c",
    "iso8859_16.c",
    // unicode key tables (separate TUs)
    "unicode_fold1_key.c",
    "unicode_fold2_key.c",
    "unicode_fold3_key.c",
    "unicode_unfold_key.c",
];

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

/// Resolve a toolchain override from the environment, honouring the
/// conventional Cargo forms for the current `TARGET`:
/// `<VAR>_<target>`, `TARGET_<VAR>`, `<VAR>` (in that order), where `<target>`
/// is `TARGET` with `-` replaced by `_`.
///
/// The value is split on whitespace so a command line such as
/// `CC="ccache aarch64-linux-gnu-gcc"` works.
fn resolve_tool(var: &str, target: &str) -> Option<Vec<String>> {
    let target_specific = format!("{var}_{}", target.replace(['-', '.'], "_"));
    let keys = [target_specific, format!("TARGET_{var}"), var.to_string()];
    for key in keys {
        let Ok(value) = env::var(&key) else { continue };
        if value.trim().is_empty() {
            continue;
        }
        return Some(value.split_whitespace().map(str::to_string).collect());
    }
    None
}

/// Locate the oniguruma source directory inside the zig package cache
/// (`deps/zig-pkg/<pkg>/src`), which `zig build` populates.
fn find_onig_src(deps_dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(deps_dir.join("zig-pkg")).ok()?;
    for entry in entries.flatten() {
        let src = entry.path().join("src");
        if src.join("oniguruma.h").is_file() {
            return Some(src);
        }
    }
    None
}

/// Write oniguruma's `config.h` for the Cargo target. Sizes come from Cargo's
/// `CARGO_CFG_TARGET_*` variables (never from the build host); the `HAVE_*`
/// feature macros follow the target family (unix vs. windows).
fn write_onig_config(include_dir: &Path) {
    let ptr_bytes = env::var("CARGO_CFG_TARGET_POINTER_WIDTH")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .map(|bits| bits / 8)
        .unwrap_or(8);
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_family = env::var("CARGO_CFG_TARGET_FAMILY").unwrap_or_default();
    let unix = target_family.split(',').any(|f| f.trim() == "unix")
        || (target_family.is_empty() && target_os != "windows");
    let sys = if unix { 1 } else { 0 };
    let long_bytes = if target_os == "windows" || ptr_bytes == 4 {
        4
    } else {
        8
    };
    let config = format!(
        "/* Generated by tjs2-sys/build.rs for Cargo target `{target}`. */\n\
         #define HAVE_ALLOCA 1\n\
         /* #undef HAVE_ALLOCA_H */\n\
         #define HAVE_STDINT_H 1\n\
         #define HAVE_SYS_TIMES_H {sys}\n\
         #define HAVE_SYS_TIME_H {sys}\n\
         #define HAVE_SYS_TYPES_H {sys}\n\
         #define HAVE_UNISTD_H {sys}\n\
         #define HAVE_INTTYPES_H 1\n\
         #define PACKAGE \"oniguruma\"\n\
         #define PACKAGE_VERSION \"6.9.10\"\n\
         #define SIZEOF_INT 4\n\
         #define SIZEOF_LONG {long_bytes}\n\
         #define SIZEOF_LONG_LONG 8\n\
         #define SIZEOF_VOIDP {ptr_bytes}\n\
         /* #undef USE_CRNL_AS_LINE_TERMINATOR */\n\
         #define VERSION \"6.9.10\"\n",
        target = env::var("TARGET").unwrap_or_default(),
    );
    std::fs::write(include_dir.join("config.h"), config).unwrap();
}

/// Compile oniguruma's C sources with the supplied C compiler and archive them
/// into `$OUT_DIR/zig-out/lib/libonig.a`. Returns one object file so the caller
/// can verify its architecture.
fn build_onig_direct(
    cc: &[String],
    ar: &[String],
    onig_src: &Path,
    deps_out: &Path,
    profile: &str,
) -> PathBuf {
    let include_dir = deps_out.join("include");
    let lib_dir = deps_out.join("lib");
    let obj_dir = deps_out.join("onig-obj");
    std::fs::create_dir_all(&include_dir).unwrap();
    std::fs::create_dir_all(&lib_dir).unwrap();
    std::fs::create_dir_all(&obj_dir).unwrap();

    write_onig_config(&include_dir);
    // `zig build` also installs this, but be explicit: this path must not
    // depend on that detail.
    std::fs::copy(
        onig_src.join("oniguruma.h"),
        include_dir.join("oniguruma.h"),
    )
    .unwrap_or_else(|e| fail(&format!("failed to install oniguruma.h: {e}")));

    let opt = if profile == "release" { "-O2" } else { "-g" };
    let flags = [
        format!("-I{}", onig_src.display()),
        format!("-I{}", include_dir.display()),
        "-fPIC".to_string(),
    ];

    let mut objects = Vec::with_capacity(ONIG_SOURCES.len());
    for name in ONIG_SOURCES {
        let src = onig_src.join(name);
        let obj = obj_dir.join(name.replace(".c", ".o"));
        let mut cmd = Command::new(&cc[0]);
        cmd.args(&cc[1..])
            .args(&flags)
            .arg(opt)
            .arg("-c")
            .arg(&src)
            .arg("-o")
            .arg(&obj);
        run(&mut cmd, &format!("compile oniguruma {name}"));
        objects.push(obj);
    }

    let lib = lib_dir.join("libonig.a");
    let _ = std::fs::remove_file(&lib);
    let mut ar_cmd = Command::new(&ar[0]);
    ar_cmd.args(&ar[1..]).arg("crus").arg(&lib).args(&objects);
    run(&mut ar_cmd, "archive libonig.a");

    objects
        .into_iter()
        .next()
        .unwrap_or_else(|| fail("no oniguruma objects were produced"))
}

/// The ELF `e_machine` value for a Cargo target architecture, or `None` if the
/// target does not use ELF (e.g. wasm) and the check should be skipped.
fn expected_elf_machine(target_arch: &str) -> Option<u16> {
    Some(match target_arch {
        "x86" => 0x03,                                       // EM_386
        "x86_64" => 0x3e,                                    // EM_X86_64
        "arm" => 0x28,                                       // EM_ARM
        "aarch64" => 0xb7,                                   // EM_AARCH64
        "riscv32" | "riscv64" => 0xf3,                       // EM_RISCV
        "powerpc" => 0x14,                                   // EM_PPC
        "powerpc64" => 0x15,                                 // EM_PPC64
        "s390x" => 0x16,                                     // EM_S390
        "sparc64" => 0x2b,                                   // EM_SPARCV9
        "loongarch64" => 0x102,                              // EM_LOONGARCH
        "mips" | "mips64" | "mips32r6" | "mips64r6" => 0x08, // EM_MIPS
        _ => return None,
    })
}

/// Verify that a compiled object matches the Cargo target. This makes a
/// "toolchain that silently defaulted to the host" impossible to miss.
fn check_object_arch(obj: &Path, target: &str, target_arch: &str) {
    let Some(expected) = expected_elf_machine(target_arch) else {
        return;
    };
    let data = std::fs::read(obj)
        .unwrap_or_else(|e| fail(&format!("failed to read object {}: {e}", obj.display())));
    // Not an ELF object (COFF/PE or wasm): nothing for this guard to say.
    if data.len() < 20 || &data[0..4] != b"\x7fELF" {
        return;
    }
    let little_endian = data[5] == 1;
    let machine = if little_endian {
        u16::from_le_bytes([data[18], data[19]])
    } else {
        u16::from_be_bytes([data[18], data[19]])
    };
    if machine != expected {
        fail(&format!(
            "architecture mismatch: `{}` is an ELF object for machine {machine:#06x}, but the Cargo \
             target `{target}` ({target_arch}) needs machine {expected:#06x}. The C/C++ compiler was \
             not told to target `{target}` and defaulted to the build host. Point the build at a \
             cross toolchain, e.g. `CC_<target>`, `TARGET_CC` or `CC` (and likewise `CXX`), where \
             `<target>` is `{target}` with `-` replaced by `_`.",
            obj.display(),
        ));
    }
}

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());

    let target = env::var("TARGET").unwrap_or_default();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    // -- zig ----------------------------------------------------------------
    let zig = env::var("ZIG")
        .ok()
        .or_else(|| which("zig").map(|p| p.display().to_string()));
    let Some(zig) = zig else {
        fail("zig not found (needed to build C++ deps and compile tjs2). Install it or set ZIG.");
    };
    println!("cargo:rerun-if-env-changed=ZIG");
    // `TJS2_OPT` selects the vendored-C++ optimisation level; changing it must
    // trigger a rebuild of the static library.
    println!("cargo:rerun-if-env-changed=TJS2_OPT");

    // -- toolchain ----------------------------------------------------------
    // The C compiler is what switches oniguruma off the zig path; see the
    // module docs.
    let cc = resolve_tool("CC", &target);
    let cxx_env = resolve_tool("CXX", &target);
    let cxx_explicit = cxx_env.is_some();
    let cxx: Vec<String> = cxx_env.unwrap_or_else(|| vec![zig.clone(), "c++".into()]);
    let ar: Vec<String> = resolve_tool("AR", &target).unwrap_or_else(|| vec!["ar".into()]);
    let target_suffix = target.replace(['-', '.'], "_");
    for base in ["CC", "CXX", "AR"] {
        println!("cargo:rerun-if-env-changed={base}_{target_suffix}");
        println!("cargo:rerun-if-env-changed=TARGET_{base}");
        println!("cargo:rerun-if-env-changed={base}");
    }

    // Build C++ deps: fmt/spdlog/boost headers + libonig.a. When `CC` is set we
    // ask the zig project for headers only and build libonig.a ourselves, so
    // zig never has to find a libc for the cross target.
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
        .arg(format!("-Doptimize={zig_optimize}"));
    if cc.is_some() {
        cmd.arg("-Dskip-onig=true");
    }
    cmd.current_dir(&deps_dir);
    run(&mut cmd, "zig build (deps)");
    println!(
        "cargo:rerun-if-changed={}",
        deps_dir.join("build.zig").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        deps_dir.join("build.zig.zon").display()
    );

    // Build oniguruma directly when a C compiler was supplied.
    let onig_probe: Option<PathBuf> = cc.as_ref().map(|cc| {
        let onig_src = find_onig_src(&deps_dir).unwrap_or_else(|| {
            fail(
                "oniguruma sources not found under deps/zig-pkg — `zig build` should have fetched \
                 them; run `zig build` in deps/ and retry",
            )
        });
        build_onig_direct(cc, &ar, &onig_src, &deps_out, &profile)
    });

    // -- locate the tjs2 sources (vendored copy, patched for krkr-rs) -------
    let tjs2_dir = manifest_dir.join("cpp/tjs2");
    if !tjs2_dir.join("tjs.cpp").is_file() {
        fail(&format!(
            "vendored tjs2 sources not found at {} — run `./scripts/vendor-tjs2.sh` to (re)vendor from the reference checkout",
            tjs2_dir.display()
        ));
    }
    println!("cargo:rerun-if-changed={}", tjs2_dir.display());

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
        // The vendored C++ is now safe to optimise because the latent UB
        // sites were fixed; see the optimisation-level comment below.
        flags.push("-DNDEBUG".into());
    } else {
        flags.push("-g".into());
        flags.push("-D_DEBUG".into());
        flags.push("-DDEBUG".into());
    }

    // krkr-rs: optimisation level for the vendored C++ VM.
    //
    // History: upstream tjs2 reads uninitialised VM register slots in the
    // exception-display dump and performs member calls on a null
    // `tTJSVariantString` `this`. Both are UB that clang >= -O1 exploits
    // (the null-`this` guards are removed, then a null string pointer is
    // dereferenced). Both classes are now fixed in the vendored sources
    // (`tjsInterCodeExec.cpp` zeroes the register area before the dump; the
    // null-string call sites in `tjsString.h/.cpp`, `tjsVariant.h/.cpp` and
    // `tjsInterCodeExec.cpp` check for the canonical null empty-string pointer
    // first), so the whole VM runs at -O2.
    //
    // This matters because save loading parses/evaluates a multi-megabyte TJS
    // literal: measured on the real game, `-O0` spends ~670 ms in parse+eval
    // while `-O2` spends ~136 ms. Every translation unit (upstream
    // sources, generated parsers, ABI/stream shims) uses this same level — no
    // per-file exception is needed. If a future regression is traced to one
    // TU, special-case its file name here. `TJS2_OPT` overrides the level for
    // benchmarking.
    let opt = env::var("TJS2_OPT").unwrap_or_else(|_| "-O2".to_string());

    // Collect all sources: upstream tjs2 + generated parsers + our shim.
    let mut sources: Vec<PathBuf> = TJS2_SOURCES.iter().map(|s| tjs2_dir.join(s)).collect();
    for y in BISON_GRAMMARS {
        let stem = y.strip_suffix(".y").unwrap();
        sources.push(gen_dir.join(format!("{stem}.tab.cpp")));
    }
    sources.push(manifest_dir.join("cpp/tjs2_abi.cpp"));
    sources.push(manifest_dir.join("cpp/streams.cpp"));

    let failed = std::sync::Arc::new(AtomicUsize::new(0));
    let lock = std::sync::Arc::new(Mutex::new(()));
    let handles: Vec<_> = sources
        .into_iter()
        .map(|src| {
            let cxx = cxx.clone();
            let flags = flags.clone();
            let opt = opt.clone();
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
                let mut cmd = Command::new(&cxx[0]);
                cmd.args(&cxx[1..])
                    .args(&flags)
                    .arg(&opt)
                    .arg("-c")
                    .arg(&src)
                    .arg("-o")
                    .arg(&obj);
                if !cxx_explicit {
                    // The default `zig c++` locates the host libc via `$CC`; a
                    // cross `CC` left in the environment makes it abort with
                    // `LibCRuntimeNotFound` instead of compiling.
                    cmd.env_remove("CC");
                }
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

    // -- architecture guard -------------------------------------------------
    // Never let a compiler that defaulted to the host produce a "successful"
    // build of host objects for a foreign target.
    let cxx_probe = obj_dir.join("tjsInterCodeExec.o");
    if cxx_probe.is_file() {
        check_object_arch(&cxx_probe, &target, &target_arch);
    }
    if let Some(probe) = &onig_probe {
        check_object_arch(probe, &target, &target_arch);
    }

    // -- archive into a static lib ------------------------------------------
    let lib = out_dir.join("libtjs2_core.a");
    let objects: Vec<PathBuf> = std::fs::read_dir(&obj_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "o").unwrap_or(false))
        .collect();
    let _ = std::fs::remove_file(&lib);
    let mut ar_cmd = Command::new(&ar[0]);
    ar_cmd.args(&ar[1..]).arg("crus").arg(&lib).args(&objects);
    run(&mut ar_cmd, "ar");

    // -- link directives -----------------------------------------------------
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=tjs2_core");
    println!(
        "cargo:rustc-link-search=native={}",
        deps_out.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=onig");
    // C++ runtime: tjs2 was compiled with zig's clang/libc++ on the desktop,
    // so those flags are only correct when the *target* (not the build host) is
    // desktop Linux. For a cross target the runtime must come from that
    // target's own toolchain instead.
    if target_os == "linux" {
        println!("cargo:rustc-link-lib=dylib=c++");
        println!("cargo:rustc-link-lib=dylib=c++abi");
        println!("cargo:rustc-link-arg=-pthread");
    }
}
