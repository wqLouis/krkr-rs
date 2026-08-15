// C ABI shim over the C++ TJS2 scripting VM (tjs2-sys).
//
// This is the only C++ code krkr-rs compiles beyond the tjs2 core itself.
// It keeps the Rust side free of tjs2 C++ headers; everything crosses the
// boundary as C types + UTF-8 strings.

#include "tjsCommHead.h"

#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <new>
#include <string>
#include <unordered_map>
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
    // Retained values: id -> variant. Each variant holds its own reference
    // (tTJSVariant copy/assign semantics AddRef object contents), so the
    // value stays callable until the id is erased. Erasing (or destroying
    // the map with the engine) releases the reference. Ids are engine-local
    // and never 0 (0 is the null tjs2_value_id sentinel).
    std::unordered_map<uintptr_t, TJS::tTJSVariant> retained;
    uintptr_t next_retained_id = 1;
    // The most recent object-valued result this engine produced (from
    // exec/eval results and native-callback argument conversions). A
    // tjs2_value carries no object handle, so tjs2_retain_value resolves an
    // OBJECT-typed value against this slot.
    TJS::tTJSVariant last_object;
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
    // Object values cannot cross the ABI as handles, so remember the most
    // recent object result per engine even when out is null;
    // tjs2_retain_value resolves OBJECT-typed tjs2_values against it.
    if(v.Type() == tvtObject)
        e->last_object = v;
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
        case tvtObject:
            out->type = TJS2_VAL_OBJECT;
            break;
        default:
            // octets etc. cross the boundary as opaque handles too, but are
            // not retainable (no object closure behind them).
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

// Copy a plain ASCII/UTF-8 message into a malloc'd buffer for *out_error.
char *make_error_string(const char *msg) {
    size_t len = std::strlen(msg);
    char *buf = (char *)malloc(len + 1);
    if(!buf)
        return nullptr;
    std::memcpy(buf, msg, len + 1);
    return buf;
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

// Convert a single tTJSVariant into a tjs2_value; string values are copied
// into `storage` so the string pointer stays valid after conversion. This is
// the per-entry logic shared by args_to_values (method arguments) and the
// property set path (one value). Object values are remembered in the
// engine's last_object slot so they can be retained later.
void variant_to_value_one(tjs2_engine *e, const TJS::tTJSVariant &var,
                          tjs2_value *out, std::string &storage) {
    out->integer = 0;
    out->real = 0.0;
    out->string = nullptr;
    switch(var.Type()) {
        case tvtVoid:
            out->type = TJS2_VAL_VOID;
            break;
        case tvtInteger:
            out->type = TJS2_VAL_INTEGER;
            out->integer = (long long)var.AsInteger();
            break;
        case tvtReal:
            out->type = TJS2_VAL_REAL;
            out->real = var.AsReal();
            break;
        case tvtString: {
            out->type = TJS2_VAL_STRING;
            ttstr s(var);
            storage = s.AsStdString();
            out->string = storage.c_str();
            break;
        }
        case tvtObject:
            e->last_object = var;
            out->type = TJS2_VAL_OBJECT;
            break;
        default:
            // objects/octets cross the boundary as opaque handles
            out->type = TJS2_VAL_OBJECT;
            break;
    }
}

// Marshal the tTJSVariant arguments of a native method call into tjs2_value
// entries. String values are copied into `storage` (one std::string per
// argument) so every entry's string pointer stays valid for the duration of
// the call — unlike the single-result path, the engine's last_string buffer
// cannot be reused here because a string argument would be clobbered by the
// next argument's conversion.
void args_to_values(tjs2_engine *e, tjs_int numparams, tTJSVariant **param,
                    std::vector<tjs2_value> &out,
                    std::vector<std::string> &storage) {
    out.resize(numparams);
    storage.resize(numparams);
    for(tjs_int i = 0; i < numparams; i++) {
        variant_to_value_one(e, *param[i], &out[i], storage[i]);
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
        args_to_values(e, numparams, param, argv, arg_storage);

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

// ---------------------------------------------------------------------------
// native property dispatch
// ---------------------------------------------------------------------------

class tjs2_native_property_dispatch;

// Forward declarations: the dispatch class below calls these from its
// PropGet/PropSet bodies (compiled as if right after the class).
tjs_error tjs2_dispatch_native_property_get(tjs2_native_property_dispatch *self,
                                            tTJSVariant *result,
                                            iTJSDispatch2 *objthis);
tjs_error tjs2_dispatch_native_property_set(tjs2_native_property_dispatch *self,
                                            const tTJSVariant *param,
                                            iTJSDispatch2 *objthis);

// A per-property dispatch object, mirroring the wave-1 method dispatch: the
// tTJSNativeClassPropertyGetCallback/SetCallback signatures (tjsNative.h)
// carry no engine or Rust callback, so each registered property gets its own
// object carrying that state; PropGet/PropSet (virtual, called by the VM when
// scripts read/write the property) dispatch to the Rust callbacks.
class tjs2_native_property_dispatch : public TJS::tTJSNativeClassProperty {
    typedef TJS::tTJSNativeClassProperty inherited;

public:
    tjs2_engine *engine;
    tjs2_native_property_get_fn get; // may be nullptr (write-only)
    tjs2_native_property_set_fn set; // may be nullptr (read-only)

    tjs2_native_property_dispatch(tjs2_engine *e, tjs2_native_property_get_fn g,
                                  tjs2_native_property_set_fn s)
        : inherited(nullptr, nullptr), engine(e), get(g), set(s) {}

    tjs_error PropGet(tjs_uint32 flag, const tjs_char *membername,
                      tjs_uint32 *hint, tTJSVariant *result,
                      iTJSDispatch2 *objthis) override {
        if(membername)
            return inherited::PropGet(flag, membername, hint, result, objthis);
        return tjs2_dispatch_native_property_get(this, result, objthis);
    }

    tjs_error PropSet(tjs_uint32 flag, const tjs_char *membername,
                      tjs_uint32 *hint, const tTJSVariant *param,
                      iTJSDispatch2 *objthis) override {
        if(membername)
            return inherited::PropSet(flag, membername, hint, param, objthis);
        return tjs2_dispatch_native_property_set(this, param, objthis);
    }
};

// Dispatch a property read to the Rust get callback. A property without a
// getter is write-only: reading it yields Void.
tjs_error tjs2_dispatch_native_property_get(tjs2_native_property_dispatch *self,
                                            tTJSVariant *result,
                                            iTJSDispatch2 *objthis) {
    (void)objthis; // static properties: objthis is the class object
    tjs2_engine *e = self->engine;
    try {
        if(result)
            result->Clear();

        if(!self->get)
            return TJS_S_OK; // write-only: reading returns void

        tjs2_value out;
        out.type = TJS2_VAL_VOID;
        out.integer = 0;
        out.real = 0.0;
        out.string = nullptr;

        char *out_error = nullptr;
        int rc = self->get(e, &out, &out_error);

        if(rc != 0) {
            std::string msg = out_error
                                  ? out_error
                                  : "native property get reported an error";
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

// Dispatch a property write to the Rust set callback. A property without a
// setter is read-only: writes are denied with an access-denied error.
tjs_error tjs2_dispatch_native_property_set(tjs2_native_property_dispatch *self,
                                            const tTJSVariant *param,
                                            iTJSDispatch2 *objthis) {
    (void)objthis;
    tjs2_engine *e = self->engine;
    try {
        if(!self->set)
            return TJS_E_ACCESSDENYED;

        tjs2_value value;
        std::string storage;
        variant_to_value_one(e, *param, &value, storage);

        char *out_error = nullptr;
        int rc = self->set(e, &value, &out_error);

        if(rc != 0) {
            std::string msg = out_error
                                  ? out_error
                                  : "native property set reported an error";
            if(out_error)
                tjs2_free_string(out_error);
            throw TJS::eTJSError(ttstr(utf8_to_u16(msg.c_str()).c_str()));
        }
        return TJS_S_OK;
    }
    TJS_CONVERT_TO_TJS_EXCEPTION
}

// ---------------------------------------------------------------------------
// native instance dispatch
// ---------------------------------------------------------------------------

class tjs2_native_instance_method_dispatch;

// Forward declaration: the dispatch class below calls this from its FuncCall
// body (compiled as if right after the class).
tjs_error tjs2_dispatch_native_instance_method(
    tjs2_native_instance_method_dispatch *self, tTJSVariant *result,
    tjs_int numparams, tTJSVariant **param, iTJSDispatch2 *objthis);

// A per-method dispatch object for instance-based native classes, carrying
// the engine, the Rust callback and the native class id. The id is what
// NativeInstanceSupport keys per-object instances on, so FuncCall can
// retrieve the Rust payload of the object the method was invoked on.
class tjs2_native_instance_method_dispatch : public TJS::tTJSNativeClassMethod {
    typedef TJS::tTJSNativeClassMethod inherited;

public:
    tjs2_engine *engine;
    tjs2_native_instance_method_fn fn;
    tjs_int32 classid;

    tjs2_native_instance_method_dispatch(tjs2_engine *e,
                                         tjs2_native_instance_method_fn f,
                                         tjs_int32 cid)
        : inherited(nullptr), engine(e), fn(f), classid(cid) {}

    tjs_error FuncCall(tjs_uint32 flag, const tjs_char *membername,
                       tjs_uint32 *hint, tTJSVariant *result,
                       tjs_int numparams, tTJSVariant **param,
                       iTJSDispatch2 *objthis) override {
        if(membername)
            return inherited::FuncCall(flag, membername, hint, result,
                                       numparams, param, objthis);
        return tjs2_dispatch_native_instance_method(this, result, numparams,
                                                    param, objthis);
    }
};

// The per-object native instance backing a Rust-owned payload. Created by
// tjs2_native_class::CreateNativeInstance for every `new ClassName()`; the
// Rust payload is allocated by the create callback and released by the
// destroy callback when the TJS object dies (tTJSCustomObject::Finalize calls
// Invalidate, then ~tTJSCustomObject calls Destruct() -> delete this, which
// runs this destructor).
class tjs2_native_instance : public TJS::tTJSNativeInstance {
    typedef TJS::tTJSNativeInstance inherited;

    tjs2_engine *engine;
    tjs2_native_destroy_instance_fn destroy;
    void *native_ptr;
    bool valid;

public:
    tjs2_native_instance(tjs2_engine *e,
                         tjs2_native_create_instance_fn create,
                         tjs2_native_destroy_instance_fn d)
        : engine(e), destroy(d), native_ptr(nullptr), valid(false) {
        native_ptr = create(e);
        valid = true;
    }

    ~tjs2_native_instance() override {
        if(valid && destroy && native_ptr) {
            destroy(engine, native_ptr);
            native_ptr = nullptr;
            valid = false;
        }
    }

    // Called by the VM on finalize; the Rust payload stays alive until the
    // destructor runs (Destruct -> delete this).
    void Invalidate() override { inherited::Invalidate(); }

    void *GetNativePtr() const { return native_ptr; }
};

// Dispatch an instance method call to the Rust callback. The instance payload
// is retrieved from objthis via NativeInstanceSupport(TJS_NIS_GETINSTANCE,
// classid) — the same mechanism the reference methods use (TJS_GET_NATIVE_
// INSTANCE in tjsNative.h, e.g. EventIntf.cpp method bodies). The lookup
// fails for objects that are not instances of this class (or for the class
// object itself), which becomes a catchable TJS error.
tjs_error tjs2_dispatch_native_instance_method(
    tjs2_native_instance_method_dispatch *self, tTJSVariant *result,
    tjs_int numparams, tTJSVariant **param, iTJSDispatch2 *objthis) {
    tjs2_engine *e = self->engine;
    try {
        if(result)
            result->Clear();

        if(!objthis)
            throw TJS::eTJSError(ttstr(TJS_W(
                "native instance method called without an object")));

        TJS::iTJSNativeInstance *native = nullptr;
        tjs_error hr = objthis->NativeInstanceSupport(TJS_NIS_GETINSTANCE,
                                                      self->classid, &native);
        if(TJS_FAILED(hr) || !native)
            throw TJS::eTJSError(ttstr(TJS_W(
                "native instance method called on an object that is not an "
                "instance of this native class")));

        void *instance =
            static_cast<tjs2_native_instance *>(native)->GetNativePtr();

        std::vector<tjs2_value> argv;
        std::vector<std::string> arg_storage;
        args_to_values(e, numparams, param, argv, arg_storage);

        tjs2_value out;
        out.type = TJS2_VAL_VOID;
        out.integer = 0;
        out.real = 0.0;
        out.string = nullptr;

        char *out_error = nullptr;
        // FFI handoff: `fn` is Rust code that must follow the ABI contract
        // (instance/argv/out valid only during the call, out_error malloc'd).
        int rc = self->fn(e, instance, (int)numparams,
                          argv.empty() ? nullptr : argv.data(), &out,
                          &out_error);

        if(rc != 0) {
            std::string msg =
                out_error ? out_error
                          : "native instance method reported an error";
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

// ---------------------------------------------------------------------------
// native instance / instance-capable native class
// ---------------------------------------------------------------------------

// tTJSNativeClass subclass whose CreateNativeInstance returns a
// Rust-backed tjs2_native_instance. Follows the reference pattern
// (e.g. tTJSNC_AsyncTrigger::CreateNativeInstance, EventIntf.cpp:1193).
class tjs2_native_class : public TJS::tTJSNativeClass {
    typedef TJS::tTJSNativeClass inherited;

    tjs2_engine *engine;
    tjs2_native_create_instance_fn create_instance;
    tjs2_native_destroy_instance_fn destroy_instance;

public:
    tjs2_native_class(const ttstr &name, tjs2_engine *e,
                      tjs2_native_create_instance_fn c,
                      tjs2_native_destroy_instance_fn d)
        : inherited(name), engine(e), create_instance(c), destroy_instance(d) {}

protected:
    TJS::iTJSNativeInstance *CreateNativeInstance() override {
        return new tjs2_native_instance(engine, create_instance,
                                        destroy_instance);
    }
};

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

int tjs2_register_native_class_ex(tjs2_engine *e, const char *class_name_utf8,
                                  const tjs2_native_method *methods, int count,
                                  const tjs2_native_property *properties,
                                  int prop_count) {
    if(!e || !class_name_utf8 || count < 0 || (count > 0 && !methods) ||
       prop_count < 0 || (prop_count > 0 && !properties))
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
        // native data) which is fine for static-only classes.
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

        // Register each property as a static property member, mirroring
        // TJS_END_NATIVE_STATIC_PROP_DECL (tjsNative.h): RegisterNCM(name,
        // TJSCreateNativeClassProperty(get, set), classname, nitProperty,
        // TJS_STATICMEMBER). Scripts read/write these as plain members and
        // may `delete` them (they live in the class's member table).
        for(int i = 0; i < prop_count; i++) {
            const tjs2_native_property &p = properties[i];
            if(!p.name || (!p.get && !p.set))
                return -3;
            std::u16string pname16 = utf8_to_u16(p.name);
            auto *dsp = new tjs2_native_property_dispatch(e, p.get, p.set);
            cls->RegisterNCM(pname16.c_str(), dsp, clsname.c_str(),
                             TJS::nitProperty, TJS_STATICMEMBER);
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

int tjs2_register_native_class(tjs2_engine *e, const char *class_name_utf8,
                               const tjs2_native_method *methods, int count) {
    return tjs2_register_native_class_ex(e, class_name_utf8, methods, count,
                                         nullptr, 0);
}

int tjs2_register_native_class_instance(
    tjs2_engine *e, const char *class_name_utf8,
    const tjs2_native_instance_method *methods, int count,
    tjs2_native_create_instance_fn create_instance,
    tjs2_native_destroy_instance_fn destroy_instance) {
    if(!e || !class_name_utf8 || count < 0 || (count > 0 && !methods) ||
       !create_instance)
        return -1;
    try {
        // Reject duplicate class names, as in the static path.
        std::string name8(class_name_utf8);
        for(const auto &existing : e->native_class_names)
            if(existing == name8)
                return -2;

        std::u16string name16 = utf8_to_u16(class_name_utf8);
        ttstr clsname(name16.c_str());

        // Register the class name and get its process-wide native class id.
        // TJS_BEGIN_NATIVE_MEMBERS does the same (TJSRegisterNativeClass,
        // tjsNative.cpp:25-32) and hands it to the class via SetClassID so
        // that tTJSNativeClass::FuncCall registers each new object's native
        // instance under this id (NativeInstanceSupport(TJS_NIS_REGISTER,
        // _ClassID, ...)) and the method dispatchers can look it back up
        // with TJS_NIS_GETINSTANCE during a call.
        tjs_int32 classid = TJS::TJSRegisterNativeClass(clsname.c_str());

        tjs2_native_class *cls =
            new tjs2_native_class(clsname, e, create_instance, destroy_instance);
        native_class_holder holder(cls);
        cls->SetClassID(classid);

        // Register each method as an instance member (no TJS_STATICMEMBER):
        // tTJSNativeClass::FuncCall copies non-static members onto every
        // created object (rebinding their objthis), so the same dispatch
        // object serves method calls on all instances of the class.
        for(int i = 0; i < count; i++) {
            const tjs2_native_instance_method &m = methods[i];
            if(!m.name || !m.fn)
                return -3;
            std::u16string mname16 = utf8_to_u16(m.name);
            auto *dsp = new tjs2_native_instance_method_dispatch(e, m.fn,
                                                                 classid);
            cls->RegisterNCM(mname16.c_str(), dsp, clsname.c_str(),
                             TJS::nitMethod);
        }

        // Attach to the global object, same as the static path.
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

// ---------------------------------------------------------------------------
// retained values (function objects)
// ---------------------------------------------------------------------------

// Retain a script value. The variant stored in the map is a refcounted copy
// (tTJSVariant assignment AddRefs object contents), so the value stays
// callable until the id is released. A tjs2_value cannot carry an object
// handle: an OBJECT-typed value is resolved against the engine's last
// object-valued result (the value of the most recent exec/eval that yielded
// an object, e.g. eval'ing a script function's name). Returns NULL on
// failure.
tjs2_value_id tjs2_retain_value(void *engine, const tjs2_value *v) {
    tjs2_engine *e = (tjs2_engine *)engine;
    if(!e || !v)
        return nullptr;
    try {
        TJS::tTJSVariant var;
        if(v->type == TJS2_VAL_OBJECT) {
            if(e->last_object.Type() != TJS::tvtObject)
                return nullptr; // no object result to resolve against
            var = e->last_object;
        } else {
            value_to_variant(v, &var);
        }
        // 0 is the null tjs2_value_id sentinel, so skip it.
        uintptr_t id = e->next_retained_id++;
        if(id == 0)
            id = e->next_retained_id++;
        e->retained.emplace(id, var);
        return (tjs2_value_id)id;
    } catch(...) {
        return nullptr;
    }
}

// Release a retained value. Idempotent: releasing an unknown or NULL id is a
// safe no-op (erasing a missing key does nothing).
void tjs2_release_value(void *engine, tjs2_value_id id) {
    tjs2_engine *e = (tjs2_engine *)engine;
    if(!e || !id)
        return;
    try {
        e->retained.erase((uintptr_t)id);
    } catch(...) {
    }
}

// Invoke the retained value's default member with the given arguments.
// Follows the VM's own calling convention for a plain function call
// (tjsInterCodeExec.cpp CallFunction, VM_CALL): FuncCall with no membername
// and no objthis — tTJSVariantClosure::FuncCall resolves objthis as
// ObjThis ? ObjThis : Object (tjsVariant.h). Returns 0 on success and fills
// `out`; non-zero on failure with a malloc'd message in *out_error.
int tjs2_call_value(void *engine, tjs2_value_id id, int argc,
                    const tjs2_value *argv, tjs2_value *out,
                    char **out_error) {
    tjs2_engine *e = (tjs2_engine *)engine;
    if(!e || !id || argc < 0 || (argc > 0 && !argv)) {
        if(out_error)
            *out_error = make_error_string("invalid retained value");
        return 1;
    }
    try {
        if(out) {
            out->type = TJS2_VAL_VOID;
            out->integer = 0;
            out->real = 0.0;
            out->string = nullptr;
        }

        auto it = e->retained.find((uintptr_t)id);
        if(it == e->retained.end()) {
            if(out_error)
                *out_error = make_error_string("invalid retained value");
            return 1;
        }

        // Convert the arguments the same way the native-method dispatch
        // converts results (value_to_variant); object/octet arguments cannot
        // be reconstructed and raise a TJS error, matching the callback
        // return-value path.
        std::vector<TJS::tTJSVariant> arg_vars;
        std::vector<TJS::tTJSVariant *> params;
        arg_vars.reserve((size_t)argc);
        params.reserve((size_t)argc);
        for(int i = 0; i < argc; i++) {
            arg_vars.emplace_back();
            value_to_variant(&argv[i], &arg_vars.back());
            params.push_back(&arg_vars.back());
        }

        TJS::tTJSVariant result;
        TJS::tTJSVariantClosure clo =
            it->second.AsObjectClosureNoAddRef(); // throws if not an object
        tjs_error hr = clo.FuncCall(0, nullptr, nullptr, &result, argc,
                                    params.empty() ? nullptr : params.data(),
                                    nullptr);
        if(TJS_FAILED(hr))
            TJSThrowFrom_tjs_error(hr, TJS_W("")); // -> catch(eTJS) below

        variant_to_value(e, result, out);
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

} // extern "C"
