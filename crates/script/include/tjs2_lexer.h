/* The C interface of the lexical analyser.
 *
 * The interface is what the vendored parser reaches the analyser through:
 * a plain C representation, no Rust types across the boundary. The state of
 * one analyser is an opaque handle, a token kind is the grammar's own
 * number, and a value is delivered in the form the native side already uses --
 * an integer, a real, a string of code units, a buffer of bytes, or the null
 * object.
 *
 * Every entry point catches a panic and reports it as an error, so a
 * failure never unwinds into the caller.
 *
 * Ownership: a handle returned by tjs2_lexer_new belongs to the caller and
 * must be freed with tjs2_lexer_free. The buffers and the text the value
 * and the error functions return belong to the handle and stay valid until
 * the next read on the same handle.
 */

#ifndef TJS2_LEXER_H
#define TJS2_LEXER_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* The analyser read the script to its end. */
#define TJS2_LEX_OK 0
/* A string, a regular expression or a binary literal was not terminated. */
#define TJS2_LEX_ERROR_LITERAL 1
/* A number could not be parsed. */
#define TJS2_LEX_ERROR_NUMBER 2
/* A comment was not terminated. */
#define TJS2_LEX_ERROR_COMMENT 3
/* A character cannot begin a token. */
#define TJS2_LEX_ERROR_CHARACTER 4
/* A conditional-compilation statement or expression is malformed. */
#define TJS2_LEX_ERROR_CONDITIONAL 5
/* The analyser panicked. The message carries the panic's text. */
#define TJS2_LEX_ERROR_PANIC 6

/* The token carries no value. */
#define TJS2_LEX_VALUE_NONE 0
/* The token carries the null object. */
#define TJS2_LEX_VALUE_NULL 1
/* The token carries an integer. */
#define TJS2_LEX_VALUE_INTEGER 2
/* The token carries a real. */
#define TJS2_LEX_VALUE_REAL 3
/* The token carries a string of code units. */
#define TJS2_LEX_VALUE_STRING 4
/* The token carries a buffer of bytes. */
#define TJS2_LEX_VALUE_OCTET 5

/* The token kind of the end of the script. */
#define TJS2_LEX_TOKEN_END 0

/* The state of one analyser. */
typedef struct tjs2_lexer tjs2_lexer;

/* Create an analyser for a script.
 *
 * The script is a sequence of UTF-16 code units; length is their number.
 * The analyser copies the text, so the caller may free it as soon as this
 * call returns.
 *
 * expression_mode reads the script as an expression and result_needed asks
 * for the token that returns the expression's value; the second is only
 * read in the expression mode.
 *
 * Returns a handle the caller owns, or null when the allocation failed. */
tjs2_lexer *tjs2_lexer_new(const uint16_t *script, uint32_t length,
                           int expression_mode, int result_needed);

/* Free an analyser. The handle must not be used afterwards. */
void tjs2_lexer_free(tjs2_lexer *lexer);

/* Read the next token.
 *
 * Returns the token's kind as the grammar numbers it, zero when the script
 * has ended, or a negative value when the analyser failed. After a failure
 * tjs2_lexer_error gives the code and tjs2_lexer_error_message the text.
 *
 * The value of the token is read with the tjs2_lexer_value_* functions. */
int tjs2_lexer_next(tjs2_lexer *lexer);

/* The error code of the last read; TJS2_LEX_OK when it succeeded. */
int tjs2_lexer_error(const tjs2_lexer *lexer);

/* The text of the error of the last read.
 *
 * Returns a NUL-terminated string owned by the handle, or null when there
 * is no error text. */
const char *tjs2_lexer_error_message(const tjs2_lexer *lexer);

/* The kind of the value of the last token; one of TJS2_LEX_VALUE_*. */
int tjs2_lexer_value_kind(const tjs2_lexer *lexer);

/* The integer of the last token; zero unless it carries an integer. */
int64_t tjs2_lexer_value_integer(const tjs2_lexer *lexer);

/* The real of the last token; zero unless it carries a real. */
double tjs2_lexer_value_real(const tjs2_lexer *lexer);

/* The code units of the last token.
 *
 * The length is written to length when it is not null. Returns a buffer
 * owned by the handle; a token that carries no string has a length of
 * zero. */
const uint16_t *tjs2_lexer_value_string(const tjs2_lexer *lexer,
                                         uint32_t *length);

/* The bytes of the last token.
 *
 * The length is written to length when it is not null. Returns a buffer
 * owned by the handle; a token that carries no buffer has a length of
 * zero. */
const uint8_t *tjs2_lexer_value_octet(const tjs2_lexer *lexer,
                                       uint32_t *length);

/* The position the analyser has read up to, counted in code units. */
uint32_t tjs2_lexer_position(const tjs2_lexer *lexer);

/* Tell the analyser that the next token is a regular expression literal.
 * The call must come before the next token is read. */
void tjs2_lexer_expect_regexp(tjs2_lexer *lexer);

/* Tell the analyser that the next name is an ordinary name, not a reserved
 * word. The call must come before the next token is read. */
void tjs2_lexer_expect_bare_word(tjs2_lexer *lexer);

#ifdef __cplusplus
}
#endif

#endif /* TJS2_LEXER_H */
