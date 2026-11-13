/*
    The script-block interface of the Rust code generator.

    This is the SHIPPED interface, and it is the only one: the compile
    entry compiles script text and hands the C++ executor the image of a
    script block, context by context. The executor runs a context's
    code stream and its size, and reads the constant area, the variable
    and argument counts and the labels beside it
    (crates/tjs/cpp/tjs2/tjsInterCodeContext.h:364-366, GetCodeSize() at
    :129, the code read at crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp:863).

    The entry catches a panic and reports it as an error value; no entry
    point unwinds into the C++ caller.
*/

#ifndef tjs2CodegenH
#define tjs2CodegenH

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* The kinds of a code context. */
#define TJS2_CG_CONTEXT_TOP_LEVEL          0
#define TJS2_CG_CONTEXT_FUNCTION           1
#define TJS2_CG_CONTEXT_EXPR_FUNCTION      2
#define TJS2_CG_CONTEXT_PROPERTY           3
#define TJS2_CG_CONTEXT_PROPERTY_SETTER    4
#define TJS2_CG_CONTEXT_PROPERTY_GETTER    5
#define TJS2_CG_CONTEXT_CLASS              6
#define TJS2_CG_CONTEXT_SUPER_CLASS_GETTER 7

/* The kinds of a constant. */
#define TJS2_CG_VALUE_VOID    0
#define TJS2_CG_VALUE_INTEGER 1
#define TJS2_CG_VALUE_REAL    2
#define TJS2_CG_VALUE_STRING  3
#define TJS2_CG_VALUE_OCTET   4
#define TJS2_CG_VALUE_OBJECT  5
/* The null object: an object with no dispatch. The reference builds it
   as an object variant whose dispatch is null
   (reference/krkr2/cpp/core/tjs2/tjsLex.cpp:1553). */
#define TJS2_CG_VALUE_NULL    6
/* A context of the block. The `object` field holds the index of the
   context within the block, and the C++ side resolves it to the context
   object; the Rust side has no pointer to store. This is the kind the
   reference's own bytecode format calls TYPE_INTER_OBJECT
   (reference/krkr2/cpp/core/tjs2/tjsConstArrayData.cpp:139-148). */
#define TJS2_CG_VALUE_CONTEXT 7
/* A constant inline array: the object the grammar builds while it parses
   the array, one element at a time
   (reference/krkr2/cpp/core/tjs2/bison/tjs.y:795-805). The Rust side
   cannot build a live object, so the image carries the kind, the elements
   and their number, and the C++ side builds the object. */
#define TJS2_CG_VALUE_ARRAY 8
/* A constant inline dictionary: the object the grammar builds while it
   parses the dictionary, one member at a time
   (reference/krkr2/cpp/core/tjs2/bison/tjs.y:831-841). The image carries
   the kind, the members and their number, and the C++ side builds the
   object. */
#define TJS2_CG_VALUE_DICTIONARY 9

/*----------------------------------------------------------------------
    The script block.

    A script block is the whole of one compiled script: the top level
    and every function, class and handler it holds. The block owns the
    contexts and the line table the executor's diagnostics read.
   ----------------------------------------------------------------------*/

/* A place in the script, and the place in the code it names.
   The C++ executor answers its error paths and the debugger through
   these, so the image must carry them. */
typedef struct tjs2_codegen_source_pos {
    int32_t code_pos;
    int32_t source_pos;
} tjs2_codegen_source_pos;

/* One line of the script.
   The C++ block holds a line vector and a line length vector
   (crates/tjs/cpp/tjs2/tjsScriptBlock.h); the executor's error messages
   and the debugger's line lookup read them. */
typedef struct tjs2_codegen_line {
    int32_t position;  /* the place in the script the line starts at */
    int32_t length;    /* the length of the line, in code units */
} tjs2_codegen_line;

typedef struct tjs2_codegen_value {
    int32_t kind;
    int64_t integer;
    double real;
    const uint16_t *string;
    int32_t string_length;
    const uint8_t *octet;
    int32_t octet_length;
    uint64_t object;
} tjs2_codegen_value;

/* One constant object of the block: a constant inline array or a constant
   inline dictionary.

   The grammar builds the object while it parses it: the array is made and
   its elements are added one by one, and the dictionary is made and its
   members are set one by one
   (reference/krkr2/cpp/core/tjs2/bison/tjs.y:795-805 and :831-841). The
   Rust side cannot build a live object, so the image carries the kind, the
   elements and their number, and the C++ side builds the object.

   A constant of the kind TJS2_CG_VALUE_ARRAY or TJS2_CG_VALUE_DICTIONARY
   names its object with the `object` field: the index of the object within
   the block's objects. */
typedef struct tjs2_codegen_object {
    /* the values of the elements */
    const tjs2_codegen_value *values;
    /* the number of elements */
    int32_t element_count;
    /* the keys of the elements: the value the member name is made of. It
       is null for the elements of an array. */
    const tjs2_codegen_value *keys;
} tjs2_codegen_object;

/* One member registration: the name of a member and the context it
   holds.

   The reference registers a child context as a member of its parent
   object while it compiles
   (reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:933-945), which
   needs a live object; the Rust side decides the list and the C++ side
   performs the registration. The reference's own bytecode format records
   the same list in the child's properties
   (reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:380-390). */
typedef struct tjs2_codegen_property {
    /* the name of the member, in UTF-16 */
    const uint16_t *name;
    /* the length of the name, in code units */
    int32_t name_length;
    /* the index of the context the member holds */
    int32_t member;
} tjs2_codegen_property;

/* The image of one context. Every pointer is owned by the generator and
   stays valid until the generator is freed or the context is released.
   The caller must not free them. */
typedef struct tjs2_codegen_context {
    /* the code stream: the instructions and their operands, in the
       order the executor reads them */
    const int32_t *code;
    /* the number of words in the code stream */
    int32_t code_size;

    /* the constant area: the values the code refers to */
    const tjs2_codegen_value *constants;
    /* the number of values in the constant area */
    int32_t constant_count;

    /* the number of local variables the context's name space held */
    int32_t variable_count;
    /* the number of registers the context reserves for itself */
    int32_t variable_reserve_count;
    /* the greatest number of frames the context needs */
    int32_t max_frame_count;

    /* the number of arguments the context declares */
    int32_t func_arg_count;
    /* the base of the unnamed argument array */
    int32_t func_unnamed_arg_base;
    /* the base of the collapsed argument */
    int32_t func_collapse_base;

    /* the labels: the places in the script the code names */
    const tjs2_codegen_source_pos *labels;
    /* the number of labels */
    int32_t label_count;

    /* the kind of the context */
    int32_t kind;
    /* the name of the context, in UTF-16; null when it has none */
    const uint16_t *name;
    /* the length of the name, in code units */
    int32_t name_length;

    /* the index of the parent context; -1 for none */
    int32_t parent;
    /* the index of the property's setter context; -1 for none */
    int32_t prop_setter;
    /* the index of the property's getter context; -1 for none */
    int32_t prop_getter;
    /* the index of the super-class proxy context; -1 for none */
    int32_t super_class_getter;

    /* the places in the super-class proxy's own code where the super
       class is evaluated, one per extends clause */
    const int32_t *super_class_getter_pointer;
    /* the number of those places */
    int32_t super_class_getter_pointer_count;

    /* the members this context contributes to its parent object */
    const tjs2_codegen_property *properties;
    /* the number of member registrations */
    int32_t property_count;
} tjs2_codegen_context;

/* One value of the constant area. The kind says which of the payload
   fields holds the value. */


/* The image of a whole script block. */
typedef struct tjs2_codegen_block {
    const tjs2_codegen_context *contexts;
    int32_t context_count;
    const tjs2_codegen_line *lines;
    int32_t line_count;
    /* the index of the top-level context; -1 when there is none */
    int32_t top_level;
    /* the constant objects of the block: the constant inline arrays and
       the constant inline dictionaries the constant areas hold */
    const tjs2_codegen_object *objects;
    /* the number of constant objects */
    int32_t object_count;
} tjs2_codegen_block;

/*----------------------------------------------------------------------
    The compile entry.

    The entry compiles script text and hands back the image of the
    script block. The handle owns the image: the C++ side copies out of
    it and frees it with tjs2_script_free.
   ----------------------------------------------------------------------*/

/* One conditional-compilation value: a name and the whole number it
   holds.

   The values outlive a script: a value one script sets is visible in the
   next one, so they go into a compile and come back out of it.
   (reference/krkr2/cpp/core/tjs2/tjsCompileControl.cpp:205) */
typedef struct tjs2_pp_value {
    /* the name of the value, in UTF-16 */
    const uint16_t *name;
    /* the length of the name, in code units */
    int32_t name_length;
    /* the value the name holds */
    int64_t integer;
} tjs2_pp_value;

/* The conditional-compilation values that go into a compile and come
   back out of it. */
typedef struct tjs2_pp_values {
    /* the values */
    const tjs2_pp_value *values;
    /* the number of values */
    int32_t count;
} tjs2_pp_values;

/* An opaque compiled script block: the handle owns the image. */
typedef struct tjs2_script tjs2_script;

/* The status of a compile. Zero is success; the negative codes are
   failures. */
#define TJS2_SCRIPT_OK            0
#define TJS2_SCRIPT_ERROR_SYNTAX -1   /* the script does not parse        */
#define TJS2_SCRIPT_ERROR_PANIC  -2   /* the front end panicked          */
#define TJS2_SCRIPT_ERROR_RANGE  -3   /* a value is out of range         */
#define TJS2_SCRIPT_ERROR_STATE  -4   /* the handle is in a wrong state  */

/* Compiles script text into the image of a script block.
 *
 * script        the text, as UTF-16 code units (tjs_char is UTF-16 in the
 *               reference; reference/krkr2/cpp/core/tjs2/tjsTypes.h:44).
 * length        the number of code units.
 * expression    non-zero when the text is an expression, not a program.
 *               The reference appends a ';' to the text in this mode
 *               (reference/krkr2/cpp/core/tjs2/tjsLex.cpp:1006-1008).
 * result_needed non-zero when the expression's value is wanted; it is read
 *               only in the expression mode
 *               (reference/krkr2/cpp/core/tjs2/tjsLex.cpp:1676).
 * values        the conditional-compilation values the script reads. The
 *               handle copies them, so the caller keeps its own; null
 *               when there are none.
 *
 * Returns a handle the caller owns (free it with tjs2_script_free). The
 * handle is returned even when the script does not compile: the failure
 * is then read with tjs2_script_status / tjs2_script_error_*. */
tjs2_script *tjs2_script_compile(const uint16_t *script, uint32_t length,
                                  int32_t expression, int32_t result_needed,
                                  const tjs2_pp_values *values);

/* Frees the handle and every buffer of the image it owns. */
void tjs2_script_free(tjs2_script *script);

/* The status of the compile: TJS2_SCRIPT_OK or a negative code. */
int32_t tjs2_script_status(const tjs2_script *script);

/* The message of the first failure; owned by the handle. NULL when there
   was none. The reference's message for a syntax error is
   "Syntax error (%1)" (reference/krkr2/cpp/core/tjs2/tjsError.h:288). */
const char *tjs2_script_error_message(const tjs2_script *script);

/* The source position of the first failure, in code units from the start
   of the script. */
int32_t tjs2_script_error_position(const tjs2_script *script);

/* The number of failures the front end counted. The reference counts
   every recovered syntax error and raises on the first one
   (reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:52-87,
   reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:444-446). */
int32_t tjs2_script_error_count(const tjs2_script *script);

/* Non-zero when the script used a conditional-compilation statement
   ("@if" / "@set"). The reference sets the same flag on the block
   (reference/krkr2/cpp/core/tjs2/tjsLex.cpp:1132, :1174) and the flag
   makes the block non-reusable
   (reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:79-82). */
int32_t tjs2_script_using_preprocessor(const tjs2_script *script);

/* The image of the script block. NULL when the compile failed. */
const tjs2_codegen_block *tjs2_script_block_image(const tjs2_script *script);

/* The conditional-compilation values the compile left behind: the
   values it was given, with the ones the script set replaced. The array
   is owned by the handle and stays valid until the handle is freed.
   NULL when the handle holds none. */
const tjs2_pp_values *tjs2_script_pp_values(const tjs2_script *script);

#ifdef __cplusplus
}
#endif

#endif /* tjs2CodegenH */
