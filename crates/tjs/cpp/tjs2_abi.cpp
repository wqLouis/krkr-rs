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
#include "tjsInterCodeExec.h"

#include "tjs2_abi.h"

#include <spdlog/spdlog.h>
#include <spdlog/sinks/base_sink.h>
#include <spdlog/sinks/stdout_color_sinks.h>

#include <atomic>
#include <memory>
#include <mutex>

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
    // Instance-capable classes registered with
    // tjs2_register_native_class_instance: UTF-8 name -> the process-wide
    // class id their objects register a tjs2_native_instance under. Only
    // that registration path creates tjs2_native_instance payloads, so
    // tjs2_get_native_instance may only static_cast native instances looked
    // up under these ids (never a builtin class id such as ClassID_Array,
    // whose native instance is a different C++ type).
    std::unordered_map<std::string, tjs_int32> native_instance_class_ids;
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

// ---------------------------------------------------------------------------
// spdlog -> engine log callback bridge
// ---------------------------------------------------------------------------

// The engine whose log callback receives the VM's spdlog records. spdlog
// loggers are process-wide while the callback is per engine; the compat
// binding is process-global and single, so at most one engine is active at a
// time. This is the most recently created engine that installed a callback.
// The pointer is atomic because the sink and the thread installing the
// callback need not be the same thread, and it is cleared before the engine
// (and its callback/user) is freed so the sink never dereferences a dead
// engine.
std::atomic<tjs2_engine *> g_log_engine{nullptr};

// Map a spdlog severity onto the ABI's coarser scale (no trace, one error
// level). A debug record stays debug so a run at `info` can ignore it.
int tjs2_level_for(spdlog::level::level_enum level) {
    switch(level) {
        case spdlog::level::trace:
        case spdlog::level::debug:
            return TJS2_LOG_DEBUG;
        case spdlog::level::info:
            return TJS2_LOG_INFO;
        case spdlog::level::warn:
            return TJS2_LOG_WARN;
        case spdlog::level::err:
        case spdlog::level::critical:
            return TJS2_LOG_ERROR;
        case spdlog::level::off:
        default:
            return TJS2_LOG_DEBUG;
    }
}

// A spdlog sink that forwards records to the active engine's log callback.
// Until an engine installs one (and after it is cleared) there is no
// destination, so records fall through to the stderr sink the VM used before
// this bridge existed. That keeps output visible, and because the logger has
// only this one sink the record is not also written to stderr once the
// callback is installed.
class Tjs2EngineLogSink : public spdlog::sinks::base_sink<std::mutex> {
    std::shared_ptr<spdlog::sinks::stderr_color_sink_mt> stderr_;

public:
    Tjs2EngineLogSink()
        : stderr_(std::make_shared<spdlog::sinks::stderr_color_sink_mt>()) {}

protected:
    void sink_it_(const spdlog::details::log_msg &msg) override {
        tjs2_engine *e = g_log_engine.load(std::memory_order_acquire);
        if(e && e->log_cb) {
            std::string text(msg.payload.data(), msg.payload.size());
            e->log_cb(tjs2_level_for(msg.level), text.c_str(), e->log_user);
            return;
        }
        stderr_->log(msg);
    }

    void flush_() override { stderr_->flush(); }
};

// The reference main() registers the spdlog loggers the tjs2 code expects
// (`spdlog::get("tjs2")` etc.). Register them once per process, all backed by
// the bridge sink above.
static void ensure_spdlog_loggers() {
    static std::once_flag once;
    std::call_once(once, [] {
        auto sink = std::make_shared<Tjs2EngineLogSink>();
        if(!spdlog::get("tjs2"))
            spdlog::register_logger(
                std::make_shared<spdlog::logger>("tjs2", sink));
        if(!spdlog::get("core"))
            spdlog::register_logger(
                std::make_shared<spdlog::logger>("core", sink));
        if(!spdlog::get("plugin"))
            spdlog::register_logger(
                std::make_shared<spdlog::logger>("plugin", sink));
        spdlog::set_default_logger(spdlog::get("core"));
        // Set the level after registration: a logger registered directly
        // (not through a factory) keeps its constructor default of `info`,
        // and `set_level` is what pushes `debug` down to every registered
        // logger and the global level for later ones.
        spdlog::set_level(spdlog::level::debug);
    });
}

// Convert UTF-8 -> tjs_char (char16_t on this build).
std::u16string utf8_to_u16(const char *s) {
    return boost::locale::conv::utf_to_utf<char16_t>(s);
}

// Convert a UTF-8 byte range -> tjs_char. The range form keeps an interior
// NUL: the single-argument overload stops at the first NUL, so a script
// supplied as a pointer and a length must use this one.
std::u16string utf8_to_u16(const char *s, size_t len) {
    if(!s || len == 0)
        return std::u16string();
    return boost::locale::conv::utf_to_utf<char16_t>(s, s + len);
}

// Convert tjs_char (UTF-16 code units on this build) -> UTF-8 std::string.
//
// This must NOT go through ttstr::AsStdString(): that narrows via
// TJS_wcstombs -> utf8_wctomb (tjsConfig.cpp), which encodes each UTF-16
// code unit on its own. A surrogate pair — every non-BMP character, e.g.
// U+1F600 — would then be emitted as CESU-8 (two 3-byte sequences encoding
// lone surrogates), which is invalid UTF-8 at the Rust boundary and gets
// mangled into replacement characters by String::from_utf8_lossy. Decode
// surrogate pairs here and emit proper 4-byte UTF-8; an unpaired surrogate
// becomes U+FFFD, like any other lossy UTF-16 -> UTF-8 conversion.
std::string u16_to_utf8(const tjs_char *s) {
    std::string out;
    if(!s)
        return out;
    out.reserve(std::char_traits<tjs_char>::length(s));
    for(const tjs_char *p = s; *p; ++p) {
        uint32_t cp = static_cast<uint32_t>(*p);
        if(cp >= 0xD800 && cp <= 0xDBFF) {
            // High surrogate: combine with a following low surrogate
            // (peeking at most one char; p[1] is at worst the terminator).
            uint32_t low = static_cast<uint32_t>(p[1]);
            if(low >= 0xDC00 && low <= 0xDFFF) {
                cp = 0x10000u + ((cp - 0xD800u) << 10) + (low - 0xDC00u);
                ++p;
            } else {
                cp = 0xFFFD; // unpaired high surrogate
            }
        } else if(cp >= 0xDC00 && cp <= 0xDFFF) {
            cp = 0xFFFD; // unpaired low surrogate
        }
        if(cp < 0x80) {
            out.push_back(static_cast<char>(cp));
        } else if(cp < 0x800) {
            out.push_back(static_cast<char>(0xC0 | (cp >> 6)));
            out.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
        } else if(cp < 0x10000) {
            out.push_back(static_cast<char>(0xE0 | (cp >> 12)));
            out.push_back(static_cast<char>(0x80 | ((cp >> 6) & 0x3F)));
            out.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
        } else {
            out.push_back(static_cast<char>(0xF0 | (cp >> 18)));
            out.push_back(static_cast<char>(0x80 | ((cp >> 12) & 0x3F)));
            out.push_back(static_cast<char>(0x80 | ((cp >> 6) & 0x3F)));
            out.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
        }
    }
    return out;
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
            // Not AsStdString(): that produces CESU-8 for non-BMP characters
            // (see u16_to_utf8 above).
            e->last_string = u16_to_utf8(s.c_str());
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
        std::string msg = u16_to_utf8(e.getMessage().c_str());
        // Script errors carry the block name and source line; surface them so
        // a host-side failure names the failing script, not just the message.
        if(const auto *se = dynamic_cast<const TJS::eTJSScriptError *>(&e)) {
            try {
                std::string where = u16_to_utf8(se->GetBlockName());
                where += "(";
                where += std::to_string(se->GetSourceLine());
                where += ")";
                msg = where + ": " + msg;
            } catch(...) {
            }
        }
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
            // Not AsStdString(): that produces CESU-8 for non-BMP characters
            // (see u16_to_utf8 above).
            storage = u16_to_utf8(s.c_str());
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
    ~tjs2_param_frame_guard() {
        engine->param_stack.pop_back();
        // Drop the argument-derived object slot. `variant_to_value_one`
        // records the last object argument so a handle-less
        // `TjsValue::Object` can resolve against it *during* the callback;
        // keeping it afterwards pins that object (and the whole scene it
        // roots) alive across calls, defeating the reference-counted
        // teardown. Results (`exec`/`eval`) still populate `last_object` for
        // `tjs2_retain_value` after those calls.
        engine->last_object.Clear();
    }
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
        // The write argument is no longer needed for handle-less object
        // resolution (see the parameter-frame guard).
        e->last_object.Clear();

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
// objthis-only member dispatch (NCB_ATTACH_CLASS / NCB_ATTACH_FUNCTION)
// ---------------------------------------------------------------------------
//
// Members attached to an existing class by
// tjs2_register_native_instance_members. Unlike the instance dispatch below,
// the Rust callbacks receive no native-instance payload: they get `objthis`,
// the object the member was invoked on. `objthis` may be an object of a
// builtin class (Array, Dictionary, WaveSoundBuffer, ...) whose class object
// the registration found through the global object.

class tjs2_objthis_method_dispatch;

// Forward declaration: the dispatch class below calls this from its FuncCall
// body (compiled as if right after the class).
tjs_error tjs2_dispatch_objthis_method(tjs2_objthis_method_dispatch *self,
                                       tTJSVariant *result,
                                       tjs_int numparams,
                                       tTJSVariant **param,
                                       iTJSDispatch2 *objthis);

// A per-method dispatch object carrying the engine and the Rust callback.
// FuncCall forwards the call, passing objthis through unchanged; it does not
// need a class id because there is no per-object payload to look up.
class tjs2_objthis_method_dispatch : public TJS::tTJSNativeClassMethod {
    typedef TJS::tTJSNativeClassMethod inherited;

public:
    tjs2_engine *engine;
    tjs2_native_objthis_method_fn fn;

    tjs2_objthis_method_dispatch(tjs2_engine *e,
                                 tjs2_native_objthis_method_fn f)
        : inherited(nullptr), engine(e), fn(f) {}

    tjs_error FuncCall(tjs_uint32 flag, const tjs_char *membername,
                       tjs_uint32 *hint, tTJSVariant *result,
                       tjs_int numparams, tTJSVariant **param,
                       iTJSDispatch2 *objthis) override {
        if(membername)
            return inherited::FuncCall(flag, membername, hint, result,
                                       numparams, param, objthis);
        return tjs2_dispatch_objthis_method(this, result, numparams, param,
                                            objthis);
    }
};

// Dispatch an objthis-only method call to the Rust callback and convert the
// result back into a tTJSVariant (same marshalling as the static dispatch).
tjs_error tjs2_dispatch_objthis_method(tjs2_objthis_method_dispatch *self,
                                       tTJSVariant *result,
                                       tjs_int numparams,
                                       tTJSVariant **param,
                                       iTJSDispatch2 *objthis) {
    tjs2_engine *e = self->engine;
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
        // (objthis/argv/out valid only during the call, out_error malloc'd).
        int rc = self->fn(e, (void *)objthis, (int)numparams,
                          argv.empty() ? nullptr : argv.data(), &out,
                          &out_error);

        if(rc != 0) {
            std::string msg =
                out_error ? out_error
                          : "native instance member reported an error";
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

class tjs2_objthis_property_dispatch;

tjs_error tjs2_dispatch_objthis_property_get(
    tjs2_objthis_property_dispatch *self, tTJSVariant *result,
    iTJSDispatch2 *objthis);
tjs_error tjs2_dispatch_objthis_property_set(
    tjs2_objthis_property_dispatch *self, const tTJSVariant *param,
    iTJSDispatch2 *objthis);

// A per-property dispatch object, mirroring the static property dispatch but
// passing objthis to the Rust callbacks instead of the class object.
class tjs2_objthis_property_dispatch : public TJS::tTJSNativeClassProperty {
    typedef TJS::tTJSNativeClassProperty inherited;

public:
    tjs2_engine *engine;
    tjs2_native_objthis_property_get_fn get; // may be nullptr (write-only)
    tjs2_native_objthis_property_set_fn set; // may be nullptr (read-only)

    tjs2_objthis_property_dispatch(tjs2_engine *e,
                                   tjs2_native_objthis_property_get_fn g,
                                   tjs2_native_objthis_property_set_fn s)
        : inherited(nullptr, nullptr), engine(e), get(g), set(s) {}

    tjs_error PropGet(tjs_uint32 flag, const tjs_char *membername,
                      tjs_uint32 *hint, tTJSVariant *result,
                      iTJSDispatch2 *objthis) override {
        if(membername)
            return inherited::PropGet(flag, membername, hint, result, objthis);
        return tjs2_dispatch_objthis_property_get(this, result, objthis);
    }

    tjs_error PropSet(tjs_uint32 flag, const tjs_char *membername,
                      tjs_uint32 *hint, const tTJSVariant *param,
                      iTJSDispatch2 *objthis) override {
        if(membername)
            return inherited::PropSet(flag, membername, hint, param, objthis);
        return tjs2_dispatch_objthis_property_set(this, param, objthis);
    }
};

tjs_error tjs2_dispatch_objthis_property_get(
    tjs2_objthis_property_dispatch *self, tTJSVariant *result,
    iTJSDispatch2 *objthis) {
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
        int rc = self->get(e, (void *)objthis, &out, &out_error);

        if(rc != 0) {
            std::string msg =
                out_error ? out_error
                          : "native instance property get reported an error";
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

tjs_error tjs2_dispatch_objthis_property_set(
    tjs2_objthis_property_dispatch *self, const tTJSVariant *param,
    iTJSDispatch2 *objthis) {
    tjs2_engine *e = self->engine;
    try {
        if(!self->set)
            return TJS_E_ACCESSDENYED;

        tjs2_value value;
        std::string storage;
        variant_to_value_one(e, *param, &value, storage);

        char *out_error = nullptr;
        int rc = self->set(e, (void *)objthis, &value, &out_error);
        // The write argument is no longer needed for handle-less object
        // resolution (see the parameter-frame guard).
        e->last_object.Clear();

        if(rc != 0) {
            std::string msg =
                out_error ? out_error
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
    tjs2_native_invalidate_instance_fn invalidate;
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
                         tjs2_native_destroy_instance_fn d,
                         tjs2_native_invalidate_instance_fn inv)
        : engine(e), destroy(d), invalidate(inv), native_ptr(nullptr),
          valid(false), invalidated(false) {
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
    //
    // An optional Rust `invalidate` callback runs the native teardown now
    // (reference concrete `Invalidate` bodies): it detaches the object from
    // its owner and releases the resources it holds, breaking the
    // script/native reference cycle so the object can reach refcount 0. The
    // payload itself stays allocated for the later destructor. Direct
    // dispatch stays enabled while the callback runs — the reference
    // teardown fires script handlers from inside it (a layer's `Invalidate`
    // reaches `Part()` -> `BlurTree` -> `FireBlur`/`FireFocus`,
    // reference/cpp/core/visual/LayerIntf.cpp:515-544) and those handlers
    // read the object's native properties — and stops only once the
    // callback returns. The object-level `IsInvalidating` guard in
    // `tTJSCustomObject::_Finalize` keeps the callback single-shot under
    // re-entry (tjsObject.cpp:372-386).
    void Invalidate() override {
        if(!invalidated) {
            if(invalidate && native_ptr)
                invalidate(engine, native_ptr);
            invalidated = true; // stop direct dispatch once teardown returned
        }
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
        // The write argument is no longer needed for handle-less object
        // resolution (see the parameter-frame guard).
        e->last_object.Clear();

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
    tjs2_native_invalidate_instance_fn invalidate_instance;

public:
    tjs2_native_class(const ttstr &name, tjs2_engine *e,
                      tjs2_native_create_instance_fn c,
                      tjs2_native_destroy_instance_fn d,
                      tjs2_native_invalidate_instance_fn inv)
        : inherited(name), engine(e), create_instance(c), destroy_instance(d),
          invalidate_instance(inv) {}

    // Public accessor for the constructor dispatch (CreateNativeInstance is
    // protected in the base class).
    TJS::iTJSNativeInstance *NewInstance() { return CreateNativeInstance(); }

protected:
    TJS::iTJSNativeInstance *CreateNativeInstance() override {
        return new tjs2_native_instance(engine, create_instance,
                                        destroy_instance,
                                        invalidate_instance);
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
            // Console output (script `print`, and the VM's per-exception
            // disassembly dump) is diagnostic: DEBUG, so a default `info` run
            // stays quiet and `RUST_LOG=debug` shows it.
            std::string s = u16_to_utf8(msg);
            e_->log_cb(TJS2_LOG_DEBUG, s.c_str(), e_->log_user);
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

// EnumMembers callback (reference `DictMemberGetCaller` in scriptsEx.cpp):
// each call receives param[0] = member name, param[1] = member flags;
// TJS_ENUM_NO_VALUE suppresses param[2] (the value), so the collector only
// needs the name. Hidden members are dropped, matching Scripts.getObjectKeys.
class enum_member_collector : public TJS::tTJSDispatch {
public:
    explicit enum_member_collector(TJS::iTJSDispatch2 *array) : array_(array) {}

    tjs_error FuncCall(tjs_uint32 flag, const tjs_char *membername,
                       tjs_uint32 *hint, TJS::tTJSVariant *result,
                       tjs_int numparams, TJS::tTJSVariant **param,
                       TJS::iTJSDispatch2 *objthis) override {
        (void)flag;
        (void)membername;
        (void)hint;
        (void)objthis;
        if(numparams > 1) {
            tTVInteger member_flags = param[1]->AsInteger();
            if(!(member_flags & TJS_HIDDENMEMBER)) {
                static tjs_uint add_hint = 0;
                array_->FuncCall(0, TJS_W("add"), &add_hint, nullptr, 1,
                                 &param[0], array_);
            }
        }
        if(result)
            *result = true;
        return TJS_S_OK;
    }

private:
    TJS::iTJSDispatch2 *array_;
};

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
    // The spdlog bridge may still point at this engine; clear it before the
    // engine (and its callback/user) is freed.
    if(g_log_engine.load(std::memory_order_acquire) == e)
        g_log_engine.store(nullptr, std::memory_order_release);
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
    // Newest installed callback wins; clearing the active engine's callback
    // detaches it so the bridge falls back to stderr.
    if(cb)
        g_log_engine.store(e, std::memory_order_release);
    else if(g_log_engine.load(std::memory_order_acquire) == e)
        g_log_engine.store(nullptr, std::memory_order_release);
}

int tjs2_exec_script(tjs2_engine *e, const char *script, size_t script_len,
                     const char *name, void *context, int lineofs,
                     tjs2_value *out_result, char **out_error) {
    if(!e || (!script && script_len != 0)) {
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
        std::u16string s = utf8_to_u16(script, script_len);
        std::u16string n = name ? utf8_to_u16(name) : std::u16string();
        // The length-aware ttstr form keeps an interior NUL in the payload;
        // the VM's own text handling decides what that NUL means.
        TJS::ttstr script_text(s.c_str(), s.size());
        TJS::ttstr script_name(n.c_str(), n.size());
        e->inner->ExecScript(script_text, &result,
                             static_cast<TJS::iTJSDispatch2 *>(context),
                             name ? &script_name : nullptr, (tjs_int)lineofs);
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

int tjs2_eval(tjs2_engine *e, const char *expression, size_t expression_len,
              const char *name, void *context, int lineofs,
              tjs2_value *out_result, char **out_error) {
    if(!e || (!expression && expression_len != 0)) {
        if(out_error)
            *out_error = nullptr;
        return -1;
    }
    e->last_object.Clear(); // see tjs2_exec_script
    try {
        TJS::tTJSVariant result;
        std::u16string s = utf8_to_u16(expression, expression_len);
        std::u16string n = name ? utf8_to_u16(name) : std::u16string();
        if(name && n.empty()) {
            // An empty (but present) name is still a name to the reference's
            // raw entry point (`if(name)`), which selects a named block
            // rather than the expression cache; keep that path.
            e->inner->EvalExpression(
                s.c_str(), &result,
                static_cast<TJS::iTJSDispatch2 *>(context), n.c_str(),
                (tjs_int)lineofs);
        } else {
            TJS::ttstr expression_text(s.c_str(), s.size());
            TJS::ttstr expression_name(n.c_str(), n.size());
            e->inner->EvalExpression(expression_text, &result,
                                     static_cast<TJS::iTJSDispatch2 *>(context),
                                     name ? &expression_name : nullptr,
                                     (tjs_int)lineofs);
        }
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

extern "C" int tjs2_assign_message_checked(const char *name,
                                          const char *message,
                                          char **out_error) {
    if(out_error)
        *out_error = nullptr;
    if(!name || !message)
        return 0;
    try {
        std::u16string n = utf8_to_u16(name);
        std::u16string m = utf8_to_u16(message);
        return TJS::TJSAssignMessage(n.c_str(), m.c_str()) ? 1 : 0;
    } catch(const TJS::eTJS &err) {
        if(out_error)
            *out_error = make_error_message(err);
        return -1;
    } catch(const std::exception &err) {
        if(out_error) {
            std::string m = std::string("C++ exception: ") + err.what();
            char *buf = (char *)malloc(m.size() + 1);
            if(buf)
                std::memcpy(buf, m.c_str(), m.size() + 1);
            *out_error = buf;
        }
        return -1;
    } catch(...) {
        if(out_error) {
            char *buf = (char *)malloc(8);
            if(buf)
                std::memcpy(buf, "unknown", 8);
            *out_error = buf;
        }
        return -1;
    }
}

int tjs2_assign_message(const char *name, const char *message) {
    return tjs2_assign_message_checked(name, message, nullptr);
}

// Guarded wrapper over the streams.cpp data-dir setter. The setter assigns a
// std::string (which can throw std::bad_alloc); this keeps the exception on
// the C++ side so it cannot cross the C ABI.
extern "C" int tjs2_set_data_dir_checked(const char *dir, char **out_error) {
    if(out_error)
        *out_error = nullptr;
    try {
        tjs2_set_data_dir(dir);
        return 0;
    } catch(const std::exception &err) {
        if(out_error) {
            std::string m = std::string("C++ exception: ") + err.what();
            char *buf = (char *)malloc(m.size() + 1);
            if(buf)
                std::memcpy(buf, m.c_str(), m.size() + 1);
            *out_error = buf;
        }
        return -1;
    } catch(...) {
        if(out_error) {
            char *buf = (char *)malloc(8);
            if(buf)
                std::memcpy(buf, "unknown", 8);
            *out_error = buf;
        }
        return -1;
    }
}

const char *tjs2_tjs_version_string(void) {
    static const std::string version = [] {
        return std::to_string(TJS::TJSVersionMajor) + "." +
               std::to_string(TJS::TJSVersionMinor) + "." +
               std::to_string(TJS::TJSVersionRelease);
    }();
    return version.c_str();
}

const char *tjs2_compile_date(void) { return __DATE__; }

const char *tjs2_compile_time(void) { return __TIME__; }

int tjs2_compile_script(tjs2_engine *e, const char *script, size_t script_len,
                        const char *output_path, int isresult,
                        int outputdebug, int isexpression, const char *name,
                        int lineofs, char **out_error) {
    if(out_error)
        *out_error = nullptr;
    if(!e || (!script && script_len != 0) || !output_path)
        return -1;
    try {
        std::u16string s = utf8_to_u16(script, script_len);
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
            // CompileScript takes the script as a NUL-terminated tjs_char*;
            // feed it the full-length buffer so the VM, not this wrapper,
            // owns the interpretation of any interior NUL.
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
        // `DoGarbageCollection` calls `TJSVariantArrayStackCompactNow()`, but
        // the vendored VM's implementation of that is an empty stub (see
        // `tjsInterCodeExec.cpp`), so the variant-array stack's stale
        // register slots are never cleared. Those slots keep a strong
        // reference to values from completed calls (notably `Layer` wrapper
        // objects whose script reference was dropped), which kept their
        // native instances alive and meant `layer_destroy` never ran across
        // repeated scene loads. Compact the stack here, through its public
        // `Compact()` (`InternalCompact`), so this GC pass actually releases
        // those references. This is the teardown the reference's GC relies
        // on: it clears every slot above the live register mark.
        if(e->inner->GetVariantArrayStack())
            e->inner->GetVariantArrayStack()->Compact();
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

// Resolve a retained object to the Rust payload of an instance-based native
// class: the same NativeInstanceSupport(TJS_NIS_GETINSTANCE, classid) ->
// GetNativePtr() lookup the instance method/property dispatchers perform
// (tjs2_dispatch_native_instance_method), except a miss is reported as NULL
// instead of a TJS error. See tjs2_abi.h for the full contract.
void *tjs2_get_native_instance(tjs2_engine *e, tjs2_value_id obj,
                               const char *class_name_utf8) {
    if(!e || !obj || !class_name_utf8)
        return nullptr;
    try {
        TJS::iTJSDispatch2 *target = resolve_retained_object(e, obj);
        if(!target)
            return nullptr; // unknown id, non-object value, or TJS null
        auto cls =
            e->native_instance_class_ids.find(std::string(class_name_utf8));
        if(cls == e->native_instance_class_ids.end())
            return nullptr; // not an instance-capable class of this engine
        TJS::iTJSNativeInstance *native = nullptr;
        tjs_error hr = target->NativeInstanceSupport(TJS_NIS_GETINSTANCE,
                                                      cls->second, &native);
        if(TJS_FAILED(hr) || !native)
            return nullptr; // not an instance (or the class object itself)
        // Same static_cast as the dispatchers: every native instance
        // registered under this class id is a tjs2_native_instance.
        // GetNativePtr() is null once the instance was invalidated.
        return static_cast<tjs2_native_instance *>(native)->GetNativePtr();
    } catch(...) {
        // Never let a C++ exception cross the C ABI.
        return nullptr;
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

int tjs2_enum_members(tjs2_engine *e, tjs2_value_id obj, tjs2_value *out,
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
        // Reference ScriptsAdd::_getKeys: EnumMembers with
        // TJS_IGNOREPROP | TJS_ENUM_NO_VALUE. The class object enumerates its
        // own members plus the inherited ones reachable through its class
        // chain, which is exactly the observable script surface.
        TJS::iTJSDispatch2 *array = TJS::TJSCreateArrayObject();
        if(!array) {
            if(out_error)
                *out_error = make_error_string("failed to create the member array");
            return 1;
        }
        try {
            enum_member_collector collector(array);
            TJS::tTJSVariantClosure closure(&collector,
                                            (TJS::iTJSDispatch2 *)nullptr);
            dsp->EnumMembers(TJS_IGNOREPROP | TJS_ENUM_NO_VALUE, &closure,
                             nullptr);
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
    tjs2_native_destroy_instance_fn destroy_instance,
    tjs2_native_invalidate_instance_fn invalidate_instance) {
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
            new tjs2_native_class(clsname, e, create_instance, destroy_instance,
                                  invalidate_instance);
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
        e->native_instance_class_ids.emplace(name8, classid);
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

int tjs2_register_native_instance_members(
    tjs2_engine *e, const char *class_name_utf8,
    const tjs2_native_objthis_method *methods, int count,
    const tjs2_native_objthis_property *properties, int prop_count) {
    if(!e || !class_name_utf8 || count < 0 || (count > 0 && !methods) ||
       prop_count < 0 || (prop_count > 0 && !properties))
        return -1;
    try {
        std::string name8(class_name_utf8);
        std::u16string name16 = utf8_to_u16(class_name_utf8);
        ttstr clsname(name16.c_str());

        // If the class is one we registered, add the members as ordinary
        // (non-static) instance members: tTJSNativeClass::FuncCall copies
        // them onto every created object. This mirrors
        // tjs2_register_native_static_members without TJS_STATICMEMBER.
        TJS::tTJSNativeClass *cls = nullptr;
        for(size_t i = 0; i < e->native_class_names.size(); i++) {
            if(e->native_class_names[i] == name8) {
                cls = static_cast<TJS::tTJSNativeClass *>(
                    e->native_classes[i]);
                break;
            }
        }
        if(cls) {
            for(int i = 0; i < count; i++) {
                const tjs2_native_objthis_method &m = methods[i];
                if(!m.name || !m.fn)
                    return -3;
                std::u16string mname16 = utf8_to_u16(m.name);
                auto *dsp = new tjs2_objthis_method_dispatch(e, m.fn);
                cls->RegisterNCM(mname16.c_str(), dsp, clsname.c_str(),
                                 TJS::nitMethod);
            }
            for(int i = 0; i < prop_count; i++) {
                const tjs2_native_objthis_property &p = properties[i];
                if(!p.name || (!p.get && !p.set))
                    return -3;
                std::u16string pname16 = utf8_to_u16(p.name);
                auto *dsp =
                    new tjs2_objthis_property_dispatch(e, p.get, p.set);
                cls->RegisterNCM(pname16.c_str(), dsp, clsname.c_str(),
                                 TJS::nitProperty);
            }
            return 0;
        }

        // Otherwise it is a builtin: find the class object through the VM
        // global object (the same lookup NCB_ATTACH_CLASS performs in its
        // GetGlobalObject, ncbind.hpp) and PropSet the members onto it. The
        // class object is owned by the global object, so no reference is
        // taken or released here; the PropSet context is the class object
        // itself, so a non-static member lands on the class and is reachable
        // from its instances.
        iTJSDispatch2 *global = e->inner->GetGlobalNoAddRef();
        if(!global)
            return -2;
        TJS::tTJSVariant class_val;
        tjs_error hr =
            global->PropGet(0, name16.c_str(), nullptr, &class_val, global);
        if(TJS_FAILED(hr))
            return -2;
        iTJSDispatch2 *classobj = class_val.AsObjectNoAddRef();
        if(!classobj)
            return -2;

        // Wrap the dispatch in a variant (AddRef), drop the creation
        // reference, and PropSet it onto the class object: the same pattern
        // as ncbAttachTJS2Class::RegistVariant (ncbind.hpp).
        for(int i = 0; i < count; i++) {
            const tjs2_native_objthis_method &m = methods[i];
            if(!m.name || !m.fn)
                return -3;
            std::u16string mname16 = utf8_to_u16(m.name);
            auto *dsp = new tjs2_objthis_method_dispatch(e, m.fn);
            TJS::tTJSVariant mval(static_cast<TJS::iTJSDispatch2 *>(dsp));
            dsp->Release();
            hr = classobj->PropSet(TJS_MEMBERENSURE, mname16.c_str(), nullptr,
                                   &mval, classobj);
            if(TJS_FAILED(hr))
                return -6;
        }
        for(int i = 0; i < prop_count; i++) {
            const tjs2_native_objthis_property &p = properties[i];
            if(!p.name || (!p.get && !p.set))
                return -3;
            std::u16string pname16 = utf8_to_u16(p.name);
            auto *dsp = new tjs2_objthis_property_dispatch(e, p.get, p.set);
            TJS::tTJSVariant pval(static_cast<TJS::iTJSDispatch2 *>(dsp));
            dsp->Release();
            hr = classobj->PropSet(TJS_MEMBERENSURE, pname16.c_str(), nullptr,
                                   &pval, classobj);
            if(TJS_FAILED(hr))
                return -6;
        }
        return 0;
    } catch(...) {
        return -4;
    }
}

int tjs2_register_native_builtin_static_members(
    tjs2_engine *e, const char *class_name_utf8,
    const tjs2_native_objthis_method *methods, int count,
    const tjs2_native_objthis_property *properties, int prop_count) {
    if(!e || !class_name_utf8 || count < 0 || (count > 0 && !methods) ||
       prop_count < 0 || (prop_count > 0 && !properties))
        return -1;
    try {
        std::u16string name16 = utf8_to_u16(class_name_utf8);

        // Same class-object lookup as the instance registration above.
        iTJSDispatch2 *global = e->inner->GetGlobalNoAddRef();
        if(!global)
            return -2;
        TJS::tTJSVariant class_val;
        tjs_error hr =
            global->PropGet(0, name16.c_str(), nullptr, &class_val, global);
        if(TJS_FAILED(hr))
            return -2;
        iTJSDispatch2 *classobj = class_val.AsObjectNoAddRef();
        if(!classobj)
            return -2;

        // A static member lives on the class object and is never copied onto
        // an instance (tTJSNativeClass::FuncCall copies the non-static
        // symbols only, tjsNative.cpp), so an instance's EnumMembers does
        // not report it. Its receiver is the global object
        // (ncbAttachTJS2Class::RegistVariant, ncbind.hpp).
        for(int i = 0; i < count; i++) {
            const tjs2_native_objthis_method &m = methods[i];
            if(!m.name || !m.fn)
                return -3;
            std::u16string mname16 = utf8_to_u16(m.name);
            auto *dsp = new tjs2_objthis_method_dispatch(e, m.fn);
            TJS::tTJSVariant mval(static_cast<TJS::iTJSDispatch2 *>(dsp));
            dsp->Release();
            hr = classobj->PropSet(TJS_MEMBERENSURE | TJS_STATICMEMBER,
                                   mname16.c_str(), nullptr, &mval, global);
            if(TJS_FAILED(hr))
                return -6;
        }
        for(int i = 0; i < prop_count; i++) {
            const tjs2_native_objthis_property &p = properties[i];
            if(!p.name || (!p.get && !p.set))
                return -3;
            std::u16string pname16 = utf8_to_u16(p.name);
            auto *dsp = new tjs2_objthis_property_dispatch(e, p.get, p.set);
            TJS::tTJSVariant pval(static_cast<TJS::iTJSDispatch2 *>(dsp));
            dsp->Release();
            hr = classobj->PropSet(TJS_MEMBERENSURE | TJS_STATICMEMBER,
                                   pname16.c_str(), nullptr, &pval, global);
            if(TJS_FAILED(hr))
                return -6;
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

// Duplicate an existing retained-value id: the variant is copied into a fresh
// id (AddRef'ing its contents) while the original entry stays live. Used to
// hand a cached object to a native result slot without consuming the cache
// (e.g. the cached `Layer.font` wrapper).
tjs2_value_id tjs2_retain_retained_id(void *engine, tjs2_value_id id) {
    tjs2_engine *e = (tjs2_engine *)engine;
    if(!e || !id)
        return nullptr;
    try {
        auto it = e->retained.find((uintptr_t)id);
        if(it == e->retained.end())
            return nullptr;
        return retain_variant(e, it->second);
    } catch(...) {
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

// Invalidate a retained object value (the reference `invalidate obj;`
// statement): invokes the object's iTJSDispatch2::Invalidate so its script
// `finalize` runs. Returns 0 on success, 1 on an unknown id or an exception.
// The retention is left in place; release it with tjs2_release_value.
int tjs2_invalidate_retained(void *engine, tjs2_value_id id) {
    tjs2_engine *e = (tjs2_engine *)engine;
    if(!e || !id)
        return 1;
    try {
        auto it = e->retained.find((uintptr_t)id);
        if(it == e->retained.end())
            return 1;
        TJS::tTJSVariantClosure clo = it->second.AsObjectClosureNoAddRef();
        if(!clo.Object)
            return 1;
        clo.Invalidate(0, nullptr, nullptr, nullptr);
        return 0;
    } catch(...) {
        return 1;
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

// Delete a named property from a retained object value (reference
// `iTJSDispatch2::DeleteMember`, tjsObject.h:472; the tTJSCustomObject
// implementation tjsObject.cpp:1537-1552 delegates to DeleteByName at
// tjsObject.cpp:759). Flag 0 and objthis = the dispatch object match the
// VM's own `delete` operator (tjsInterCodeExec.cpp:1826), so only the
// object's own member table is affected. An absent member is the reference's
// TJS_E_MEMBERNOTFOUND (tjsObject.cpp:1546), which the `delete` operator
// turns into the boolean `false` rather than an exception
// (tjsInterCodeExec.cpp:1829-1833); this entry point reports it as a typed
// failure. Same error convention as tjs2_prop_get.
int tjs2_prop_delete(tjs2_engine *e, tjs2_value_id obj,
                     const char *membername_utf8, char **out_error) {
    if(!e || !obj || !membername_utf8) {
        if(out_error)
            *out_error = make_error_string("invalid retained value");
        return 1;
    }
    try {
        auto it = e->retained.find((uintptr_t)obj);
        if(it == e->retained.end()) {
            if(out_error)
                *out_error = make_error_string("invalid retained value");
            return 1;
        }
        std::u16string member16 = utf8_to_u16(membername_utf8);
        // AsObjectNoAddRef throws for a non-object; TJS `null` is an object
        // value with no object pointer, so it comes back as null here.
        TJS::iTJSDispatch2 *dsp = it->second.AsObjectNoAddRef();
        if(!dsp) {
            if(out_error)
                *out_error = make_error_string("not an object");
            return 1;
        }
        tjs_error hr = dsp->DeleteMember(0, member16.c_str(), nullptr, dsp);
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
            *out_error = make_error_string("prop_delete failed");
        return 1;
    }
}

int tjs2_prop_set_default(void *engine, tjs2_value_id id,
                          const tjs2_value *value, char **out_error) {
    tjs2_engine *e = (tjs2_engine *)engine;
    if(!e || !id || !value) {
        if(out_error)
            *out_error = make_error_string("invalid retained value");
        return 1;
    }
    try {
        // Validate the target before converting the value: conversion can
        // consume a TJS2_VAL_RETAINED entry, so re-resolve afterwards.
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
        TJS::tTJSVariantClosure clo =
            it->second.AsObjectClosureNoAddRef(); // throws if not an object
        tjs_error hr =
            clo.PropSet(TJS_MEMBERENSURE, nullptr, nullptr, &var, nullptr);
        if(TJS_FAILED(hr))
            TJSThrowFrom_tjs_error(hr, nullptr); // -> catch below
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
            *out_error = make_error_string("prop_set_default failed");
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
