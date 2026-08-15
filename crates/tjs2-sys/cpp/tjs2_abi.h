// C ABI boundary between Rust (krkr-rs) and the C++ TJS2 scripting VM.
// The C++ implementation lives in tjs2_abi.cpp; this header is mirrored
// manually in crates/tjs2-sys/src/lib.rs (the surface is small and stable,
// no bindgen needed).
#ifndef TJS2_ABI_H
#define TJS2_ABI_H

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

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* TJS2_ABI_H */
