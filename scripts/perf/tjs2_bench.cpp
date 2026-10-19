// Standalone TJS2 VM benchmark driver (krkr-rs build-level perf work).
// Not linked into the product; used only via scripts/perf/bench-tjs-opt.sh.
#include "tjs2_abi.h"

#include <algorithm>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <sstream>
#include <string>
#include <vector>

namespace fs = std::filesystem;
using steady_ms_clock = std::chrono::steady_clock;

static double now_ms() {
    return std::chrono::duration<double, std::milli>(
               steady_ms_clock::now().time_since_epoch())
        .count();
}

static bool read_file(const std::string &path, std::string &out) {
    std::ifstream in(path, std::ios::binary);
    if (!in)
        return false;
    std::ostringstream ss;
    ss << in.rdbuf();
    out = ss.str();
    return true;
}

static void report_error(const char *what, const std::string &name, char *err) {
    std::fprintf(stderr, "ERROR %s %s: %s\n", what, name.c_str(),
                 err ? err : "(no message)");
    if (err)
        tjs2_free_string(err);
}

// Compile every *.tjs / *.ks under `dir` (recursively) `iters` times.
static int cmd_compile(tjs2_engine *e, const std::string &dir, int iters,
                       bool tjs_only) {
    std::vector<std::string> files;
    for (auto &p : fs::recursive_directory_iterator(dir)) {
        if (!p.is_regular_file())
            continue;
        std::string ext = p.path().extension().string();
        bool match = tjs_only ? ext == ".tjs" : (ext == ".tjs" || ext == ".ks");
        if (match)
            files.push_back(p.path().string());
    }
    std::sort(files.begin(), files.end());
    const fs::path outdir = fs::temp_directory_path() / "tjs-bench-bytecode";
    fs::create_directories(outdir);

    double total = 0.0;
    int ok = 0, fail = 0;
    for (int it = 0; it < iters; ++it) {
        double t0 = now_ms();
        for (auto &f : files) {
            std::string src;
            if (!read_file(f, src))
                continue;
            std::string out =
                (outdir / (fs::path(f).filename().string() + ".tjsb")).string();
            char *err = nullptr;
            int rc = tjs2_compile_script(e, src.c_str(), out.c_str(), 0, 0, 0,
                                         f.c_str(), 0, &err);
            if (rc != 0) {
                if (it == 0) {
                    ++fail;
                    report_error("compile", f, err);
                } else if (err) {
                    tjs2_free_string(err);
                }
            } else if (it == 0) {
                ++ok;
            }
        }
        total += now_ms() - t0;
    }
    std::printf("compile files=%zu ok=%d fail=%d iters=%d "
                "avg_ms=%.3f total_ms=%.3f\n",
                files.size(), ok, fail, iters, total / iters, total);
    return fail == 0 ? 0 : 2;
}

// Execute one script (or a literal with `--raw`) `iters` times.
static int cmd_exec(tjs2_engine *e, const std::string &arg, int iters,
                    bool raw, const char *check) {
    std::string src;
    if (raw)
        src = arg;
    else if (!read_file(arg, src)) {
        std::fprintf(stderr, "cannot read %s\n", arg.c_str());
        return 1;
    }
    double best = 1e300, total = 0.0;
    int rc = 0;
    for (int i = 0; i < iters; ++i) {
        tjs2_value v;
        std::memset(&v, 0, sizeof(v));
        char *err = nullptr;
        double t0 = now_ms();
        rc = tjs2_exec_script(e, src.c_str(), arg.c_str(), nullptr, 0, &v, &err);
        double dt = now_ms() - t0;
        best = std::min(best, dt);
        total += dt;
        if (rc != 0)
            report_error("exec", arg, err);
        else if (err)
            tjs2_free_string(err);
    }
    std::printf("exec iters=%d avg_ms=%.3f best_ms=%.3f last_rc=%d\n", iters,
                total / iters, best, rc);
    if (rc == 0 && check && *check) {
        tjs2_value v;
        std::memset(&v, 0, sizeof(v));
        char *err = nullptr;
        if (tjs2_eval(e, check, "<check>", nullptr, 0, &v, &err) == 0) {
            if (v.type == TJS2_VAL_INTEGER)
                std::printf("check %s = %lld\n", check, v.integer);
            else if (v.type == TJS2_VAL_REAL)
                std::printf("check %s = %.17g\n", check, v.real);
            else if (v.type == TJS2_VAL_STRING)
                std::printf("check %s = \"%s\"\n", check,
                            v.string ? v.string : "");
            else
                std::printf("check %s = type %d\n", check, v.type);
        } else {
            report_error("check", check, err);
        }
    }
    return rc == 0 ? 0 : 2;
}

int main(int argc, char **argv) {
    if (argc < 3) {
        std::fprintf(stderr,
                     "usage: %s compile <dir> [iters] [--tjs-only] |\n"
                     "       exec <file> [iters] | exec-raw <script> [iters]\n",
                     argv[0]);
        return 2;
    }
    tjs2_engine *e = tjs2_create();
    if (!e) {
        std::fprintf(stderr, "tjs2_create failed\n");
        return 1;
    }
    int rc = 0;
    std::string mode = argv[1];
    if (mode == "compile") {
        int iters = 1;
        bool tjs_only = false;
        for (int i = 3; i < argc; ++i) {
            std::string a = argv[i];
            if (a == "--tjs-only")
                tjs_only = true;
            else
                iters = std::atoi(a.c_str());
        }
        rc = cmd_compile(e, argv[2], iters, tjs_only);
    } else if (mode == "exec") {
        rc = cmd_exec(e, argv[2], argc > 3 ? std::atoi(argv[3]) : 1, false,
                      argc > 4 ? argv[4] : nullptr);
    } else if (mode == "exec-raw") {
        int iters = argc > 3 ? std::atoi(argv[3]) : 1;
        rc = cmd_exec(e, argv[2], iters, true,
                      argc > 4 ? argv[4] : nullptr);
    } else {
        std::fprintf(stderr, "unknown mode %s\n", mode.c_str());
        rc = 2;
    }
    tjs2_destroy(e);
    return rc;
}
