/*
    The script-block interface of the Rust code generator.

    This is the SHIPPED interface: it hands the C++ executor the image of
    a script block, context by context. The executor runs a context's
    code stream and its size, and reads the constant area, the variable
    area, the function-argument area and the labels beside it
    (crates/tjs/cpp/tjs2/tjsInterCodeGen.h:293-295, GetCodeSize() at :435,
    the code read at crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp:862).

    The node-building calls further down are the PROOF INSTRUMENT. They
    exist only so that the C++ grammar can drive the Rust generator and
    the two code streams can be compared word for word. They are not the
    shipped interface, and their marshalling cost is not a shipping
    number.

    Every entry point catches a panic and reports it as an error value;
    no entry point unwinds into the C++ caller.
*/

#ifndef tjs2CodegenH
#define tjs2CodegenH

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* The error codes. Zero is success; the negative codes are failures. */
#define TJS2_CG_OK             0
#define TJS2_CG_ERROR_SYNTAX  -1
#define TJS2_CG_ERROR_PANIC   -2
#define TJS2_CG_ERROR_RANGE   -3
#define TJS2_CG_ERROR_STATE   -4

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

/* The kinds of a function argument. */
#define TJS2_CG_ARG_NORMAL         0
#define TJS2_CG_ARG_EXPAND         1
#define TJS2_CG_ARG_UNNAMED_EXPAND 2

/* An opaque generator: it holds the contexts of one script block. */
typedef struct tjs2_codegen tjs2_codegen;

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

typedef struct tjs2_codegen_variable {
    const uint16_t *name;
    int32_t name_length;
    int32_t register_;
} tjs2_codegen_variable;

typedef struct tjs2_codegen_func_arg {
    int32_t kind;
    int32_t register_;
    const uint16_t *name;
    int32_t name_length;
} tjs2_codegen_func_arg;

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

    /* the variable area: the local variables of the context, in the
       order the name space holds them */
    const tjs2_codegen_variable *variables;
    /* the number of variables in the variable area */
    int32_t variable_count;
    /* the number of registers the context reserves for itself */
    int32_t variable_reserve_count;
    /* the greatest number of frames the context needs */
    int32_t max_frame_count;

    /* the function-argument area: the arguments the context declares */
    const tjs2_codegen_func_arg *func_args;
    /* the number of arguments in the function-argument area */
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


/* One local variable of the variable area. */


/* One argument of the function-argument area. */


/* The image of a whole script block. */
typedef struct tjs2_codegen_block {
    const tjs2_codegen_context *contexts;
    int32_t context_count;
    const tjs2_codegen_line *lines;
    int32_t line_count;
    /* the index of the top-level context; -1 when there is none */
    int32_t top_level;
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

/* Makes a generator: an empty script block. */
tjs2_codegen *tjs2_codegen_new(void);

/* Frees a generator and every image it holds. */
void tjs2_codegen_free(tjs2_codegen *generator);

/* The image of the script block.
   The returned pointer is owned by the generator and stays valid until
   the generator is freed. It is null when the block could not be made,
   and the error is then read with tjs2_codegen_last_error. */
const tjs2_codegen_block *tjs2_codegen_block_image(const tjs2_codegen *generator);







/* The image of one context. */
const tjs2_codegen_context *tjs2_codegen_context_image(const tjs2_codegen *generator,
                                                         int32_t context);

/* The last error of the generator: a negative code, or zero. */
int32_t tjs2_codegen_last_error(const tjs2_codegen *generator);

/* The message of the last error. The returned pointer is owned by the
   generator and stays valid until the next call on it. */
const char *tjs2_codegen_last_error_message(const tjs2_codegen *generator);

/*----------------------------------------------------------------------
    The proof instrument.

    These calls build the expression tree node by node, so that the C++
    grammar can drive the Rust generator and the two code streams can be
    compared. They are NOT the shipped interface: the shipped path hands
    the Rust parser's tree to the Rust generator directly, and the C++
    side consumes the script-block image above.
   ----------------------------------------------------------------------*/

/* A node of the tree: a number that names a node in the generator's
   own table. No subtree is copied across the boundary. */
typedef int32_t tjs2_codegen_node;

/* Adds a child to a node. */
int32_t tjs2_codegen_node_add_child(tjs2_codegen *generator,
                                     tjs2_codegen_node node,
                                     tjs2_codegen_node child);

/* The opcode of a node. */
int32_t tjs2_codegen_node_kind(tjs2_codegen *generator,
                               tjs2_codegen_node node);

/* The place of a node in the script. */
int32_t tjs2_codegen_node_position(tjs2_codegen *generator,
                                    tjs2_codegen_node node);

/* Sets the opcode of a node. */
int32_t tjs2_codegen_node_set_opcode(tjs2_codegen *generator,
                                      tjs2_codegen_node node,
                                      int32_t opcode);

/* The number of contexts the generator holds. */
int32_t tjs2_codegen_context_count(const tjs2_codegen *generator);

/* The number of words in the code of a context. */
int32_t tjs2_codegen_code_size(tjs2_codegen *generator, int32_t context);

/* One word of the code of a context. */
int32_t tjs2_codegen_code_word(tjs2_codegen *generator,
                                int32_t context,
                                int32_t index);


/* Makes a node with an opcode. */
int32_t tjs2_codegen_make_np0(tjs2_codegen *generator,
                              int32_t opcode,
                              int32_t position);

/* Makes a node with an opcode and one child. */
int32_t tjs2_codegen_make_np1(tjs2_codegen *generator,
                              int32_t opcode,
                              int32_t position,
                              tjs2_codegen_node child);

/* Makes a node with an opcode and two children. */
int32_t tjs2_codegen_make_np2(tjs2_codegen *generator,
                              int32_t opcode,
                              int32_t position,
                              tjs2_codegen_node child1,
                              tjs2_codegen_node child2);

/* Makes a node with an opcode and three children. */
int32_t tjs2_codegen_make_np3(tjs2_codegen *generator,
                              int32_t opcode,
                              int32_t position,
                              tjs2_codegen_node child1,
                              tjs2_codegen_node child2,
                              tjs2_codegen_node child3);

/* Makes a constant node. */
int32_t tjs2_codegen_make_const(tjs2_codegen *generator,
                                int32_t position,
                                const tjs2_codegen_value *value);

/* Sets the value of a node. */
int32_t tjs2_codegen_node_set_value(tjs2_codegen *generator,
                                     tjs2_codegen_node node,
                                     const tjs2_codegen_value *value);

/* The place in the script the analyser is at.

   The generator writes this place into the instructions that have no place
   of their own. The native grammar tells the generator where the analyser
   is every time the analyser reads. */
int32_t tjs2_codegen_at_lex_position(tjs2_codegen *generator,
                                     int32_t position);

/* Opens a code context. */
int32_t tjs2_codegen_push_context(tjs2_codegen *generator,
                                   const uint16_t *name,
                                   uint32_t name_length,
                                   int32_t kind);

/* Closes the code context the code is generated for. */
int32_t tjs2_codegen_pop_context(tjs2_codegen *generator);

/* The code of an expression that stands on its own. */
int32_t tjs2_codegen_create_expr_code(tjs2_codegen *generator,
                                       tjs2_codegen_node node);

/* A local variable of the context. */
int32_t tjs2_codegen_add_local_variable(tjs2_codegen *generator,
                                        const uint16_t *name,
                                        uint32_t name_length,
                                        int32_t init);

/* A local variable of the context, with its first value. */
int32_t tjs2_codegen_init_local_variable(tjs2_codegen *generator,
                                         const uint16_t *name,
                                         uint32_t name_length,
                                         tjs2_codegen_node node);

/* A declared argument of a function. */
int32_t tjs2_codegen_add_function_decl_arg(tjs2_codegen *generator,
                                            const uint16_t *name,
                                            uint32_t name_length,
                                            tjs2_codegen_node node);

/* The collapsed argument of a function. */
int32_t tjs2_codegen_add_function_decl_arg_collapse(tjs2_codegen *generator,
                                                    const uint16_t *name,
                                                    uint32_t name_length);

/* The argument of a property setter. */
int32_t tjs2_codegen_set_property_decl_arg(tjs2_codegen *generator,
                                            const uint16_t *name,
                                            uint32_t name_length);

/* The commands of the grammar. */
int32_t tjs2_codegen_enter_if_code(tjs2_codegen *generator);
int32_t tjs2_codegen_create_if_expr_code(tjs2_codegen *generator,
                                         tjs2_codegen_node node);
int32_t tjs2_codegen_exit_if_code(tjs2_codegen *generator);
int32_t tjs2_codegen_enter_else_code(tjs2_codegen *generator);
int32_t tjs2_codegen_exit_else_code(tjs2_codegen *generator);
int32_t tjs2_codegen_enter_while_code(tjs2_codegen *generator, int32_t do_while);
int32_t tjs2_codegen_create_while_expr_code(tjs2_codegen *generator,
                                            tjs2_codegen_node node,
                                            int32_t do_while);
int32_t tjs2_codegen_exit_while_code(tjs2_codegen *generator, int32_t do_while);
int32_t tjs2_codegen_enter_for_code(tjs2_codegen *generator);
int32_t tjs2_codegen_create_for_expr_code(tjs2_codegen *generator,
                                          tjs2_codegen_node node);
int32_t tjs2_codegen_set_for_third_expr_code(tjs2_codegen *generator,
                                              tjs2_codegen_node node);
int32_t tjs2_codegen_exit_for_code(tjs2_codegen *generator);
int32_t tjs2_codegen_enter_switch_code(tjs2_codegen *generator,
                                        tjs2_codegen_node node);
int32_t tjs2_codegen_exit_switch_code(tjs2_codegen *generator);
int32_t tjs2_codegen_process_case_code(tjs2_codegen *generator,
                                        tjs2_codegen_node node);
int32_t tjs2_codegen_enter_with_code(tjs2_codegen *generator,
                                     tjs2_codegen_node node);
int32_t tjs2_codegen_exit_with_code(tjs2_codegen *generator);
int32_t tjs2_codegen_enter_try_code(tjs2_codegen *generator);
int32_t tjs2_codegen_enter_catch_code(tjs2_codegen *generator,
                                       const uint16_t *name,
                                       uint32_t name_length);
int32_t tjs2_codegen_exit_try_code(tjs2_codegen *generator);
int32_t tjs2_codegen_process_throw_code(tjs2_codegen *generator,
                                         tjs2_codegen_node node);
int32_t tjs2_codegen_do_break(tjs2_codegen *generator);
int32_t tjs2_codegen_do_continue(tjs2_codegen *generator);
int32_t tjs2_codegen_do_debugger(tjs2_codegen *generator);
int32_t tjs2_codegen_return_from_func(tjs2_codegen *generator,
                                       tjs2_codegen_node node);
int32_t tjs2_codegen_create_extends_expr_code(tjs2_codegen *generator,
                                              tjs2_codegen_node node,
                                              int32_t hold);
int32_t tjs2_codegen_enter_block(tjs2_codegen *generator);
int32_t tjs2_codegen_exit_block(tjs2_codegen *generator);

/* Compares the data of a context with the data the caller holds. */
int32_t tjs2_codegen_compare_data(const tjs2_codegen *generator,
                                   int32_t context,
                                   const tjs2_codegen_value *values,
                                   int32_t count);

/* Compares the code of a context with the code the caller holds.
   The return value is zero when the two are the same, the number of the
   first word that differs (counting from one) when they are not, and a
   negative value when the lengths differ. */
int32_t tjs2_codegen_compare_code(const tjs2_codegen *generator,
                                   int32_t context,
                                   const int32_t *code,
                                   int32_t length);

#ifdef __cplusplus
}
#endif

#endif /* tjs2CodegenH */
