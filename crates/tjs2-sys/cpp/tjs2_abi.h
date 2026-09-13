// C ABI boundary between Rust (krkr-rs) and the C++ TJS2 scripting VM.
// The C++ implementation lives in tjs2_abi.cpp; this header is mirrored
// manually in crates/tjs2-sys/src/lib.rs (the surface is small and stable,
// no bindgen needed).
#ifndef TJS2_ABI_H
#define TJS2_ABI_H

#include <vector>

#include <stddef.h> /* size_t */

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
void tjs2_destroy(tjs2_engine *e);

/* Set the console output / log callback (may be NULL). */
void tjs2_set_log_cb(tjs2_engine *e, tjs2_log_cb cb, void *user);

/*
 * Execute a TJS script (UTF-8). On success returns 0 and, if out_result is
 * non-NULL, fills it. On failure returns non-zero and, if out_error is
 * non-NULL, points it at a malloc'd UTF-8 message (free with
 * tjs2_free_string).
 */
int tjs2_exec_script(tjs2_engine *e, const char *script, const char *name,
                     tjs2_value *out_result, char **out_error);

/* Evaluate a TJS expression (UTF-8). Same conventions as tjs2_exec_script. */
int tjs2_eval(tjs2_engine *e, const char *expression, const char *name,
              tjs2_value *out_result, char **out_error);

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

/* Diagnostic: number of entries currently in the retained-value map (used by
 * the test suite to detect retention leaks). */
size_t tjs2_retained_count(void *engine);

/* Stack trace string for Scripts.getTraceString; malloc'd, free with
 * tjs2_free_string. */
char *tjs2_get_stack_trace_string(void *engine, int limit);

/* Retain a raw TJS object (see tjs2_abi.cpp). Returns a per-engine id or NULL. */
tjs2_value_id tjs2_retain_object(void *engine, void *obj);

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
 * Read a named property from a retained object value (PropGet through the
 * object's class chain). Same conventions as tjs2_call_value: returns 0 on
 * success and fills out; on failure returns non-zero with a malloc'd UTF-8
 * message in out_error.
 */
int tjs2_prop_get(void *engine, tjs2_value_id id, const char *membername,
                  tjs2_value *out, char **out_error);

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
    tjs2_native_destroy_instance_fn destroy_instance);

/* Allocate with malloc; used to build error strings on the Rust side.
 * Pair with tjs2_free_string. */
void *tjs2_malloc(size_t size);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* TJS2_ABI_H */
