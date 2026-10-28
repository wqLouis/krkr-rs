// C ABI boundary between Rust (krkr-rs) and the C++ TJS2 scripting VM.
// The C++ implementation lives in tjs2_abi.cpp; this header is mirrored
// manually in crates/tjs2-sys/src/lib.rs (the surface is small and stable,
// no bindgen needed).
#ifndef TJS2_ABI_H
#define TJS2_ABI_H

#include <vector>

#include <stddef.h> /* size_t */
#include <stdint.h> /* uint16_t */

#ifdef __cplusplus
namespace TJS {
class tTJSBinaryStream;
}
/*
 * Create a read-only binary stream over `len` bytes of `data` (which stays
 * borrowed, not copied, and must outlive the stream). The caller owns the
 * returned stream (`delete` it). This is the in-memory stream the VM's
 * stream-based `LoadByteCode` runs on; the file-backed stream factory cannot
 * be used because the bytes are already in memory.
 */
TJS::tTJSBinaryStream *tjs2_create_memory_stream(const void *data, size_t len);
#endif

#ifdef __cplusplus
extern "C" {
#endif

typedef struct tjs2_engine tjs2_engine;

/* Log levels, keep in sync with the Rust mirror. */
#define TJS2_LOG_DEBUG 0
#define TJS2_LOG_INFO 1
#define TJS2_LOG_WARN 2
#define TJS2_LOG_ERROR 3

/* Console output / log callback. msg is UTF-8, valid only during the call. */
typedef void (*tjs2_log_cb)(int level, const char *msg, void *user);

/* Result value types. */
#define TJS2_VAL_VOID 0
#define TJS2_VAL_INTEGER 1
#define TJS2_VAL_REAL 2
#define TJS2_VAL_STRING 3
#define TJS2_VAL_OBJECT 4
#define TJS2_VAL_ARRAY 5
#define TJS2_VAL_RETAINED 6
#define TJS2_VAL_OCTET 7
/* TJS `null`: an object value with no object pointer (tvtObject with
 * Object == nullptr). Distinct from TJS2_VAL_VOID (no value) and from
 * TJS2_VAL_OBJECT with a zero handle (an object whose handle was not
 * marshaled). */
#define TJS2_VAL_NULL 8

typedef struct tjs2_value_id_t *tjs2_value_id;

typedef struct {
    int type;
    long long integer;
    double real;
    /* UTF-8 string; owned by the engine, valid until the next engine call.
     * TJS2_VAL_OCTET: raw binary data (may contain NUL); `array_count` is its
     * byte length. Also owned by the engine/caller for the duration of the
     * call, exactly like `string`. */
    const char *string;
    /* TJS2_VAL_ARRAY: `count` NUL-terminated UTF-8 strings (owned by the
     * caller, valid until the callback returns).
     * TJS2_VAL_OBJECT: the closure receiver (`objthis`, an opaque
     *   iTJSDispatch2*) of the object argument when it is a method/closure
     *   reference; 0 for a plain object. Retaining the value preserves the
     *   receiver so a later call runs with the correct `this`. */
    const char **array;
    /* TJS2_VAL_ARRAY: element count. TJS2_VAL_OCTET: byte length of
     * `string`. Unused (0) for objects. */
    int array_count;
    /* TJS2_VAL_RETAINED: a value retained via tjs2_retain_value.
     * TJS2_VAL_OBJECT: the raw iTJSDispatch2* of the object argument (an
     *   opaque pointer in this ABI; 0 when the object is not known).
     *   tjs2_retain_value retains exactly this object when it is set, and
     *   otherwise falls back to the engine's most recent object-valued
     *   result. */
    tjs2_value_id retained;
} tjs2_value;

/* Create / destroy a script engine instance. */
tjs2_engine *tjs2_create(void);

// Stream factories + data dir (see streams.cpp).
void tjs2_wire_stream_factories(void);
void tjs2_set_data_dir(const char *dir);

/*
 * Storage seam for the text/binary stream factories.
 *
 * The reference wires TJSCreate{Text,Binary}StreamFor{Read,Write} to the
 * storage layer (ScriptMgnIntf.cpp:464-469), so `Array.load` on
 * `archive.xp3>member` opens the archive member (StorageIntf.cpp:1131-1148).
 * The storage lives in the compatibility layer, not in this crate, so the
 * host installs one read callback here. The read factories call it first for
 * every name and fall back to the local filesystem when it reports
 * TJS2_STORAGE_READ_NOT_FOUND; a name neither source serves raises.
 *
 * `name`/`name_len` are UTF-8 (not NUL-terminated) and may carry the
 * `archive.xp3>member` form. On TJS2_STORAGE_READ_OK the callback stores a
 * malloc'd buffer in *out_data (freed by the stream layer with
 * tjs2_free_storage_bytes) and its byte length in *out_len. On
 * TJS2_STORAGE_READ_ERROR it may store a malloc'd UTF-8 message in
 * *out_error (freed with tjs2_free_string).
 */
#define TJS2_STORAGE_READ_OK 0
#define TJS2_STORAGE_READ_NOT_FOUND 1
#define TJS2_STORAGE_READ_ERROR 2

typedef int (*tjs2_storage_read_fn)(void *user, const char *name,
                                    size_t name_len,
                                    unsigned char **out_data, size_t *out_len,
                                    char **out_error);

/*
 * Install (or, with a NULL callback, clear) the storage read callback. One
 * process-wide registration; `user` is owned by the installer and must
 * outlive the registration.
 */
void tjs2_set_storage_read_callback(tjs2_storage_read_fn read, void *user);

/* Free a buffer a storage read callback returned. */
void tjs2_free_storage_bytes(void *data);
void tjs2_destroy(tjs2_engine *e);

/* Set the console output / log callback (may be NULL). */
void tjs2_set_log_cb(tjs2_engine *e, tjs2_log_cb cb, void *user);

/*
 * Execute a TJS script (UTF-8). On success returns 0 and, if out_result is
 * non-NULL, fills it. On failure returns non-zero and, if out_error is
 * non-NULL, points it at a malloc'd UTF-8 message (free with
 * tjs2_free_string).
 *
 * `script` is a byte pointer and `script_len` its length; the pair is
 * passed to the VM as `ttstr` so the ABI neither rejects nor cuts an
 * interior NUL. The VM's text storage is NUL-terminated, so the VM itself
 * decides what an interior NUL means (it may cut the payload); that is the
 * VM's behaviour, not a pre-check here.
 *
 * `name` stays a NUL-terminated diagnostic label: the VM consumes it as a
 * `ttstr` as well, but a name is a C-string-shaped label and is not the
 * payload a script byte range is. `context` is the raw iTJSDispatch2* the
 * script runs with as `this` (the `context` argument of the reference
 * `TJS::tTJS::ExecScript`), or NULL for the global object. `lineofs` shifts
 * the reported line numbers (the reference `lineofs`).
 */
int tjs2_exec_script(tjs2_engine *e, const char *script, size_t script_len,
                     const char *name, void *context, int lineofs,
                     tjs2_value *out_result, char **out_error);

/* Evaluate a TJS expression (UTF-8). Same conventions as tjs2_exec_script. */
int tjs2_eval(tjs2_engine *e, const char *expression, size_t expression_len,
              const char *name, void *context, int lineofs,
              tjs2_value *out_result, char **out_error);

/*
 * Execute a TJS script whose source is UTF-16 code units (the reference
 * `tjs_char` array). Identical to tjs2_exec_script except the payload is not
 * a UTF-8 byte range, so an unpaired surrogate survives the boundary. The
 * script loader uses this because the reference text stream keeps the code
 * units (`reference/cpp/core/base/TextStream.cpp`). `script_len` counts code
 * units; `name` is UTF-8 like tjs2_exec_script.
 */
int tjs2_exec_script_wide(tjs2_engine *e, const uint16_t *script,
                          size_t script_len, const char *name, void *context,
                          int lineofs, tjs2_value *out_result,
                          char **out_error);

/* Evaluate a TJS expression from UTF-16 code units; same conventions. */
int tjs2_eval_wide(tjs2_engine *e, const uint16_t *expression,
                   size_t expression_len, const char *name, void *context,
                   int lineofs, tjs2_value *out_result, char **out_error);

/*
 * Replace the text of the registered TJS message named `name` (UTF-8) and
 * return 1, or return 0 when no message by that name is registered
 * (reference `TJS::TJSAssignMessage`).
 */
int tjs2_assign_message(const char *name, const char *message);

/*
 * Compile a UTF-8 script to a binary bytecode stream (reference
 * `tTJS::CompileScript` / `TVPCompileStorage`). `script`/`script_len` are
 * length-aware like tjs2_exec_script; `output_path` stays NUL-terminated
 * because it is a filesystem path handed to the OS. `output_path` names the
 * binary file to create; it is resolved by the wired binary-stream factory
 * (absolute paths, data-dir-relative names). `isresult`, `outputdebug` and
 * `isexpression` mirror the reference bool parameters; `name` and `lineofs`
 * label the script block for diagnostics.
 *
 * Returns 0 on success. On failure returns non-zero and, if out_error is
 * non-NULL, points it at a malloc'd UTF-8 message (free with
 * tjs2_free_string). The output file is written incrementally, so a failed
 * compile may leave a partial file (the caller controls the path).
 */
int tjs2_compile_script(tjs2_engine *e, const char *script, size_t script_len,
                        const char *output_path, int isresult,
                        int outputdebug, int isexpression, const char *name,
                        int lineofs, char **out_error);

/*
 * Load and execute TJS2 precompiled bytecode from an in-memory buffer
 * (reference `tTJS::LoadByteCode(tTJSBinaryStream*, ...)`, whose only caller
 * in the script path is `TVPExecuteStorage`,
 * `reference/cpp/core/base/ScriptMgnIntf.cpp:713-727`). `buff`/`len` are the
 * raw storage bytes (offset applied by the caller); `name` is the diagnostic
 * block name and `context` the script `this` (the `context` argument of the
 * reference `LoadByteCode`).
 *
 * `out_is_bytecode` mirrors the reference's `isbytecode`: 1 when the loader
 * recognized and consumed the input (TJS2 bytecode, or the binary-serialized
 * data the same loader entry also accepts) and `out_result` holds its value,
 * and 0 when the input was neither, with `out_result` left void; the caller
 * then falls through to the text path. A non-bytecode input is not an error.
 *
 * Returns 0 on success, non-zero on a genuine failure (bytecode that the
 * loader recognizes and then rejects) with a malloc'd UTF-8 message in
 * *out_error (free with tjs2_free_string). `name` stays NUL-terminated;
 * `out_result`/`out_is_bytecode` may be NULL.
 */
int tjs2_load_bytecode(tjs2_engine *e, const char *buff, size_t len,
                       const char *name, void *context, int *out_is_bytecode,
                       tjs2_value *out_result, char **out_error);

/*
 * The vendored TJS2 engine version as `major.minor.release` UTF-8 (reference
 * `TJSVersionMajor`/`TJSVersionMinor`/`TJSVersionRelease`, `tjs.cpp:44-46`).
 * The returned pointer is a static string; do not free it.
 */
const char *tjs2_tjs_version_string(void);

/*
 * The C++ translation unit's compile date/time (`__DATE__`/`__TIME__`) as
 * static strings, the counterpart of the reference's `TVPCompileDate` /
 * `TVPCompileTime` (`MsgIntf.cpp:136-137`). Do not free them.
 */
const char *tjs2_compile_date(void);
const char *tjs2_compile_time(void);

/*
 * Dump every live script block to the engine's console/log output
 * (reference `tTJS::Dump` / `TVPDumpScriptEngine`). Returns 0 on success;
 * on failure returns non-zero with a malloc'd message in *out_error.
 */
int tjs2_dump(tjs2_engine *e, char **out_error);

/*
 * Request a member-table rebuild for every live object. The rebuild is lazy:
 * an object rebuilds on its next property access. The frame body calls this
 * every 1500 ms. Returns nothing; the request only bumps the VM's rebuild
 * magic.
 */
void tjs2_do_rehash(void);

/*
 * Run the TJS2 garbage collector (reference `tTJS::DoGarbageCollection`).
 * This is the `clIdle` half of `System.doCompact`: the reference's compact
 * event hook calls it whenever the compact level reaches
 * `TVP_COMPACT_LEVEL_IDLE`. Returns 0 on success; on failure returns non-zero
 * and, if out_error is non-NULL, points it at a malloc'd UTF-8 message (free
 * with tjs2_free_string).
 */
int tjs2_do_gc(tjs2_engine *e, char **out_error);

/*
 * Class-name list of a retained object (reference `Scripts.getClassNames`):
 * loop `iTJSDispatch2::ClassInstanceInfo(TJS_CII_GET, n, value)` and build
 * a TJS Array of names, the last (most-derived) class first. `obj` is a
 * retained object id (see tjs2_retain_value / tjs2_retain_object).
 *
 * Returns 0 on success and, if out is non-NULL, fills it with a
 * TJS2_VAL_RETAINED id for the new Array (release it with
 * tjs2_release_value, or consume it through a native's return slot). On
 * failure returns non-zero with a malloc'd UTF-8 message in *out_error.
 */
int tjs2_get_class_names(tjs2_engine *e, tjs2_value_id obj, tjs2_value *out,
                         char **out_error);

/*
 * Enumerate the members of a retained object (reference
 * `Scripts.getObjectKeys` / `ScriptsAdd::_getKeys`): walk the object's
 * `iTJSDispatch2::EnumMembers` with `TJS_IGNOREPROP | TJS_ENUM_NO_VALUE`
 * (the flags the reference uses), skipping hidden members, and return the
 * names as a TJS Array of strings. Class objects are enumerated together
 * with their inherited / static members, so the result is the object's
 * whole observable member surface. `obj` is a retained object id (see
 * tjs2_retain_value / tjs2_retain_object).
 *
 * Returns 0 on success and, if out is non-NULL, fills it with a
 * TJS2_VAL_RETAINED id for the new Array (release it with
 * tjs2_release_value, or consume it through a native's return slot). On
 * failure returns non-zero with a malloc'd UTF-8 message in *out_error.
 */
int tjs2_enum_members(tjs2_engine *e, tjs2_value_id obj, tjs2_value *out,
                      char **out_error);

/*
 * Enable the "missing" member handler on a retained object (reference
 * `Scripts.setCallMissing`): `ClassInstanceInfo(TJS_CII_SET_MISSING, 0,
 * "missing")` so an unknown member routes to the object's `missing`
 * method. Returns 0 on success; on failure returns non-zero with a malloc'd
 * message in *out_error.
 */
int tjs2_set_call_missing(tjs2_engine *e, tjs2_value_id obj,
                          char **out_error);

/*
 * Resolve a retained object to the Rust payload pointer of an
 * instance-based native class — the C++ equivalent of the reference's
 * `objthis->NativeInstanceSupport(TJS_NIS_GETINSTANCE, classid, &native)`
 * lookup that TJS_GET_NATIVE_INSTANCE performs in method bodies, followed by
 * the payload the VM kept for that instance. It is exposed so Rust can map
 * "script object" -> "our native payload" without the script-visible `id` /
 * `nativeId` helper properties.
 *
 * `obj` is a retained object id (see tjs2_retain_value /
 * tjs2_retain_object). `class_name_utf8` is the UTF-8 name the class was
 * registered with via tjs2_register_native_class_instance — the class
 * *name*, not a numeric class id: numeric ids are allocated inside the VM
 * (TJSRegisterNativeClass) and are never exposed to Rust, while the name is
 * exactly what Rust supplies at registration.
 *
 * Returns the payload pointer on a hit, or NULL when there is none:
 *   - `e`, `obj` or `class_name_utf8` is NULL, `obj` is not a live
 *     retained id, or the retained value is not an object;
 *   - the object is TJS `null` (an object value with no object pointer);
 *   - `class_name_utf8` does not name a class registered with
 *     tjs2_register_native_class_instance on this engine (names of
 *     static-only or builtin classes resolve to NULL too — their objects
 *     carry no Rust payload, and their VM-native instances must not be
 *     mistaken for ours);
 *   - the object is not an instance of that class: a plain script object,
 *     an instance of another native class, or the class object itself
 *     (class objects have no native instance registered under their own
 *     class id);
 *   - the create callback produced no payload.
 *
 * An object whose native instance was invalidated is still a hit: the
 * reference NativeInstanceSupport keeps the instance registered until the
 * object is destructed (`tTJSCustomObject::Finalize` calls Invalidate, the
 * class ids stay until Destruct), so an argument lookup must keep
 * recognizing its class. NULL is therefore an ordinary, non-error outcome;
 * this entry point never raises a TJS error and never lets a C++ exception
 * cross the ABI.
 *
 * Ownership: the payload stays owned by the engine (allocated by the
 * tjs2_native_create_instance_fn callback, freed exactly once by
 * tjs2_native_destroy_instance_fn when the object dies); this call borrows
 * the pointer and never transfers ownership — callers must not free it.
 *
 * Validity: the pointer is valid only while the object stays alive (keep
 * the retained id live) and must not outlive tjs2_destroy. An `invalidate
 * obj;` still resolves, but the invalidate callback has already released the
 * resources the payload owns; callers must treat the payload's state as shut
 * down (a layer has no images, a window has no draw device). Direct
 * method/property dispatch is separate: it keeps rejecting a finalized
 * instance, so this lookup must not be used to run methods on such an
 * object.
 */
void *tjs2_get_native_instance(tjs2_engine *e, tjs2_value_id obj,
                               const char *class_name_utf8);

/* Free a string returned via out_error. */
void tjs2_free_string(char *s);

/*
 * Retained TJS values (function objects in particular), so Rust can hold a
 * script value across engine calls and invoke it later — the Timer native
 * needs this: timers fire from the host update loop and call their script
 * callback.
 *
 * A tjs2_value cannot carry an object handle, so an OBJECT-typed value is
 * resolved against the engine's most recent object-valued result (e.g. the
 * result of eval'ing the name of a script function). Retained ids are
 * engine-local; release them with tjs2_release_value before (or by)
 * tjs2_destroy. Release is idempotent.
 */


/* Retain a script value. Returns a per-engine id holding its own reference
 * (the retained value stays callable until released), or NULL on failure
 * (e.g. an OBJECT value when no object result is available to resolve
 * against). */
tjs2_value_id tjs2_retain_value(void *engine, const tjs2_value *v);

/* Release a retained value. Idempotent: releasing an unknown or NULL id is
 * a safe no-op. */
void tjs2_release_value(void *engine, tjs2_value_id id);

/* Invalidate the retained object value (the reference `invalidate obj;`
 * statement): calls the object's `iTJSDispatch2::Invalidate` so its script
 * `finalize` runs. Returns 0 on success, non-zero on an unknown id or a
 * script exception. The retention is *not* released. */
int tjs2_invalidate_retained(void *engine, tjs2_value_id id);

/* The reason the last tjs2_invalidate_retained call failed: the failing
 * script's own message when the finalizer raised, a description of the
 * failure otherwise. NULL when the last call succeeded. The string is
 * malloc'd; free it with tjs2_free_string. */
char *tjs2_invalidate_error_message(void *engine);

/* Diagnostic: number of entries currently in the retained-value map (used by
 * the test suite to detect retention leaks). */
size_t tjs2_retained_count(void *engine);

/* Stack trace string for Scripts.getTraceString; malloc'd, free with
 * tjs2_free_string. */
char *tjs2_get_stack_trace_string(void *engine, int limit);

/* Retain a raw TJS object (see tjs2_abi.cpp). Returns a per-engine id or NULL. */
tjs2_value_id tjs2_retain_object(void *engine, void *obj);

/* Duplicate an existing retained id into a fresh id (the original stays
 * live). Used to return a cached object without consuming the cache. */
tjs2_value_id tjs2_retain_retained_id(void *engine, tjs2_value_id id);

/*
 * Invoke the retained value's default member (FuncCall, no membername, no
 * objthis) with argc arguments. On success returns 0 and, if out is
 * non-NULL, fills it. On failure (unknown id, non-callable value, or a
 * script exception) returns non-zero and, if out_error is non-NULL, points
 * it at a malloc'd UTF-8 message (free with tjs2_free_string).
 */
int tjs2_call_value(void *engine, tjs2_value_id id, int argc,
                    const tjs2_value *argv, tjs2_value *out,
                    char **out_error);

/*
 * Like tjs2_call_value, but supplies a fallback receiver. The retained
 * value's own ObjThis wins when it is set; otherwise `objthis` is used, and
 * only when both are null does it fall back to the function object itself
 * (tTJSVariantClosure::FuncCall's preference). Script callbacks that pass an
 * anonymous function to a helper use the helper's calling context as the
 * fallback (reference `ScriptsAdd::foreach`, scriptsEx.cpp:690-693).
 */
int tjs2_call_value_with_objthis(void *engine, tjs2_value_id id, int argc,
                                 const tjs2_value *argv, void *objthis,
                                 tjs2_value *out, char **out_error);

/*
 * Read a named property from a retained object value (PropGet through the
 * object's class chain). Same conventions as tjs2_call_value: returns 0 on
 * success and fills out; on failure returns non-zero with a malloc'd UTF-8
 * message in out_error.
 *
 * `flags` is the TJS PropGet flag set: 0 reads the member and calls the
 * object's default member, TJS_IGNOREPROP (0x800) reads the member's own
 * value without calling it. The member walks of ScriptsAdd::_getKeys and
 * ScriptsAdd::foreach pass TJS_IGNOREPROP, so a member that is a TJS
 * `property` yields the property object and its getter never runs.
 */
int tjs2_prop_get(void *engine, tjs2_value_id id, const char *membername,
                  unsigned int flags, tjs2_value *out, char **out_error);

/*
 * Write a named property on a retained object value (PropSet through the
 * object's class chain). TJS_MEMBERENSURE is passed, matching the flag the
 * VM uses for plain assignment (tjsInterCodeExec.cpp:1182): a missing member
 * is created, and an existing native or script property setter is invoked.
 *
 * `value` is a tjs2_value produced by the Rust side (scalars/strings,
 * octets, null, or a TJS2_VAL_RETAINED id, which is consumed). Writing to an
 * invalid/invalidated object, a read-only property, or an unknown member is
 * reported as a catchable error. Same error convention as tjs2_prop_get:
 * returns 0 on success; on failure returns non-zero with a malloc'd UTF-8
 * message in out_error.
 */
int tjs2_prop_set(void *engine, tjs2_value_id id, const char *membername,
                  const tjs2_value *value, char **out_error);

/*
 * Delete a named property from a retained object value (reference
 * `iTJSDispatch2::DeleteMember`, tjsObject.h:472; the `tTJSCustomObject`
 * implementation is tjsObject.cpp:1537-1552 over `DeleteByName`,
 * tjsObject.cpp:759). `flag` is 0 and `objthis` is the dispatch object
 * itself — the same values the VM's own `delete` operator passes
 * (tjsInterCodeExec.cpp:1826) — so the member is removed from the object's
 * own table, not from a superclass.
 *
 * An absent member is the reference's `TJS_E_MEMBERNOTFOUND`
 * (tjsObject.cpp:1546), which the `delete` operator surfaces as the boolean
 * `false` rather than a thrown exception (tjsInterCodeExec.cpp:1829-1833);
 * this entry point reports it as a typed failure, the same way a member read
 * reports it. A retained id that is not a live object (null, released, or a
 * non-object value such as TJS `null`) is also a typed failure.
 *
 * Same error convention as tjs2_prop_get: returns 0 on success; on failure
 * returns non-zero with a malloc'd UTF-8 message in out_error.
 */
int tjs2_prop_delete(tjs2_engine *e, tjs2_value_id obj,
                     const char *membername_utf8, char **out_error);

/*
 * Write the **default member** of a retained object value (PropSet with a
 * NULL member name). A native `missing` handler uses this to supply the
 * value for an absent member: the VM passes a `tTJSSimpleGetSetProperty` as
 * the handler's third argument, and `CallGetMissing` reads that object's
 * default member back, so setting it is what lets an absent member be
 * *called* rather than merely read.
 *
 * Same value/error conventions as tjs2_prop_set.
 */
int tjs2_prop_set_default(void *engine, tjs2_value_id id,
                          const tjs2_value *value, char **out_error);

/*
 * Write a value back into a by-reference argument of the native method call
 * currently executing on this engine (reference native methods do
 * `(*param[index]) = value`, e.g. `Window.getMouseVelocity`). `index` is the
 * zero-based argument position; only valid while a native method callback is
 * running, because the ABI snapshots the arguments into `tjs2_value` copies
 * and this is the only way to reach the caller's original variant slot.
 *
 * Returns 0 on success; on failure returns non-zero with a malloc'd UTF-8
 * message in out_error. Callers that do not need to report the failure can
 * pass a NULL out_error.
 */
int tjs2_set_arg(tjs2_engine *e, int index, const tjs2_value *value,
                 char **out_error);

/*
 * Whether the VM asked for the return value of the native method call
 * currently executing on `e`. Reference native methods receive a null
 * `result` pointer when the caller discards the value (e.g. a bare method
 * call statement); this exposes that to a Rust callback so expression-mode
 * evaluation can compile without requiring a result. Returns 1 when the
 * value is wanted, 0 when it is discarded. Only meaningful during a native
 * method callback.
 */
int tjs2_result_needed(tjs2_engine *e);

/*
 * Native class registration (static methods only for this milestone).
 *
 * The VM is single-threaded: register classes only from the thread that
 * owns the engine, and not from inside a native method callback.
 */

/* A native method implemented in Rust.
 *
 * Returns 0 on success and fills `out`. On error returns non-zero and, if
 * out_error is non-NULL, points it at a malloc'd UTF-8 message (free with
 * tjs2_free_string).
 *
 *   engine   opaque tjs2_engine* the method was registered on
 *   argc     number of arguments
 *   argv     argc tjs2_value entries, valid only during the call
 *   out      return slot, filled by the callback on success
 */
typedef int (*tjs2_native_method_fn)(void *engine, int argc,
                                     const tjs2_value *argv, tjs2_value *out,
                                     char **out_error);

typedef struct tjs2_native_method {
    const char *name;         /* UTF-8, method name on the class */
    tjs2_native_method_fn fn; /* Rust callback */
} tjs2_native_method;

/*
 * Register a native class named `class_name` on the VM **global object** so
 * scripts can call ClassName.method(...). Methods are registered as static
 * members; `objthis` is not passed through (instance semantics are handled
 * by tjs2_register_native_class_instance below). The class and its members
 * are owned by the engine and released by tjs2_destroy.
 *
 * Returns 0 on success; non-zero on failure (invalid arguments, duplicate
 * class name, or an exception while registering).
 */
int tjs2_register_native_class(tjs2_engine *e, const char *class_name_utf8,
                               const tjs2_native_method *methods, int count);

/* Native property accessors, implemented in Rust.
 *
 * A get callback returns 0 on success and fills `out`; on error returns
 * non-zero and may point *out_error at a malloc'd UTF-8 message (free with
 * tjs2_free_string). A set callback receives the new value and follows the
 * same error convention (no return slot). */
typedef int (*tjs2_native_property_get_fn)(void *engine, tjs2_value *out,
                                           char **out_error);
typedef int (*tjs2_native_property_set_fn)(void *engine,
                                           const tjs2_value *value,
                                           char **out_error);

typedef struct tjs2_native_property {
    const char *name;               /* UTF-8, property name on the class */
    tjs2_native_property_get_fn get; /* may be NULL (write-only) */
    tjs2_native_property_set_fn set; /* may be NULL (read-only) */
} tjs2_native_property;

/*
 * Like tjs2_register_native_class, but also registers static, class-level
 * native properties. Scripts read and write them as plain members
 * (ClassName.prop / ClassName.prop = v) and may `delete ClassName.prop`.
 * A registered property without a getter reads as Void; without a setter,
 * writes raise an access-denied error. Same return conventions.
 */
int tjs2_register_native_class_ex(tjs2_engine *e, const char *class_name_utf8,
                                  const tjs2_native_method *methods, int count,
                                  const tjs2_native_property *properties,
                                  int prop_count);

/*
 * Instance-based native classes: `new ClassName(...)` creates an object
 * backed by a Rust-owned opaque instance.
 */

/* Create the Rust-side payload for a new instance of the class. Returns an
 * opaque pointer stored on the TJS object; must be non-NULL. */
typedef void *(*tjs2_native_create_instance_fn)(void *engine);

/* Free a payload created by tjs2_native_create_instance_fn, called exactly
 * once when the TJS object is destroyed. */
typedef void (*tjs2_native_destroy_instance_fn)(void *engine, void *instance);

/* Tear down a payload's native resources when the TJS object is invalidated
 * (reference `iTJSNativeInstance::Invalidate`, tjsNative.h:42). Optional;
 * may be NULL. The payload is still released by
 * tjs2_native_destroy_instance_fn when the object is destroyed, so the
 * callback must leave the payload valid for the later destructor (the
 * reference keeps the C++ instance alive until `Destruct`). */
typedef void (*tjs2_native_invalidate_instance_fn)(void *engine, void *instance);

/* An instance method implemented in Rust. Same conventions as
 * tjs2_native_method_fn, plus `instance`: the opaque payload created by
 * the create callback for the object the method was called on. */
typedef int (*tjs2_native_instance_method_fn)(void *engine, void *instance,
                                              int argc, const tjs2_value *argv,
                                              tjs2_value *out, char **out_error,
                                              void *objthis);

typedef struct tjs2_native_instance_method {
    const char *name;                  /* UTF-8, method name on the class */
    tjs2_native_instance_method_fn fn; /* Rust callback */
} tjs2_native_instance_method;

/* An instance property implemented in Rust. Same conventions as the
 * static property callbacks, plus `instance`: the payload of the object
 * the property was accessed on. get/set may be NULL (Void / read-only). */
typedef int (*tjs2_native_instance_property_get_fn)(void *engine, void *instance,
                                                    tjs2_value *out,
                                                    char **out_error,
                                                    void *objthis);
typedef int (*tjs2_native_instance_property_set_fn)(void *engine, void *instance,
                                                    const tjs2_value *value,
                                                    char **out_error,
                                                    void *objthis);

typedef struct tjs2_native_instance_property {
    const char *name; /* UTF-8, property name on the class */
    tjs2_native_instance_property_get_fn get; /* may be NULL */
    tjs2_native_instance_property_set_fn set; /* may be NULL */
} tjs2_native_instance_property;

/*
 * Register a native class whose instances carry a Rust-owned payload.
 * `create_instance` is called for every `new ClassName()`; the payload is
 * passed to each method call and released with `destroy_instance` when the
 * object is destroyed. Methods are registered as instance members (not
 * static): `objthis` resolves to the payload of the object the method was
 * invoked on. Calling an instance method on an object of a different native
 * class (or on the class itself) raises a TJS error.
 *
 * Same return conventions as tjs2_register_native_class.
 */
int tjs2_register_native_class_instance(
    tjs2_engine *e, const char *class_name_utf8,
    const tjs2_native_instance_method *methods, int count,
    const tjs2_native_instance_property *properties, int property_count,
    tjs2_native_create_instance_fn create_instance,
    tjs2_native_destroy_instance_fn destroy_instance,
    tjs2_native_invalidate_instance_fn invalidate_instance);

/*
 * Attach static (class-level) members to an already-registered native class
 * (reference `TJS_END_NATIVE_STATIC_METHOD_DECL` /
 * `TJS_END_NATIVE_STATIC_PROP_DECL_OUTER`). Instance-capable classes
 * registered with tjs2_register_native_class_instance put every member on
 * their instances; this adds the members the reference declares with
 * TJS_STATICMEMBER so they live on the class object and are not copied onto
 * instances (e.g. `Bitmap.loadHeader`, `MenuItem.textToKeycode`).
 *
 * `class_name_utf8` must name a class already registered on this engine.
 * Methods use the static callback signature (no instance/objthis). Returns 0
 * on success; non-zero on failure (unknown class, invalid arguments,
 * duplicate member, or an exception while registering).
 */
int tjs2_register_native_static_members(
    tjs2_engine *e, const char *class_name_utf8,
    const tjs2_native_method *methods, int count,
    const tjs2_native_property *properties, int prop_count);

/*
 * Attach instance members **without a native payload** to an already-existing
 * class, including a TJS2 builtin such as `Array` / `Dictionary` (reference
 * `NCB_ATTACH_CLASS` / `NCB_ATTACH_FUNCTION` in the plugin Binder). Unlike
 * tjs2_native_instance_method_fn, the callbacks receive no opaque `instance`
 * pointer: only `objthis`, the object the member was invoked on.
 *
 * A `Class` registered by tjs2_register_native_class[_instance] receives the
 * members as ordinary (non-static) instance members. Any other name is looked
 * up on the VM global object as a builtin class object (the same lookup
 * `NCB_ATTACH_CLASS` performs) and the members are PropSet onto it, so they
 * are reachable from every instance of that class. Properties work the same
 * way and use the objthis get/set callbacks.
 *
 * `class_name_utf8` must name a class already reachable on this engine.
 * Returns 0 on success; non-zero on failure (unknown class, invalid
 * arguments, a class member that could not be added, or an exception while
 * registering).
 */
typedef int (*tjs2_native_objthis_method_fn)(void *engine, void *objthis,
                                             int argc, const tjs2_value *argv,
                                             tjs2_value *out,
                                             char **out_error);
typedef struct tjs2_native_objthis_method {
    const char *name;                  /* UTF-8, method name on the class */
    tjs2_native_objthis_method_fn fn;  /* Rust callback */
} tjs2_native_objthis_method;

typedef int (*tjs2_native_objthis_property_get_fn)(void *engine, void *objthis,
                                                   tjs2_value *out,
                                                   char **out_error);
typedef int (*tjs2_native_objthis_property_set_fn)(void *engine, void *objthis,
                                                   const tjs2_value *value,
                                                   char **out_error);
typedef struct tjs2_native_objthis_property {
    const char *name; /* UTF-8, property name on the class */
    tjs2_native_objthis_property_get_fn get; /* may be NULL */
    tjs2_native_objthis_property_set_fn set; /* may be NULL */
} tjs2_native_objthis_property;

int tjs2_register_native_instance_members(
    tjs2_engine *e, const char *class_name_utf8,
    const tjs2_native_objthis_method *methods, int count,
    const tjs2_native_objthis_property *properties, int prop_count);

int tjs2_register_native_builtin_static_members(
    tjs2_engine *e, const char *class_name_utf8,
    const tjs2_native_objthis_method *methods, int count,
    const tjs2_native_objthis_property *properties, int prop_count);

/* Allocate with malloc; used to build error strings on the Rust side.
 * Pair with tjs2_free_string. */
void *tjs2_malloc(size_t size);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* TJS2_ABI_H */
