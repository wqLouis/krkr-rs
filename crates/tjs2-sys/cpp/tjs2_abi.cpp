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
#include <vector>

#include "tjs.h"
#include "tjsString.h"
#include "tjsError.h"
#include "tjsVariant.h"
#include "tjsNative.h"

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
    // Registered native classes. Each class object is held with one AddRef
    // (independent of the reference the global object keeps) so it stays
    // alive even if a script deletes the global member early, and is
    // released exactly once by tjs2_destroy. Parallel to
    // native_class_names (used for duplicate registration checks).
    std::vector<TJS::iTJSDispatch2 *> native_classes;
    std::vector<std::string> native_class_names;
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

// ---------------------------------------------------------------------------
// native method dispatch
// ---------------------------------------------------------------------------

class tjs2_native_method_dispatch;

// Forward declaration: the dispatch class below calls this from its FuncCall
// body (compiled as if right after the class, before this definition).
tjs_error tjs2_dispatch_native_method(tjs2_native_method_dispatch *self,
                                      tTJSVariant *result, tjs_int numparams,
                                      tTJSVariant **param,
                                      iTJSDispatch2 *objthis);

// A per-method dispatch object. The tTJSNativeClassMethodCallback signature
// (tjsNative.h:55) has no slot for the engine or the Rust callback, so each
// registered method gets its own object carrying that state; FuncCall
// (virtual, called by the VM when scripts invoke the method) dispatches to
// the Rust callback. This mirrors what TJS_END_NATIVE_METHOD_DECL expands
// to (TJSCreateNativeClassMethod + RegisterNCM), but with per-method state.
class tjs2_native_method_dispatch : public TJS::tTJSNativeClassMethod {
    typedef TJS::tTJSNativeClassMethod inherited;

public:
    tjs2_engine *engine;
    tjs2_native_method_fn fn;

    tjs2_native_method_dispatch(tjs2_engine *e, tjs2_native_method_fn f)
        : inherited(nullptr), engine(e), fn(f) {}

    tjs_error FuncCall(tjs_uint32 flag, const tjs_char *membername,
                       tjs_uint32 *hint, tTJSVariant *result,
                       tjs_int numparams, tTJSVariant **param,
                       iTJSDispatch2 *objthis) override {
        if(membername)
            return inherited::FuncCall(flag, membername, hint, result,
                                       numparams, param, objthis);
        return tjs2_dispatch_native_method(this, result, numparams, param,
                                           objthis);
    }
};

// Marshal the tTJSVariant arguments of a native method call into tjs2_value
// entries. String values are copied into `storage` (one std::string per
// argument) so every entry's string pointer stays valid for the duration of
// the call — unlike the single-result path, the engine's last_string buffer
// cannot be reused here because a string argument would be clobbered by the
// next argument's conversion.
void args_to_values(tjs_int numparams, tTJSVariant **param,
                    std::vector<tjs2_value> &out,
                    std::vector<std::string> &storage) {
    out.resize(numparams);
    storage.resize(numparams);
    for(tjs_int i = 0; i < numparams; i++) {
        tjs2_value &v = out[i];
        v.integer = 0;
        v.real = 0.0;
        v.string = nullptr;
        const TJS::tTJSVariant &var = *param[i];
        switch(var.Type()) {
            case tvtVoid:
                v.type = TJS2_VAL_VOID;
                break;
            case tvtInteger:
                v.type = TJS2_VAL_INTEGER;
                v.integer = (long long)var.AsInteger();
                break;
            case tvtReal:
                v.type = TJS2_VAL_REAL;
                v.real = var.AsReal();
                break;
            case tvtString: {
                v.type = TJS2_VAL_STRING;
                ttstr s(var);
                storage[i] = s.AsStdString();
                v.string = storage[i].c_str();
                break;
            }
            default:
                // objects/octets cross the boundary as opaque handles
                v.type = TJS2_VAL_OBJECT;
                break;
        }
    }
}

// Convert a tjs2_value produced by a Rust native method callback into a
// tTJSVariant result. Strings are UTF-8 and owned by the caller for the
// duration of the callback. Throws TJS::eTJSError for types that cannot be
// reconstructed across the ABI boundary.
void value_to_variant(const tjs2_value *in, TJS::tTJSVariant *out) {
    switch(in->type) {
        case TJS2_VAL_VOID:
            out->Clear();
            break;
        case TJS2_VAL_INTEGER:
            *out = (tjs_int64)in->integer;
            break;
        case TJS2_VAL_REAL:
            *out = (tjs_real)in->real;
            break;
        case TJS2_VAL_STRING: {
            const char *s = in->string ? in->string : "";
            ttstr tmp(utf8_to_u16(s).c_str());
            *out = tmp;
            break;
        }
        default:
            // A tjs2_value carries no object handle, so an object result
            // cannot be reconstructed; instance/object semantics are a
            // later milestone.
            throw TJS::eTJSError(ttstr(TJS_W(
                "native method returned an object; object return values "
                "are not supported yet")));
    }
}

// Dispatch a native method call to the Rust callback and convert the result
// back into a tTJSVariant. Throws a TJS exception (catchable from scripts)
// when the Rust callback reports an error or the return type cannot be
// marshaled. C++ exceptions raised anywhere in the VM during dispatch are
// converted to TJS exceptions by TJS_CONVERT_TO_TJS_EXCEPTION.
tjs_error tjs2_dispatch_native_method(tjs2_native_method_dispatch *self,
                                      tTJSVariant *result, tjs_int numparams,
                                      tTJSVariant **param,
                                      iTJSDispatch2 *objthis) {
    tjs2_engine *e = self->engine;
    (void)objthis; // static methods only: no instance semantics yet
    try {
        if(result)
            result->Clear();

        std::vector<tjs2_value> argv;
        std::vector<std::string> arg_storage;
        args_to_values(numparams, param, argv, arg_storage);

        tjs2_value out;
        out.type = TJS2_VAL_VOID;
        out.integer = 0;
        out.real = 0.0;
        out.string = nullptr;

        char *out_error = nullptr;
        // FFI handoff: `fn` is Rust code that must follow the ABI contract
        // (argv/out valid only during the call, out_error malloc'd).
        int rc = self->fn(e, (int)numparams,
                          argv.empty() ? nullptr : argv.data(), &out,
                          &out_error);

        if(rc != 0) {
            std::string msg = out_error ? out_error
                                        : "native method reported an error";
            if(out_error)
                tjs2_free_string(out_error);
            throw TJS::eTJSError(ttstr(utf8_to_u16(msg.c_str()).c_str()));
        }

        if(result)
            value_to_variant(&out, result);
        return TJS_S_OK;
    }
    TJS_CONVERT_TO_TJS_EXCEPTION
}

// RAII holder for the creation reference of a freshly built native class:
// releases it on every failure path, or hands it off on success.
struct native_class_holder {
    TJS::iTJSDispatch2 *p;
    explicit native_class_holder(TJS::iTJSDispatch2 *ptr) : p(ptr) {}
    ~native_class_holder() {
        if(p)
            p->Release();
    }
    void dispose() {
        if(p)
            p->Release();
        p = nullptr;
    }
};

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
    // Release the classes we registered (the global object's refs were
    // dropped by Shutdown's Global->Clear()).
    for(TJS::iTJSDispatch2 *cls : e->native_classes)
        cls->Release();
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

void *tjs2_malloc(size_t size) {
    return malloc(size);
}

int tjs2_register_native_class(tjs2_engine *e, const char *class_name_utf8,
                               const tjs2_native_method *methods, int count) {
    if(!e || !class_name_utf8 || count < 0 || (count > 0 && !methods))
        return -1;
    try {
        // Reject duplicate class names: re-registering would silently
        // replace the global member and confuse the destroy-time registry.
        std::string name8(class_name_utf8);
        for(const auto &existing : e->native_class_names)
            if(existing == name8)
                return -2;

        std::u16string name16 = utf8_to_u16(class_name_utf8);
        ttstr clsname(name16.c_str());

        // Construct the native class. We do not subclass tTJSNativeClass:
        // CreateNativeInstance() stays nullptr (instances would carry no
        // native data) which is fine for static-only methods.
        TJS::tTJSNativeClass *cls = new TJS::tTJSNativeClass(clsname);
        native_class_holder holder(cls);

        // Register each method as a static member of the class. This
        // replicates what TJS_END_NATIVE_STATIC_METHOD_DECL expands to in
        // tjsNative.h:365-372: RegisterNCM(name,
        // TJSCreateNativeClassMethod(callback), classname, nitMethod,
        // TJS_STATICMEMBER). RegisterNCM PropSets the dispatch onto the
        // class and releases the creation reference, so the class owns it.
        for(int i = 0; i < count; i++) {
            const tjs2_native_method &m = methods[i];
            if(!m.name || !m.fn)
                return -3;
            std::u16string mname16 = utf8_to_u16(m.name);
            auto *dsp = new tjs2_native_method_dispatch(e, m.fn);
            cls->RegisterNCM(mname16.c_str(), dsp, clsname.c_str(),
                             TJS::nitMethod, TJS_STATICMEMBER);
        }

        // Attach the class to the global object, following the reference
        // registerObject pattern (ScriptMgnIntf.cpp:476-483): wrap in a
        // variant (AddRef), drop the creation ref, PropSet onto the global.
        // We additionally keep one AddRef in the engine registry so the
        // class outlives the global member and is released exactly once by
        // tjs2_destroy.
        iTJSDispatch2 *global = e->inner->GetGlobalNoAddRef();
        cls->AddRef();
        e->native_classes.push_back(cls);
        e->native_class_names.push_back(name8);
        TJS::tTJSVariant val(cls);
        holder.dispose();
        global->PropSet(TJS_MEMBERENSURE | TJS_IGNOREPROP, name16.c_str(),
                        nullptr, &val, global);
        return 0;
    } catch(...) {
        return -4;
    }
}

} // extern "C"
