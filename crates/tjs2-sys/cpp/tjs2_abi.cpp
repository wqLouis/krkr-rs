// C ABI shim over the C++ TJS2 scripting VM (tjs2-sys).
//
// This is the only C++ code krkr-rs compiles beyond the tjs2 core itself.
// It keeps the Rust side free of tjs2 C++ headers; everything crosses the
// boundary as C types + UTF-8 strings.

#include "tjsCommHead.h"

#include <cstdlib>
#include <cstring>
#include <new>
#include <string>

#include "tjs.h"
#include "tjsString.h"
#include "tjsError.h"
#include "tjsVariant.h"

#include "tjs2_abi.h"

#include <spdlog/spdlog.h>
#include <spdlog/sinks/stdout_color_sinks.h>

// The reference main() registers the spdlog loggers the tjs2 code expects
// (`spdlog::get("tjs2")` etc.). Register them once per process, routing to
// stderr. (Routing into the per-engine log callback can come later.)
static void ensure_spdlog_loggers() {
    static std::once_flag once;
    std::call_once(once, [] {
        spdlog::set_level(spdlog::level::debug);
        if(!spdlog::get("tjs2"))
            spdlog::stderr_color_mt("tjs2");
        if(!spdlog::get("core"))
            spdlog::stderr_color_mt("core");
        if(!spdlog::get("plugin"))
            spdlog::stderr_color_mt("plugin");
        spdlog::set_default_logger(spdlog::get("core"));
    });
}

// Provided by the environ module in the full engine; we have no locale
// catalog yet, so fall back to returning the key itself.
ttstr TVPGetMessageByLocale(const std::string &key) {
    std::u16string u = boost::locale::conv::utf_to_utf<char16_t>(key);
    return ttstr(u.c_str());
}

// ---------------------------------------------------------------------------
// engine holder
// ---------------------------------------------------------------------------

struct tjs2_engine {
    TJS::tTJS *inner;
    tjs2_log_cb log_cb;
    void *log_user;
    std::string last_string; // storage for the last string result
};

namespace {

// Convert UTF-8 -> tjs_char (char16_t on this build).
std::u16string utf8_to_u16(const char *s) {
    return boost::locale::conv::utf_to_utf<char16_t>(s);
}

// Convert tjs_char -> UTF-8 std::string.
std::string u16_to_utf8(const tjs_char *s) {
    ttstr tmp(s);
    return tmp.AsStdString();
}

// Copy the value of a tTJSVariant into a tjs2_value; string results are
// stored in the engine's last_string buffer.
void variant_to_value(tjs2_engine *e, const TJS::tTJSVariant &v,
                      tjs2_value *out) {
    if(!out)
        return;
    out->integer = 0;
    out->real = 0.0;
    out->string = nullptr;
    switch(v.Type()) {
        case tvtVoid:
            out->type = TJS2_VAL_VOID;
            break;
        case tvtInteger:
            out->type = TJS2_VAL_INTEGER;
            out->integer = (long long)v.AsInteger();
            break;
        case tvtReal:
            out->type = TJS2_VAL_REAL;
            out->real = v.AsReal();
            break;
        case tvtString: {
            out->type = TJS2_VAL_STRING;
            ttstr s(v);
            e->last_string = s.AsStdString();
            out->string = e->last_string.c_str();
            break;
        }
        default:
            out->type = TJS2_VAL_OBJECT;
            break;
    }
}

// Convert a TJS exception into a malloc'd UTF-8 message (or fall back to a
// generic message on unexpected error types).
char *make_error_message(const TJS::eTJS &e) {
    try {
        std::string msg = e.getMessage().AsStdString();
        char *buf = (char *)malloc(msg.size() + 1);
        if(!buf)
            return nullptr;
        std::memcpy(buf, msg.c_str(), msg.size() + 1);
        return buf;
    } catch(...) {
        char *buf = (char *)malloc(8);
        if(!buf)
            return nullptr;
        std::memcpy(buf, "TJS error", 10); // includes NUL
        return buf;
    }
}

} // namespace

// ---------------------------------------------------------------------------
// console output adapter
// ---------------------------------------------------------------------------

namespace {

class ConsoleOutputAdapter : public TJS::iTJSConsoleOutput {
    tjs2_engine *e_;

public:
    explicit ConsoleOutputAdapter(tjs2_engine *e) : e_(e) {}

    void ExceptionPrint(const tjs_char *msg) override {
        if(e_->log_cb) {
            std::string s = u16_to_utf8(msg);
            e_->log_cb(TJS2_LOG_ERROR, s.c_str(), e_->log_user);
        }
    }

    void Print(const tjs_char *msg) override {
        if(e_->log_cb) {
            std::string s = u16_to_utf8(msg);
            e_->log_cb(TJS2_LOG_INFO, s.c_str(), e_->log_user);
        }
    }
};

} // namespace

// ---------------------------------------------------------------------------
// C ABI
// ---------------------------------------------------------------------------

extern "C" {

tjs2_engine *tjs2_create(void) {
    ensure_spdlog_loggers();
    try {
        tjs2_engine *e = new (std::nothrow) tjs2_engine;
        if(!e)
            return nullptr;
        e->inner = new (std::nothrow) TJS::tTJS();
        if(!e->inner) {
            delete e;
            return nullptr;
        }
        e->log_cb = nullptr;
        e->log_user = nullptr;
        e->inner->SetConsoleOutput(new ConsoleOutputAdapter(e));
        return e;
    } catch(...) {
        return nullptr;
    }
}

void tjs2_destroy(tjs2_engine *e) {
    if(!e)
        return;
    try {
        e->inner->Shutdown();
        e->inner->Release(); // refcounted; ~tTJS is protected
    } catch(...) {
    }
    delete e;
}

void tjs2_set_log_cb(tjs2_engine *e, tjs2_log_cb cb, void *user) {
    if(!e)
        return;
    e->log_cb = cb;
    e->log_user = user;
}

int tjs2_exec_script(tjs2_engine *e, const char *script, const char *name,
                     tjs2_value *out_result, char **out_error) {
    if(!e || !script) {
        if(out_error)
            *out_error = nullptr;
        return -1;
    }
    try {
        TJS::tTJSVariant result;
        std::u16string s = utf8_to_u16(script);
        std::u16string n = name ? utf8_to_u16(name) : std::u16string();
        e->inner->ExecScript(s.c_str(), &result, nullptr, n.c_str(), 0);
        variant_to_value(e, result, out_result);
        if(out_error)
            *out_error = nullptr;
        return 0;
    } catch(const TJS::eTJS &err) {
        if(out_error)
            *out_error = make_error_message(err);
        return 1;
    } catch(const std::exception &err) {
        if(out_error) {
            std::string m = std::string("C++ exception: ") + err.what();
            char *buf = (char *)malloc(m.size() + 1);
            if(buf)
                std::memcpy(buf, m.c_str(), m.size() + 1);
            *out_error = buf;
        }
        return 1;
    } catch(...) {
        if(out_error) {
            char *buf = (char *)malloc(8);
            if(buf)
                std::memcpy(buf, "unknown", 8);
            *out_error = buf;
        }
        return 1;
    }
}

int tjs2_eval(tjs2_engine *e, const char *expression, const char *name,
              tjs2_value *out_result, char **out_error) {
    if(!e || !expression) {
        if(out_error)
            *out_error = nullptr;
        return -1;
    }
    try {
        TJS::tTJSVariant result;
        std::u16string s = utf8_to_u16(expression);
        std::u16string n = name ? utf8_to_u16(name) : std::u16string();
        e->inner->EvalExpression(s.c_str(), &result, nullptr, n.c_str(), 0);
        variant_to_value(e, result, out_result);
        if(out_error)
            *out_error = nullptr;
        return 0;
    } catch(const TJS::eTJS &err) {
        if(out_error)
            *out_error = make_error_message(err);
        return 1;
    } catch(const std::exception &err) {
        if(out_error) {
            std::string m = std::string("C++ exception: ") + err.what();
            char *buf = (char *)malloc(m.size() + 1);
            if(buf)
                std::memcpy(buf, m.c_str(), m.size() + 1);
            *out_error = buf;
        }
        return 1;
    } catch(...) {
        if(out_error) {
            char *buf = (char *)malloc(8);
            if(buf)
                std::memcpy(buf, "unknown", 8);
            *out_error = buf;
        }
        return 1;
    }
}

void tjs2_free_string(char *s) {
    free(s);
}

} // extern "C"
