// C ABI boundary between Rust (krkr-rs) and the C++ TJS2 scripting VM.
// The C++ implementation lives in tjs2_abi.cpp; this header is mirrored
// manually in crates/tjs2-sys/src/lib.rs (the surface is small and stable,
// no bindgen needed).
#ifndef TJS2_ABI_H
#define TJS2_ABI_H

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

typedef struct {
    int type;
    long long integer;
    double real;
    /* UTF-8 string; owned by the engine, valid until the next engine call. */
    const char *string;
} tjs2_value;

/* Create / destroy a script engine instance. */
tjs2_engine *tjs2_create(void);
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
 * members; `objthis` is not passed through (instance semantics are a later
 * milestone). The class and its methods are owned by the engine and released
 * by tjs2_destroy.
 *
 * Returns 0 on success; non-zero on failure (invalid arguments, duplicate
 * class name, or an exception while registering).
 */
int tjs2_register_native_class(tjs2_engine *e, const char *class_name_utf8,
                               const tjs2_native_method *methods, int count);

/* Allocate with malloc; used to build error strings on the Rust side.
 * Pair with tjs2_free_string. */
void *tjs2_malloc(size_t size);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* TJS2_ABI_H */
