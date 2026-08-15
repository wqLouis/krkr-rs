//! Dependency preparation for krkr-rs.
//!
//! Fetches and stages the C/C++ dependencies needed by the C++ TJS2 VM:
//!
//!   * fmt        — headers only (FMT_HEADER_ONLY), pin 10.x (C++17-compatible)
//!   * spdlog     — headers only (header-only mode)
//!   * boost      — headers only (boost::locale utf conversion is header-only)
//!   * oniguruma  — compiled into libonig.a (tjs2 regular expressions)
//!
//! Output (`zig-out/`):
//!   include/  — fmt/, spdlog/, boost/ and oniguruma.h
//!   lib/libonig.a
//!
//! build.rs invokes `zig build --prefix <OUT_DIR>/zig-out` from this
//! directory; nothing here is compiled into the final binary directly
//! besides libonig.a (linked statically).

const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    // ---- headers ---------------------------------------------------------
    const fmt = b.dependency("fmt", .{});
    b.installDirectory(.{
        .source_dir = fmt.path("include"),
        .install_dir = .header,
        .install_subdir = "",
    });

    const spdlog = b.dependency("spdlog", .{});
    b.installDirectory(.{
        .source_dir = spdlog.path("include"),
        .install_dir = .header,
        .install_subdir = "",
    });

    const boost = b.dependency("boost", .{});
    b.installDirectory(.{
        .source_dir = boost.path("boost"),
        .install_dir = .header,
        .install_subdir = "boost",
    });

    const onig = b.dependency("oniguruma", .{});
    const onig_header =
        b.addInstallFileWithDir(onig.path("src/oniguruma.h"), .header, "oniguruma.h");
    b.getInstallStep().dependOn(&onig_header.step);

    // Generate `config.h` from the upstream cmake template with target-aware
    // C type sizes (oniguruma includes it unconditionally).
    const ptr_bytes = target.result.ptrBitWidth() / 8;
    const long_bytes: i64 = if (target.result.os.tag == .windows) 4 else ptr_bytes;
    const config_header = b.addConfigHeader(
        .{
            .style = .{ .cmake = onig.path("src/config.h.cmake.in") },
            .include_path = "config.h",
        },
        .{
            .CRAY_STACKSEG_END = false,
            .C_ALLOCA = false,
            .HAVE_ALLOCA = true,
            .HAVE_ALLOCA_H = false,
            .HAVE_STDINT_H = true,
            .HAVE_SYS_TIMES_H = true,
            .HAVE_SYS_TIME_H = true,
            .HAVE_SYS_TYPES_H = true,
            .HAVE_UNISTD_H = true,
            .HAVE_INTTYPES_H = true,
            .PACKAGE = "oniguruma",
            .PACKAGE_VERSION = "6.9.10",
            .SIZEOF_INT = 4,
            .SIZEOF_LONG = long_bytes,
            .SIZEOF_LONG_LONG = 8,
            .SIZEOF_VOIDP = ptr_bytes,
            .USE_CRNL_AS_LINE_TERMINATOR = false,
            .VERSION = "6.9.10",
        },
    );

    // ---- oniguruma static library ---------------------------------------
    const onig_sources = [_][]const u8{
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
        // unicode key tables (separate TUs; the *_data.c files are
        // #included by unicode.c and must NOT be compiled standalone)
        "unicode_fold1_key.c",
        "unicode_fold2_key.c",
        "unicode_fold3_key.c",
        "unicode_unfold_key.c",
    };

    const onig_module = b.createModule(.{
        .target = target,
        .optimize = optimize,
        .link_libc = true,
        // zig enables UBSan for C in Debug; the final link is driven by
        // rustc so the sanitizer runtime must not be referenced.
        .sanitize_c = .off,
    });
    onig_module.addIncludePath(onig.path("src"));
    onig_module.addConfigHeader(config_header);
    onig_module.addCSourceFiles(.{
        .root = onig.path("src"),
        .files = &onig_sources,
        .flags = &.{},
    });

    const onig_lib = b.addLibrary(.{
        .name = "onig",
        .linkage = .static,
        .root_module = onig_module,
    });
    b.installArtifact(onig_lib);
}
