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
#include "tjsArray.h"
#include "tjsString.h"
#include "tjsError.h"
#include "tjsVariant.h"
#include "tjsNative.h"
#include "tjsInterface.h"
#include "tjsDebug.h"

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
    // Stack of by-reference argument frames for the native method call(s)
    // currently executing on this engine. The ABI hands the Rust callback
    // copied tjs2_value entries, so this is the only path back to the
    // caller's original tTJSVariant slots (tjs2_set_arg writes here). It is
    // a stack, not a single slot, because a native callback may re-enter the
    // VM (tjs2_call_value / tjs2_exec_script) before writing its out params.
    struct tjs2_param_frame {
        TJS::tTJSVariant **param;
        tjs_int count;
    };
    std::vector<tjs2_param_frame> param_stack;
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
    out->array = nullptr;
    out->array_count = 0;
    out->retained = nullptr;
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
            // TJS `null` is an object with a null Object pointer; keep it
            // distinct from a real object and from void. A real object's
            // closure receiver (ObjThis) is carried in the otherwise-unused
            // `array` slot (see variant_to_value_one).
            if(v.AsObjectNoAddRef()) {
                out->type = TJS2_VAL_OBJECT;
                out->array = reinterpret_cast<const char **>(
                    v.AsObjectThisNoAddRef());
            } else {
                out->type = TJS2_VAL_NULL;
            }
            break;
        case tvtOctet: {
            // Raw binary data. std::string can carry embedded NULs, so the
            // bytes stay valid until the next engine call (same lifetime as
            // the UTF-8 string results). `array_count` carries the length.
            TJS::tTJSVariantOctet *oct = v.AsOctetNoAddRef();
            if(oct) {
                e->last_string.assign(
                    reinterpret_cast<const char *>(oct->GetData()),
                    oct->GetLength());
                out->array_count = (int)oct->GetLength();
            } else {
                e->last_string.clear();
                out->array_count = 0;
            }
            out->string = e->last_string.data();
            out->type = TJS2_VAL_OCTET;
            break;
        }
        default:
            // No other variant type exists; keep the ABI total rather than
            // misclassifying an unknown value as an object.
            out->type = TJS2_VAL_VOID;
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
        // "TJS error" is 10 bytes including the NUL terminator.
        char *buf = (char *)malloc(10);
        if(!buf)
            return nullptr;
        std::memcpy(buf, "TJS error", 10);
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

// When the VM discards a native result (a bare expression statement passes
// result == NULL), a TJS2_VAL_RETAINED result would otherwise linger in the
// retained map forever: the map entry holds a reference nothing ever
// releases. Consume it (release the reference) exactly as the conversion
// would have.
void consume_retained_if_discarded(tjs2_engine *e, const tjs2_value &out) {
    if(out.type == TJS2_VAL_RETAINED && out.retained && e) {
        auto it = e->retained.find((uintptr_t)out.retained);
        if(it != e->retained.end())
            e->retained.erase(it);
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
    out->array = nullptr;
    out->array_count = 0;
    out->retained = nullptr;
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
            // Carry the object handle itself. A native method can receive
            // several object arguments (e.g. Layer.drawPolygon(app, points));
            // last_object only remembers the last one, so each argument
            // records its own handle for tjs2_retain_value.
            if(var.AsObjectNoAddRef()) {
                out->retained =
                    reinterpret_cast<tjs2_value_id>(var.AsObjectNoAddRef());
                // A method/closure reference has a distinct ObjThis (its
                // receiver). Marshal it in the unused-for-objects `array`
                // slot so a native that retains the argument and invokes it
                // later runs with the correct `this` (tTJSVariantClosure::
                // FuncCall prefers ObjThis, tjsVariant.h:227-238). Null for a
                // plain object.
                out->array = reinterpret_cast<const char **>(
                    var.AsObjectThisNoAddRef());
                out->type = TJS2_VAL_OBJECT;
            } else {
                // TJS `null` (tvtObject with a null Object pointer) must not
                // masquerade as a handle-less object, or retaining it would
                // fall back to last_object and retain the wrong value.
                out->retained = nullptr;
                out->array = nullptr;
                out->type = TJS2_VAL_NULL;
            }
            break;
        case tvtOctet: {
            // Raw binary data; `storage` keeps the bytes alive until the
            // callback returns, `array_count` carries the length.
            TJS::tTJSVariantOctet *oct = var.AsOctetNoAddRef();
            if(oct) {
                storage.assign(
                    reinterpret_cast<const char *>(oct->GetData()),
                    oct->GetLength());
                out->string = storage.data();
                out->array_count = (int)oct->GetLength();
            }
            out->type = TJS2_VAL_OCTET;
            break;
        }
        default:
            // No other variant type exists; keep the ABI total rather than
            // misclassifying an unknown value as an object.
            out->type = TJS2_VAL_VOID;
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

// RAII push/pop of the by-reference argument frame around a Rust native
// callback, so tjs2_set_arg can address the caller's original variants.
// Nested (re-entrant) calls stack correctly, and the destructor pops on
// every exit path, including a C++ exception from the callback.
struct tjs2_param_frame_guard {
    tjs2_engine *engine;
    tjs2_param_frame_guard(tjs2_engine *e, TJS::tTJSVariant **p,
                           tjs_int count)
        : engine(e) {
        engine->param_stack.push_back({p, count});
    }
    ~tjs2_param_frame_guard() { engine->param_stack.pop_back(); }
};

// Convert a tjs2_value produced by a Rust native method callback into a
// tTJSVariant result. Strings are UTF-8 and owned by the caller for the
// duration of the callback. Throws TJS::eTJSError for types that cannot be
// reconstructed across the ABI boundary.
void value_to_variant(tjs2_engine *e, const tjs2_value *in, TJS::tTJSVariant *out) {
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
        case TJS2_VAL_RETAINED: {
            // Consume the retention: copy the variant (AddRef'd) into the
            // result and erase the map entry, so the id is released right
            // after the copy (the Rust side's own release on drop is then a
            // safe no-op).
            if(!e || !in->retained)
                throw TJS::eTJSError(ttstr(TJS_W("retained value id is null")));
            auto it = e->retained.find((uintptr_t)in->retained);
            if(it == e->retained.end())
                throw TJS::eTJSError(ttstr(TJS_W("retained value id not found")));
            *out = it->second; // tTJSVariant copy (AddRef'd)
            e->retained.erase(it);
            break;
        }
        case TJS2_VAL_OCTET: {
            const tjs_uint8 *data =
                reinterpret_cast<const tjs_uint8 *>(in->string);
            tjs_uint len = in->array_count > 0 ? (tjs_uint)in->array_count : 0;
            TJS::tTJSVariantOctet *oct = TJS::TJSAllocVariantOctet(data, len);
            *out = oct; // AddRefs (or sets an empty octet for null)
            if(oct)
                oct->Release(); // transfer the allocation reference
            break;
        }
        case TJS2_VAL_OBJECT: {
            // A raw object handle can be reconstructed when present (native
            // arguments marshal it in `retained`); a handle-less object
            // (eval/exec results, synthetic TjsValue::Object) is resolved
            // against the engine's last object-valued result, like
            // resolve_value_variant does.
            if(in->retained) {
                TJS::iTJSDispatch2 *obj =
                    reinterpret_cast<TJS::iTJSDispatch2 *>(in->retained);
                TJS::iTJSDispatch2 *objthis =
                    in->array ? reinterpret_cast<TJS::iTJSDispatch2 *>(
                                    const_cast<char **>(in->array))
                              : obj;
                *out = TJS::tTJSVariant(obj, objthis);
            } else if(e && e->last_object.Type() == TJS::tvtObject) {
                *out = e->last_object;
            } else {
                throw TJS::eTJSError(ttstr(TJS_W(
                    "object value carries no handle and no object result is "
                    "available")));
            }
            break;
        }
        case TJS2_VAL_NULL:
            // TJS `null`: a tvtObject with a null Object pointer.
            *out = (TJS::iTJSDispatch2 *)nullptr;
            break;
        case TJS2_VAL_ARRAY: {
            TJS::iTJSDispatch2 *arr = TJS::TJSCreateArrayObject();
            if(in->array && in->array_count > 0) {
                TJS::tTJSArrayNI *ni = nullptr;
                arr->NativeInstanceSupport(TJS_NIS_GETINSTANCE,
                                           TJS::TJSGetArrayClassID(),
                                           (TJS::iTJSNativeInstance **)&ni);
                if(ni) {
                    for(int i = 0; i < in->array_count; i++) {
                        if(!in->array[i])
                            continue;
                        ttstr el(utf8_to_u16(in->array[i]).c_str());
                        ni->Items.push_back(tTJSVariant(el));
                    }
                }
            }
            *out = tTJSVariant(arr, arr);
            arr->Release();
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
        out.array = nullptr;
        out.array_count = 0;
        out.retained = nullptr;

        char *out_error = nullptr;
        // Expose the caller's by-reference argument slots to Rust for the
        // duration of the callback (tjs2_set_arg).
        tjs2_param_frame_guard param_guard(e, param, numparams);
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

        if(result) {
            value_to_variant(e, &out, result);
        } else {
            // The VM discarded the result (bare statement): release any
            // retained value instead of leaking the map entry.
            consume_retained_if_discarded(e, out);
        }
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
        out.array = nullptr;
        out.array_count = 0;
        out.retained = nullptr;

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

        if(result) {
            value_to_variant(e, &out, result);
        } else {
            // The VM discarded the result (bare statement): release any
            // retained value instead of leaking the map entry.
            consume_retained_if_discarded(e, out);
        }
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
class tjs2_native_class;

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

// A constructor-member dispatch for instance-based native classes. The
// reference registers the class name itself as a member (via
// TJS_BEGIN_NATIVE_CONSTRUCTOR_DECL), so script subclasses can call
// `super.ClassName()` in their constructor:
//
//     class ScController extends KAGParser {
//         function ScController(handle){ super.KAGParser(); ... }
//     }
//
// Script-subclass objects have NO native instance yet, so this dispatch
// creates one and registers it on objthis before invoking the Rust
// constructor callback — mirroring what the reference's
// TJS_GET_NATIVE_INSTANCE does for the constructor member. The FuncCall
// body lives after the tjs2_native_class definition (it needs its
// NewInstance() accessor).
class tjs2_native_instance_constructor_dispatch
    : public TJS::tTJSNativeClassMethod {
    typedef TJS::tTJSNativeClassMethod inherited;

public:
    tjs2_engine *engine;
    tjs2_native_class *cls;
    tjs_int32 classid;
    tjs2_native_instance_method_fn fn;

    tjs2_native_instance_constructor_dispatch(
        tjs2_engine *e, tjs2_native_class *c, tjs_int32 cid,
        tjs2_native_instance_method_fn f)
        : inherited(nullptr), engine(e), cls(c), classid(cid), fn(f) {}

    tjs_error FuncCall(tjs_uint32 flag, const tjs_char *membername,
                       tjs_uint32 *hint, tTJSVariant *result,
                       tjs_int numparams, tTJSVariant **param,
                       iTJSDispatch2 *objthis) override;
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
    bool valid;       // payload allocated and not yet released
    bool invalidated; // finalized; direct dispatch must stop

    // Release the Rust payload exactly once (from the destructor).
    void release_payload() {
        if(valid) {
            if(destroy && native_ptr)
                destroy(engine, native_ptr);
            native_ptr = nullptr;
            valid = false;
        }
    }

public:
    tjs2_native_instance(tjs2_engine *e,
                         tjs2_native_create_instance_fn create,
                         tjs2_native_destroy_instance_fn d)
        : engine(e), destroy(d), native_ptr(nullptr), valid(false),
          invalidated(false) {
        native_ptr = create(e);
        valid = true;
    }

    ~tjs2_native_instance() override { release_payload(); }

    // Reference `tTJSNativeInstance::Invalidate()` is a no-op (tjsNative.h:42)
    // and `Destruct()` deletes the instance (tjsNative.h:44). Concrete
    // natives release resources in `Invalidate`, but the C++ instance object
    // stays alive until `Destruct()`, and `tTJSCustomObject::Finalize()`
    // calls the script `finalize` BEFORE `Invalidate()` (tjsObject.cpp:389-
    // 405). Freeing the Rust payload here would tear down backing state
    // (e.g. remove a scene layer via the destroy callback) while sibling or
    // child objects are still finalizing; defer the release to the
    // destructor and only mark the instance so direct dispatch stops. This
    // keeps a script `finalize` able to set native properties
    // (`SelectItemBase.finalize`: `cursor = crDefault`) while still refusing
    // calls on a finalized object (the first-task guard).
    void Invalidate() override {
        invalidated = true;
        inherited::Invalidate();
    }

    // Null once finalized (so the dispatchers reject calls) or if the Rust
    // create callback returned null. The payload itself is released only by
    // the destructor.
    void *GetNativePtr() const {
        return (valid && !invalidated) ? native_ptr : nullptr;
    }
    bool IsValid() const { return valid; }
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
        if(!instance)
            throw TJS::eTJSError(ttstr(TJS_W(
                "native instance method called on an invalidated object")));

        std::vector<tjs2_value> argv;
        std::vector<std::string> arg_storage;
        args_to_values(e, numparams, param, argv, arg_storage);

        tjs2_value out;
        out.type = TJS2_VAL_VOID;
        out.integer = 0;
        out.real = 0.0;
        out.string = nullptr;
        out.array = nullptr;
        out.array_count = 0;
        out.retained = nullptr;

        char *out_error = nullptr;
        // Expose the caller's by-reference argument slots to Rust for the
        // duration of the callback (tjs2_set_arg).
        tjs2_param_frame_guard param_guard(e, param, numparams);
        // FFI handoff: `fn` is Rust code that must follow the ABI contract
        // (instance/argv/out valid only during the call, out_error malloc'd).
        int rc = self->fn(e, instance, (int)numparams,
                          argv.empty() ? nullptr : argv.data(), &out,
                          &out_error, (void *)objthis);

        if(rc != 0) {
            std::string msg =
                out_error ? out_error
                          : "native instance method reported an error";
            if(out_error)
                tjs2_free_string(out_error);
            throw TJS::eTJSError(ttstr(utf8_to_u16(msg.c_str()).c_str()));
        }

        if(result) {
            value_to_variant(e, &out, result);
        } else {
            // The VM discarded the result (bare statement): release any
            // retained value instead of leaking the map entry.
            consume_retained_if_discarded(e, out);
        }
        return TJS_S_OK;
    }
    TJS_CONVERT_TO_TJS_EXCEPTION
}

// ---------------------------------------------------------------------------
// native instance property dispatch
// ---------------------------------------------------------------------------

class tjs2_native_instance_property_dispatch;

tjs_error tjs2_dispatch_native_instance_property_get(
    tjs2_native_instance_property_dispatch *self, tTJSVariant *result,
    iTJSDispatch2 *objthis);
tjs_error tjs2_dispatch_native_instance_property_set(
    tjs2_native_instance_property_dispatch *self, const tTJSVariant *param,
    iTJSDispatch2 *objthis);

// A per-property dispatch object for instance-based native classes. Like the
// instance method dispatch, it carries the class id so PropGet/PropSet can
// retrieve the Rust payload of the object the property was accessed on.
class tjs2_native_instance_property_dispatch
    : public TJS::tTJSNativeClassProperty {
    typedef TJS::tTJSNativeClassProperty inherited;

public:
    tjs2_engine *engine;
    tjs2_native_instance_property_get_fn get; // may be nullptr (write-only)
    tjs2_native_instance_property_set_fn set; // may be nullptr (read-only)
    tjs_int32 classid;

    tjs2_native_instance_property_dispatch(tjs2_engine *e,
                                           tjs2_native_instance_property_get_fn g,
                                           tjs2_native_instance_property_set_fn s,
                                           tjs_int32 cid)
        : inherited(nullptr, nullptr), engine(e), get(g), set(s), classid(cid) {}

    tjs_error PropGet(tjs_uint32 flag, const tjs_char *membername,
                      tjs_uint32 *hint, tTJSVariant *result,
                      iTJSDispatch2 *objthis) override {
        if(membername)
            return inherited::PropGet(flag, membername, hint, result, objthis);
        return tjs2_dispatch_native_instance_property_get(this, result,
                                                          objthis);
    }

    tjs_error PropSet(tjs_uint32 flag, const tjs_char *membername,
                      tjs_uint32 *hint, const tTJSVariant *param,
                      iTJSDispatch2 *objthis) override {
        if(membername)
            return inherited::PropSet(flag, membername, hint, param, objthis);
        return tjs2_dispatch_native_instance_property_set(this, param,
                                                          objthis);
    }
};

// Dispatch an instance property read to the Rust get callback. The instance
// payload is retrieved from objthis exactly like the instance method
// dispatch (NativeInstanceSupport(TJS_NIS_GETINSTANCE, classid)).
tjs_error tjs2_dispatch_native_instance_property_get(
    tjs2_native_instance_property_dispatch *self, tTJSVariant *result,
    iTJSDispatch2 *objthis) {
    tjs2_engine *e = self->engine;
    try {
        if(result)
            result->Clear();

        if(!self->get)
            return TJS_S_OK; // write-only: reading returns void

        if(!objthis)
            throw TJS::eTJSError(ttstr(TJS_W(
                "native instance property read without an object")));

        TJS::iTJSNativeInstance *native = nullptr;
        tjs_error hr = objthis->NativeInstanceSupport(TJS_NIS_GETINSTANCE,
                                                      self->classid, &native);
        if(TJS_FAILED(hr) || !native)
            throw TJS::eTJSError(ttstr(TJS_W(
                "native instance property read on an object that is not an "
                "instance of this native class")));

        void *instance =
            static_cast<tjs2_native_instance *>(native)->GetNativePtr();
        if(!instance)
            throw TJS::eTJSError(ttstr(TJS_W(
                "native instance property read on an invalidated object")));

        tjs2_value out;
        out.type = TJS2_VAL_VOID;
        out.integer = 0;
        out.real = 0.0;
        out.string = nullptr;
        out.array = nullptr;
        out.array_count = 0;
        out.retained = nullptr;

        char *out_error = nullptr;
        int rc = self->get(e, instance, &out, &out_error, (void *)objthis);

        if(rc != 0) {
            std::string msg = out_error
                                  ? out_error
                                  : "native instance property get reported an error";
            if(out_error)
                tjs2_free_string(out_error);
            throw TJS::eTJSError(ttstr(utf8_to_u16(msg.c_str()).c_str()));
        }

        if(result) {
            value_to_variant(e, &out, result);
        } else {
            // The VM discarded the result (bare statement): release any
            // retained value instead of leaking the map entry.
            consume_retained_if_discarded(e, out);
        }
        return TJS_S_OK;
    }
    TJS_CONVERT_TO_TJS_EXCEPTION
}

tjs_error tjs2_dispatch_native_instance_property_set(
    tjs2_native_instance_property_dispatch *self, const tTJSVariant *param,
    iTJSDispatch2 *objthis) {
    tjs2_engine *e = self->engine;
    try {
        if(!self->set)
            return TJS_E_ACCESSDENYED;

        if(!objthis)
            throw TJS::eTJSError(ttstr(TJS_W(
                "native instance property write without an object")));

        TJS::iTJSNativeInstance *native = nullptr;
        tjs_error hr = objthis->NativeInstanceSupport(TJS_NIS_GETINSTANCE,
                                                      self->classid, &native);
        if(TJS_FAILED(hr) || !native)
            throw TJS::eTJSError(ttstr(TJS_W(
                "native instance property write on an object that is not an "
                "instance of this native class")));

        void *instance =
            static_cast<tjs2_native_instance *>(native)->GetNativePtr();
        if(!instance)
            throw TJS::eTJSError(ttstr(TJS_W(
                "native instance property write on an invalidated object")));

        tjs2_value value;
        std::string storage;
        variant_to_value_one(e, *param, &value, storage);

        char *out_error = nullptr;
        int rc = self->set(e, instance, &value, &out_error, (void *)objthis);

        if(rc != 0) {
            std::string msg = out_error
                                  ? out_error
                                  : "native instance property set reported an error";
            if(out_error)
                tjs2_free_string(out_error);
            throw TJS::eTJSError(ttstr(utf8_to_u16(msg.c_str()).c_str()));
        }
        return TJS_S_OK;
    }
    TJS_CONVERT_TO_TJS_EXCEPTION
}

// ---------------------------------------------------------------------------
// native instance / instance-capable native class
// ---------------------------------------------------------------------------
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

    // Public accessor for the constructor dispatch (CreateNativeInstance is
    // protected in the base class).
    TJS::iTJSNativeInstance *NewInstance() { return CreateNativeInstance(); }

protected:
    TJS::iTJSNativeInstance *CreateNativeInstance() override {
        return new tjs2_native_instance(engine, create_instance,
                                        destroy_instance);
    }
};

// Constructor-member dispatch body (see the class declaration above).
tjs_error tjs2_native_instance_constructor_dispatch::FuncCall(
    tjs_uint32 flag, const tjs_char *membername, tjs_uint32 *hint,
    tTJSVariant *result, tjs_int numparams, tTJSVariant **param,
    iTJSDispatch2 *objthis) {
    if(membername)
        return inherited::FuncCall(flag, membername, hint, result, numparams,
                                   param, objthis);
    tjs2_engine *e = this->engine;
    try {
        if(result)
            result->Clear();
        if(!objthis)
            throw TJS::eTJSError(ttstr(TJS_W(
                "native instance constructor called without an object")));

        TJS::iTJSNativeInstance *native = nullptr;
        tjs_error hr = objthis->NativeInstanceSupport(TJS_NIS_GETINSTANCE,
                                                      classid, &native);
        if(TJS_FAILED(hr) || !native) {
            // Script-subclass object: create + register the native instance
            // (reference: TJS_GET_NATIVE_INSTANCE inside the constructor,
            // tjsNative.h:368-381). Check both the allocation and the
            // registration; a failure must not leave a null/absent instance
            // behind for the payload lookup below.
            TJS::iTJSNativeInstance *created = cls->NewInstance();
            if(!created)
                throw TJS::eTJSError(
                    ttstr(TJS_W("failed to create native instance")));
            hr = objthis->NativeInstanceSupport(TJS_NIS_REGISTER, classid,
                                                &created);
            if(TJS_FAILED(hr))
                throw TJS::eTJSError(
                    ttstr(TJS_W("failed to register native instance")));
            native = created;
        }
        void *instance =
            static_cast<tjs2_native_instance *>(native)->GetNativePtr();
        if(!instance)
            throw TJS::eTJSError(ttstr(TJS_W(
                "native instance constructor called on an invalidated "
                "object")));

        std::vector<tjs2_value> argv;
        std::vector<std::string> arg_storage;
        args_to_values(e, numparams, param, argv, arg_storage);

        tjs2_value out;
        out.type = TJS2_VAL_VOID;
        out.integer = 0;
        out.real = 0.0;
        out.string = nullptr;
        out.array = nullptr;
        out.array_count = 0;
        out.retained = nullptr;

        char *out_error = nullptr;
        // Expose the caller's by-reference argument slots to Rust for the
        // duration of the callback (tjs2_set_arg).
        tjs2_param_frame_guard param_guard(e, param, numparams);
        int rc = fn(e, instance, (int)numparams,
                    argv.empty() ? nullptr : argv.data(), &out, &out_error,
                    (void *)objthis);
        if(rc != 0) {
            std::string msg =
                out_error ? out_error
                          : "native instance constructor reported an error";
            if(out_error)
                tjs2_free_string(out_error);
            throw TJS::eTJSError(ttstr(utf8_to_u16(msg.c_str()).c_str()));
        }
        if(result) {
            value_to_variant(e, &out, result);
        } else {
            consume_retained_if_discarded(e, out);
        }
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
// value resolution shared by the retained-value entry points
// ---------------------------------------------------------------------------

// Resolve a tjs2_value into a tTJSVariant. An OBJECT value that carries its
// own raw object handle (set by variant_to_value_one when marshalling native
// arguments) resolves to exactly that object, so a native taking several
// object arguments can retain each one. A zero handle keeps the legacy
// fallback: resolve against the engine's most recent object-valued result
// (used by the Rust-side synthetic TjsValue::Object and by eval results).
// Returns false only when an OBJECT value has neither a handle nor a
// last_object to fall back to.
static bool resolve_value_variant(tjs2_engine *e, const tjs2_value *v,
                                  TJS::tTJSVariant &var) {
    if(v->type == TJS2_VAL_OBJECT) {
        if(v->retained) {
            TJS::iTJSDispatch2 *obj =
                reinterpret_cast<TJS::iTJSDispatch2 *>(v->retained);
            // `array` carries the closure's ObjThis for a native argument
            // (null for a plain object); fall back to `obj` otherwise, which
            // matches tTJSVariantClosure's Object-as-receiver default.
            TJS::iTJSDispatch2 *objthis =
                v->array ? reinterpret_cast<TJS::iTJSDispatch2 *>(
                               const_cast<char **>(v->array))
                         : obj;
            var = TJS::tTJSVariant(obj, objthis);
            return true;
        }
        if(e->last_object.Type() != TJS::tvtObject)
            return false; // no object result to resolve against
        var = e->last_object;
        return true;
    }
    value_to_variant(e, v, &var);
    return true;
}

// Insert a variant into the engine's retained map and return a fresh,
// non-zero id. Used by entry points that hand an object back to Rust, which
// receives it as a TJS2_VAL_RETAINED value and owns the release.
static tjs2_value_id retain_variant(tjs2_engine *e,
                                    const TJS::tTJSVariant &var) {
    uintptr_t id = e->next_retained_id++;
    if(id == 0)
        id = e->next_retained_id++;
    e->retained.emplace(id, var);
    return (tjs2_value_id)id;
}

// Resolve a retained id to the object it holds (null when the id is not a
// live retained object). Shared by the ClassInstanceInfo entry points, which
// take an object argument the Rust side has retained.
static TJS::iTJSDispatch2 *resolve_retained_object(tjs2_engine *e,
                                                   tjs2_value_id obj) {
    if(!e || !obj)
        return nullptr;
    auto it = e->retained.find((uintptr_t)obj);
    if(it == e->retained.end() || it->second.Type() != TJS::tvtObject)
        return nullptr;
    return it->second.AsObjectNoAddRef();
}

// ---------------------------------------------------------------------------
// C ABI
// ---------------------------------------------------------------------------

extern "C" {

tjs2_engine *tjs2_create(void) {
    try {
        // May throw (logger registration). Keep it inside the try so no C++
        // exception can cross the C ABI boundary.
        ensure_spdlog_loggers();
        tjs2_engine *e = new (std::nothrow) tjs2_engine;
        if(!e)
            return nullptr;
        e->inner = new (std::nothrow) TJS::tTJS();
        if(!e->inner) {
            delete e;
            return nullptr;
        }
        // Wire Array/Dictionary saveStruct/loadStruct streams (the base
        // module that normally sets these pointers is not compiled).
        tjs2_wire_stream_factories();
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
        // Drop retained values, the last-object slot and the registered
        // classes while the VM is still alive: their releases/destructors
        // may touch global VM state that ~tTJS frees. (The global object's
        // refs were already dropped by Shutdown's Global->Clear().)
        e->retained.clear();
        e->last_object.Clear();
        for(TJS::iTJSDispatch2 *cls : e->native_classes)
            cls->Release();
        e->native_classes.clear();
        e->native_class_names.clear();
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
    // Clear the retained-object slot at entry: it exists so Rust can retain
    // the most recent object-valued result of THIS call; holding it across
    // calls would pin objects alive past their script lifetime.
    e->last_object.Clear();
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
    e->last_object.Clear(); // see tjs2_exec_script
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

int tjs2_compile_script(tjs2_engine *e, const char *script,
                        const char *output_path, int isresult,
                        int outputdebug, int isexpression, const char *name,
                        int lineofs, char **out_error) {
    if(out_error)
        *out_error = nullptr;
    if(!e || !script || !output_path)
        return -1;
    try {
        std::u16string s = utf8_to_u16(script);
        std::u16string n = name ? utf8_to_u16(name) : std::u16string();
        std::u16string path = utf8_to_u16(output_path);
        // The reference opens the destination through the wired storage
        // stream factory (TVPCompileStorage -> TVPCreateStream(TJS_BS_WRITE));
        // krkr-rs's factory is file-backed, so `output_path` is a plain
        // path (absolute, or data-dir-relative).
        TJS::tTJSBinaryStream *output =
            TJS::TJSCreateBinaryStreamForWrite(ttstr(path.c_str()),
                                               ttstr(TJS_W("wb")));
        if(!output) {
            if(out_error)
                *out_error =
                    make_error_string("cannot open the script output file");
            return 1;
        }
        try {
            e->inner->CompileScript(s.c_str(), output, isresult != 0,
                                    outputdebug != 0, isexpression != 0,
                                    n.c_str(), (tjs_int)lineofs);
        } catch(...) {
            delete output;
            throw;
        }
        delete output;
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

int tjs2_dump(tjs2_engine *e, char **out_error) {
    if(out_error)
        *out_error = nullptr;
    if(!e)
        return -1;
    try {
        // Routes through the engine's console output adapter, i.e. the
        // Rust log callback set with tjs2_set_log_cb.
        e->inner->Dump();
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

int tjs2_do_gc(tjs2_engine *e, char **out_error) {
    if(out_error)
        *out_error = nullptr;
    if(!e)
        return -1;
    try {
        // Reference `tTJS::DoGarbageCollection` (`tjs.cpp:495`), invoked by
        // the `TVP_COMPACT_LEVEL_IDLE` callback in `System.doCompact`.
        e->inner->DoGarbageCollection();
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
        if(out_error)
            *out_error = make_error_string("garbage collection failed");
        return 1;
    }
}

int tjs2_get_class_names(tjs2_engine *e, tjs2_value_id obj, tjs2_value *out,
                         char **out_error) {
    if(out_error)
        *out_error = nullptr;
    if(!e || !obj || !out) {
        if(out_error)
            *out_error = make_error_string("invalid object argument");
        return -1;
    }
    try {
        TJS::iTJSDispatch2 *dsp = resolve_retained_object(e, obj);
        if(!dsp) {
            if(out_error)
                *out_error = make_error_string("invalid retained object");
            return 1;
        }
        // Reference Scripts.getClassNames: walk ClassInstanceInfo(TJS_CII_GET)
        // until it fails and collect the names into a TJS Array.
        TJS::iTJSDispatch2 *array = TJS::TJSCreateArrayObject();
        if(!array) {
            if(out_error)
                *out_error = make_error_string("failed to create the name array");
            return 1;
        }
        try {
            tjs_uint num = 0;
            while(true) {
                TJS::tTJSVariant val;
                tjs_error err = dsp->ClassInstanceInfo(TJS_CII_GET, num, &val);
                if(TJS_FAILED(err))
                    break;
                array->PropSetByNum(TJS_MEMBERENSURE, num, &val, array);
                num++;
            }
        } catch(...) {
            array->Release();
            throw;
        }
        TJS::tTJSVariant var(array, array);
        array->Release();
        out->type = TJS2_VAL_RETAINED;
        out->integer = 0;
        out->real = 0.0;
        out->string = nullptr;
        out->array = nullptr;
        out->array_count = 0;
        out->retained = retain_variant(e, var);
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

int tjs2_set_call_missing(tjs2_engine *e, tjs2_value_id obj,
                          char **out_error) {
    if(out_error)
        *out_error = nullptr;
    if(!e || !obj) {
        if(out_error)
            *out_error = make_error_string("invalid object argument");
        return -1;
    }
    try {
        TJS::iTJSDispatch2 *dsp = resolve_retained_object(e, obj);
        if(!dsp) {
            if(out_error)
                *out_error = make_error_string("invalid retained object");
            return 1;
        }
        // Reference Scripts.setCallMissing passes "missing" so the object's
        // `missing` method is called for absent members (CallMissing = true).
        TJS::tTJSVariant missing(ttstr(TJS_W("missing")));
        dsp->ClassInstanceInfo(TJS_CII_SET_MISSING, 0, &missing);
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
        tjs_error hr = global->PropSet(TJS_MEMBERENSURE | TJS_IGNOREPROP,
                                       name16.c_str(), nullptr, &val, global);
        if(TJS_FAILED(hr))
            return -6;
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

// Empty method body used for the automatically-registered `finalize` member
// (see tjs2_register_native_class_instance).
int tjs2_noop_instance_method(void *, void *, int, const tjs2_value *,
                              tjs2_value *out, char **, void *) {
    if(out) {
        out->type = TJS2_VAL_VOID;
        out->integer = 0;
        out->real = 0.0;
        out->string = nullptr;
        out->array = nullptr;
        out->array_count = 0;
        out->retained = nullptr;
    }
    return 0;
}

int tjs2_register_native_class_instance(
    tjs2_engine *e, const char *class_name_utf8,
    const tjs2_native_instance_method *methods, int count,
    const tjs2_native_instance_property *properties, int property_count,
    tjs2_native_create_instance_fn create_instance,
    tjs2_native_destroy_instance_fn destroy_instance) {
    if(!e || !class_name_utf8 || count < 0 || (count > 0 && !methods) ||
       property_count < 0 || (property_count > 0 && !properties) ||
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
        bool has_finalize = false;
        for(int i = 0; i < count; i++) {
            const tjs2_native_instance_method &m = methods[i];
            if(!m.name || !m.fn)
                return -3;
            std::u16string mname16 = utf8_to_u16(m.name);
            if(mname16 == u"finalize")
                has_finalize = true;
            if(mname16 == name16) {
                // The class-name member is the constructor: it must also
                // work on script-subclass objects that do not have a native
                // instance yet (`super.ClassName()`), so it uses the
                // constructor dispatch which creates+registers on demand.
                auto *dsp = new tjs2_native_instance_constructor_dispatch(
                    e, cls, classid, m.fn);
                cls->RegisterNCM(mname16.c_str(), dsp, clsname.c_str(),
                                 TJS::nitMethod);
            } else {
                auto *dsp = new tjs2_native_instance_method_dispatch(e, m.fn,
                                                                     classid);
                cls->RegisterNCM(mname16.c_str(), dsp, clsname.c_str(),
                                 TJS::nitMethod);
            }
        }

        // The reference's `TJS_BEGIN_NATIVE_MEMBERS` classes include
        // `TJS_DECL_EMPTY_FINALIZE_METHOD`, so a script subclass can call
        // `super.finalize()` (e.g. the game's AffineLayer.finalize). Mirror
        // that by giving every instance class a no-op `finalize` unless it
        // registered its own.
        if(!has_finalize) {
            auto *dsp = new tjs2_native_instance_method_dispatch(
                e, tjs2_noop_instance_method, classid);
            static const char16_t finalize_name[] = u"finalize";
            cls->RegisterNCM(finalize_name, dsp, clsname.c_str(),
                             TJS::nitMethod);
        }

        // Register each property as an instance member (no TJS_STATICMEMBER)
        // so reads/writes on any instance resolve through the same dispatch
        // object, which looks the payload up via the class id.
        for(int i = 0; i < property_count; i++) {
            const tjs2_native_instance_property &p = properties[i];
            if(!p.name)
                return -5;
            std::u16string pname16 = utf8_to_u16(p.name);
            auto *dsp = new tjs2_native_instance_property_dispatch(
                e, p.get, p.set, classid);
            cls->RegisterNCM(pname16.c_str(), dsp, clsname.c_str(),
                             TJS::nitProperty);
        }

        // Attach to the global object, same as the static path.
        iTJSDispatch2 *global = e->inner->GetGlobalNoAddRef();
        cls->AddRef();
        e->native_classes.push_back(cls);
        e->native_class_names.push_back(name8);
        TJS::tTJSVariant val(cls);
        holder.dispose();
        tjs_error hr = global->PropSet(TJS_MEMBERENSURE | TJS_IGNOREPROP,
                                       name16.c_str(), nullptr, &val, global);
        if(TJS_FAILED(hr))
            return -6;
        return 0;
    } catch(...) {
        return -4;
    }
}

int tjs2_register_native_static_members(
    tjs2_engine *e, const char *class_name_utf8,
    const tjs2_native_method *methods, int count,
    const tjs2_native_property *properties, int prop_count) {
    if(!e || !class_name_utf8 || count < 0 || (count > 0 && !methods) ||
       prop_count < 0 || (prop_count > 0 && !properties))
        return -1;
    try {
        // Find the already-registered class (both registration paths create a
        // tTJSNativeClass; an instance class is a tjs2_native_class derived
        // from it).
        std::string name8(class_name_utf8);
        TJS::tTJSNativeClass *cls = nullptr;
        for(size_t i = 0; i < e->native_class_names.size(); i++) {
            if(e->native_class_names[i] == name8) {
                cls = static_cast<TJS::tTJSNativeClass *>(
                    e->native_classes[i]);
                break;
            }
        }
        if(!cls)
            return -2;
        std::u16string name16 = utf8_to_u16(class_name_utf8);
        ttstr clsname(name16.c_str());

        // TJS_STATICMEMBER mirrors TJS_END_NATIVE_STATIC_METHOD_DECL: the
        // member lives on the class object and tTJSNativeClass::FuncCall's
        // EnumMembers copy skips it, so instances never see it.
        for(int i = 0; i < count; i++) {
            const tjs2_native_method &m = methods[i];
            if(!m.name || !m.fn)
                return -3;
            std::u16string mname16 = utf8_to_u16(m.name);
            auto *dsp = new tjs2_native_method_dispatch(e, m.fn);
            cls->RegisterNCM(mname16.c_str(), dsp, clsname.c_str(),
                             TJS::nitMethod, TJS_STATICMEMBER);
        }
        for(int i = 0; i < prop_count; i++) {
            const tjs2_native_property &p = properties[i];
            if(!p.name || (!p.get && !p.set))
                return -3;
            std::u16string pname16 = utf8_to_u16(p.name);
            auto *dsp = new tjs2_native_property_dispatch(e, p.get, p.set);
            cls->RegisterNCM(pname16.c_str(), dsp, clsname.c_str(),
                             TJS::nitProperty, TJS_STATICMEMBER);
        }
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
// callable until the id is released. An OBJECT-typed value that carries a
// raw object handle (a native method argument marshalled by
// variant_to_value_one) retains that exact object; a handle-less OBJECT
// value falls back to the engine's last object-valued result (the value of
// the most recent exec/eval that yielded an object, e.g. eval'ing a script
// function's name). Returns NULL on failure.
tjs2_value_id tjs2_retain_value(void *engine, const tjs2_value *v) {
    tjs2_engine *e = (tjs2_engine *)engine;
    if(!e || !v)
        return nullptr;
    try {
        TJS::tTJSVariant var;
        if(!resolve_value_variant(e, v, var))
            return nullptr;
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

// Whether two variants are the same script value: scalar equality, or the
// same closure (Object + ObjThis) for objects. Used to match a function
// value registered by addContinuousHandler against the same function
// passed to removeContinuousHandler (retain allocates a fresh id per call,
// so raw-id comparison cannot work).
static bool tjs2_same_value(const TJS::tTJSVariant &a,
                            const TJS::tTJSVariant &b) {
    if(a.Type() != b.Type())
        return false;
    switch(a.Type()) {
        case TJS::tvtVoid:
            return true;
        case TJS::tvtInteger:
            return a.AsInteger() == b.AsInteger();
        case TJS::tvtReal:
            return a.AsReal() == b.AsReal();
        case TJS::tvtString: {
            TJS::ttstr sa(a), sb(b);
            return sa == sb;
        }
        case TJS::tvtObject: {
            TJS::tTJSVariantClosure ca = a.AsObjectClosureNoAddRef();
            TJS::tTJSVariantClosure cb = b.AsObjectClosureNoAddRef();
            return ca.Object == cb.Object && ca.ObjThis == cb.ObjThis;
        }
        default:
            return false;
    }
}

// Find the retained id of a value already in the engine's retained map
// (without retaining anything new). OBJECT-typed inputs resolve exactly like
// tjs2_retain_value (per-argument handle first, last_object fallback), then
// the map is scanned for a map entry that is the same script value. Returns
// NULL (the null id) when not found.
tjs2_value_id tjs2_find_retained_id(void *engine, const tjs2_value *v) {
    tjs2_engine *e = (tjs2_engine *)engine;
    if(!e || !v)
        return nullptr;
    try {
        TJS::tTJSVariant var;
        if(!resolve_value_variant(e, v, var))
            return nullptr;
        for(auto &kv : e->retained) {
            if(tjs2_same_value(kv.second, var))
                return (tjs2_value_id)kv.first;
        }
        return nullptr;
    } catch(...) {
        return nullptr;
    }
}

// Retain a raw TJS object (used by natives returning their own `this` or a
// related object — e.g. Window.primaryLayer returns the primary Layer
// object). Adds a reference into the engine's retained map and returns the
// id, which the caller should return as a TJS2_VAL_RETAINED result (the
// conversion consumes it).
// Stack trace string for Scripts.getTraceString (TJSGetStackTraceString).
// Returns a malloc'd UTF-8 string (free with tjs2_free_string) or NULL.
char *tjs2_get_stack_trace_string(void *engine, int limit) {
    (void)engine;
    try {
        TJS::ttstr s = TJS::TJSGetStackTraceString(limit);
        std::string u8 = u16_to_utf8(s.c_str());
        char *buf = (char *)malloc(u8.size() + 1);
        if(!buf)
            return nullptr;
        std::memcpy(buf, u8.c_str(), u8.size() + 1);
        return buf;
    } catch(...) {
        return nullptr;
    }
}

tjs2_value_id tjs2_retain_object(void *engine, void *obj) {
    tjs2_engine *e = (tjs2_engine *)engine;
    if(!e || !obj)
        return nullptr;
    try {
        uintptr_t id = e->next_retained_id++;
        if(id == 0)
            id = e->next_retained_id++;
        e->retained.emplace(id, TJS::tTJSVariant((TJS::iTJSDispatch2 *)obj,
                                                 (TJS::iTJSDispatch2 *)obj));
        return (tjs2_value_id)id;
    } catch(...) {
        // Never let a C++ exception (e.g. bad_alloc) cross the C ABI.
        return nullptr;
    }
}

// Diagnostic: the number of entries currently in the retained-value map.
// Used by the tjs2-sys test suite to prove retentions are consumed/released
// (no unbounded growth).
size_t tjs2_retained_count(void *engine) {
    tjs2_engine *e = (tjs2_engine *)engine;
    if(!e)
        return 0;
    return e->retained.size();
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
            out->array = nullptr;
            out->array_count = 0;
            out->retained = nullptr;
        }

        // Convert the arguments the same way the native-method dispatch
        // converts results (value_to_variant); object/octet arguments cannot
        // be reconstructed and raise a TJS error, matching the callback
        // return-value path. This runs BEFORE the retained-id lookup: the
        // conversion can mutate the retained map (a TJS2_VAL_RETAINED
        // argument consumes its entry), so no map iterator may be held
        // across it (a held iterator would dangle once the entry is
        // erased).
        std::vector<TJS::tTJSVariant> arg_vars;
        std::vector<TJS::tTJSVariant *> params;
        arg_vars.reserve((size_t)argc);
        params.reserve((size_t)argc);
        for(int i = 0; i < argc; i++) {
            arg_vars.emplace_back();
            value_to_variant(e, &argv[i], &arg_vars.back());
            params.push_back(&arg_vars.back());
        }

        auto it = e->retained.find((uintptr_t)id);
        if(it == e->retained.end()) {
            if(out_error)
                *out_error = make_error_string("invalid retained value");
            return 1;
        }

        TJS::tTJSVariant result;
        TJS::tTJSVariantClosure clo =
            it->second.AsObjectClosureNoAddRef(); // throws if not an object
        tjs_error hr = clo.FuncCall(0, nullptr, nullptr, &result, argc,
                                    params.empty() ? nullptr : params.data(),
                                    nullptr);
        if(TJS_FAILED(hr))
            TJSThrowFrom_tjs_error(hr, nullptr); // -> catch(eTJS) below

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

// Invoke a named member on a retained object value (e.g. the timer
// object's `onTimer`), the way the reference's event system dispatches
// (TimerIntf.cpp posts an "onTimer" event to the Timer object, whose
// handler then resolves to the script subclass's override). Member lookup
// goes through the object's own class chain, so a script subclass that
// overrides `onTimer` (like the game's OnceTimer) runs its override
// instead of the native method. Returns 0 on success and fills `out`;
// non-zero on failure with a malloc'd message in *out_error.
int tjs2_call_member(void *engine, tjs2_value_id id, const char *membername,
                     int argc, const tjs2_value *argv, tjs2_value *out,
                     char **out_error) {
    tjs2_engine *e = (tjs2_engine *)engine;
    if(!e || !id || !membername || argc < 0 || (argc > 0 && !argv)) {
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
            out->array = nullptr;
            out->array_count = 0;
            out->retained = nullptr;
        }

        std::vector<TJS::tTJSVariant> arg_vars;
        std::vector<TJS::tTJSVariant *> params;
        arg_vars.reserve((size_t)argc);
        params.reserve((size_t)argc);
        for(int i = 0; i < argc; i++) {
            arg_vars.emplace_back();
            value_to_variant(e, &argv[i], &arg_vars.back());
            params.push_back(&arg_vars.back());
        }

        auto it = e->retained.find((uintptr_t)id);
        if(it == e->retained.end()) {
            if(out_error)
                *out_error = make_error_string("invalid retained value");
            return 1;
        }

        std::u16string member16 = utf8_to_u16(membername);
        TJS::tTJSVariant result;
        TJS::tTJSVariantClosure clo =
            it->second.AsObjectClosureNoAddRef(); // throws if not an object
        tjs_error hr = clo.FuncCall(0, member16.c_str(), nullptr, &result,
                                    argc,
                                    params.empty() ? nullptr : params.data(),
                                    nullptr);
        if(TJS_FAILED(hr))
            TJSThrowFrom_tjs_error(hr, member16.c_str()); // -> catch(eTJS) below

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

// Read a named property from a retained object value through its class
// chain (PropGet). Used by natives that must resolve an object argument to
// one of its script-visible properties (e.g. Layer.parent = <Layer object>
// -> the parent's `id`). Same error convention as tjs2_call_member.
int tjs2_prop_get(void *engine, tjs2_value_id id, const char *membername,
                  tjs2_value *out, char **out_error) {
    tjs2_engine *e = (tjs2_engine *)engine;
    if(!e || !id || !membername) {
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
            out->array = nullptr;
            out->array_count = 0;
            out->retained = nullptr;
        }
        auto it = e->retained.find((uintptr_t)id);
        if(it == e->retained.end()) {
            if(out_error)
                *out_error = make_error_string("invalid retained value");
            return 1;
        }
        std::u16string member16 = utf8_to_u16(membername);
        TJS::tTJSVariant result;
        TJS::tTJSVariantClosure clo =
            it->second.AsObjectClosureNoAddRef(); // throws if not an object
        tjs_error hr = clo.PropGet(0, member16.c_str(), nullptr, &result,
                                   nullptr);
        if(TJS_FAILED(hr))
            TJSThrowFrom_tjs_error(hr, member16.c_str()); // -> catch below
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
        if(out_error)
            *out_error = make_error_string("prop_get failed");
        return 1;
    }
}

// Write a named property on a retained object value through its class chain
// (PropSet). TJS_MEMBERENSURE matches the VM's plain-assignment flag
// (tjsInterCodeExec.cpp:1182), so a missing member is created and an existing
// native/script property setter runs. The closure resolves objthis exactly
// like tjs2_prop_get/tjs2_call_member (tjsVariant.h:261-269). Same error
// convention as tjs2_prop_get.
int tjs2_prop_set(void *engine, tjs2_value_id id, const char *membername,
                  const tjs2_value *value, char **out_error) {
    tjs2_engine *e = (tjs2_engine *)engine;
    if(!e || !id || !membername || !value) {
        if(out_error)
            *out_error = make_error_string("invalid retained value");
        return 1;
    }
    try {
        // Validate the target before converting the value: conversion can
        // consume a TJS2_VAL_RETAINED entry (possibly the target itself), so
        // re-resolve the closure afterwards.
        if(e->retained.find((uintptr_t)id) == e->retained.end()) {
            if(out_error)
                *out_error = make_error_string("invalid retained value");
            return 1;
        }
        TJS::tTJSVariant var;
        value_to_variant(e, value, &var);
        auto it = e->retained.find((uintptr_t)id);
        if(it == e->retained.end()) {
            if(out_error)
                *out_error = make_error_string("invalid retained value");
            return 1;
        }
        std::u16string member16 = utf8_to_u16(membername);
        TJS::tTJSVariantClosure clo =
            it->second.AsObjectClosureNoAddRef(); // throws if not an object
        tjs_error hr = clo.PropSet(TJS_MEMBERENSURE, member16.c_str(), nullptr,
                                   &var, nullptr);
        if(TJS_FAILED(hr))
            TJSThrowFrom_tjs_error(hr, member16.c_str()); // -> catch below
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
        if(out_error)
            *out_error = make_error_string("prop_set failed");
        return 1;
    }
}

int tjs2_set_arg(tjs2_engine *e, int index, const tjs2_value *value,
                 char **out_error) {
    if(out_error)
        *out_error = nullptr;
    if(!e || !value) {
        if(out_error)
            *out_error = make_error_string("invalid out-param argument");
        return 1;
    }
    if(e->param_stack.empty()) {
        if(out_error)
            *out_error = make_error_string(
                "no native method call is active on this engine");
        return 1;
    }
    const tjs2_engine::tjs2_param_frame &frame = e->param_stack.back();
    if(index < 0 || index >= frame.count || !frame.param ||
       !frame.param[index]) {
        if(out_error)
            *out_error = make_error_string("out-param index out of range");
        return 1;
    }
    try {
        TJS::tTJSVariant var;
        value_to_variant(e, value, &var);
        // `tTJSVariant::operator=` handles AddRef/Release of the old and new
        // contents, exactly like the reference's `(*param[i]) = value`.
        *frame.param[index] = var;
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
        if(out_error)
            *out_error = make_error_string("set_arg failed");
        return 1;
    }
}

} // extern "C"
