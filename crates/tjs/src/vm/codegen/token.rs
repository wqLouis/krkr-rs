//! The tokens of the grammar.
//!
//! The numbering is the grammar's: the grammar's token numbers are part
//! of the tree the generator walks, so they must match the grammar's
//! exactly.

/// A token of the grammar.
pub type Token = i32;

/// The `T_COMMA` token.
pub const T_COMMA: Token = 258;
/// The `T_EQUAL` token.
pub const T_EQUAL: Token = 259;
/// The `T_AMPERSANDEQUAL` token.
pub const T_AMPERSANDEQUAL: Token = 260;
/// The `T_VERTLINEEQUAL` token.
pub const T_VERTLINEEQUAL: Token = 261;
/// The `T_CHEVRONEQUAL` token.
pub const T_CHEVRONEQUAL: Token = 262;
/// The `T_MINUSEQUAL` token.
pub const T_MINUSEQUAL: Token = 263;
/// The `T_PLUSEQUAL` token.
pub const T_PLUSEQUAL: Token = 264;
/// The `T_PERCENTEQUAL` token.
pub const T_PERCENTEQUAL: Token = 265;
/// The `T_SLASHEQUAL` token.
pub const T_SLASHEQUAL: Token = 266;
/// The `T_BACKSLASHEQUAL` token.
pub const T_BACKSLASHEQUAL: Token = 267;
/// The `T_ASTERISKEQUAL` token.
pub const T_ASTERISKEQUAL: Token = 268;
/// The `T_LOGICALOREQUAL` token.
pub const T_LOGICALOREQUAL: Token = 269;
/// The `T_LOGICALANDEQUAL` token.
pub const T_LOGICALANDEQUAL: Token = 270;
/// The `T_RBITSHIFTEQUAL` token.
pub const T_RBITSHIFTEQUAL: Token = 271;
/// The `T_LARITHSHIFTEQUAL` token.
pub const T_LARITHSHIFTEQUAL: Token = 272;
/// The `T_RARITHSHIFTEQUAL` token.
pub const T_RARITHSHIFTEQUAL: Token = 273;
/// The `T_QUESTION` token.
pub const T_QUESTION: Token = 274;
/// The `T_LOGICALOR` token.
pub const T_LOGICALOR: Token = 275;
/// The `T_LOGICALAND` token.
pub const T_LOGICALAND: Token = 276;
/// The `T_VERTLINE` token.
pub const T_VERTLINE: Token = 277;
/// The `T_CHEVRON` token.
pub const T_CHEVRON: Token = 278;
/// The `T_AMPERSAND` token.
pub const T_AMPERSAND: Token = 279;
/// The `T_NOTEQUAL` token.
pub const T_NOTEQUAL: Token = 280;
/// The `T_EQUALEQUAL` token.
pub const T_EQUALEQUAL: Token = 281;
/// The `T_DISCNOTEQUAL` token.
pub const T_DISCNOTEQUAL: Token = 282;
/// The `T_DISCEQUAL` token.
pub const T_DISCEQUAL: Token = 283;
/// The `T_SWAP` token.
pub const T_SWAP: Token = 284;
/// The `T_LT` token.
pub const T_LT: Token = 285;
/// The `T_GT` token.
pub const T_GT: Token = 286;
/// The `T_LTOREQUAL` token.
pub const T_LTOREQUAL: Token = 287;
/// The `T_GTOREQUAL` token.
pub const T_GTOREQUAL: Token = 288;
/// The `T_RARITHSHIFT` token.
pub const T_RARITHSHIFT: Token = 289;
/// The `T_LARITHSHIFT` token.
pub const T_LARITHSHIFT: Token = 290;
/// The `T_RBITSHIFT` token.
pub const T_RBITSHIFT: Token = 291;
/// The `T_PERCENT` token.
pub const T_PERCENT: Token = 292;
/// The `T_SLASH` token.
pub const T_SLASH: Token = 293;
/// The `T_BACKSLASH` token.
pub const T_BACKSLASH: Token = 294;
/// The `T_ASTERISK` token.
pub const T_ASTERISK: Token = 295;
/// The `T_EXCRAMATION` token.
pub const T_EXCRAMATION: Token = 296;
/// The `T_TILDE` token.
pub const T_TILDE: Token = 297;
/// The `T_DECREMENT` token.
pub const T_DECREMENT: Token = 298;
/// The `T_INCREMENT` token.
pub const T_INCREMENT: Token = 299;
/// The `T_NEW` token.
pub const T_NEW: Token = 300;
/// The `T_DELETE` token.
pub const T_DELETE: Token = 301;
/// The `T_TYPEOF` token.
pub const T_TYPEOF: Token = 302;
/// The `T_PLUS` token.
pub const T_PLUS: Token = 303;
/// The `T_MINUS` token.
pub const T_MINUS: Token = 304;
/// The `T_SHARP` token.
pub const T_SHARP: Token = 305;
/// The `T_DOLLAR` token.
pub const T_DOLLAR: Token = 306;
/// The `T_ISVALID` token.
pub const T_ISVALID: Token = 307;
/// The `T_INVALIDATE` token.
pub const T_INVALIDATE: Token = 308;
/// The `T_INSTANCEOF` token.
pub const T_INSTANCEOF: Token = 309;
/// The `T_LPARENTHESIS` token.
pub const T_LPARENTHESIS: Token = 310;
/// The `T_DOT` token.
pub const T_DOT: Token = 311;
/// The `T_LBRACKET` token.
pub const T_LBRACKET: Token = 312;
/// The `T_THIS` token.
pub const T_THIS: Token = 313;
/// The `T_SUPER` token.
pub const T_SUPER: Token = 314;
/// The `T_GLOBAL` token.
pub const T_GLOBAL: Token = 315;
/// The `T_RBRACKET` token.
pub const T_RBRACKET: Token = 316;
/// The `T_CLASS` token.
pub const T_CLASS: Token = 317;
/// The `T_RPARENTHESIS` token.
pub const T_RPARENTHESIS: Token = 318;
/// The `T_COLON` token.
pub const T_COLON: Token = 319;
/// The `T_SEMICOLON` token.
pub const T_SEMICOLON: Token = 320;
/// The `T_LBRACE` token.
pub const T_LBRACE: Token = 321;
/// The `T_RBRACE` token.
pub const T_RBRACE: Token = 322;
/// The `T_CONTINUE` token.
pub const T_CONTINUE: Token = 323;
/// The `T_FUNCTION` token.
pub const T_FUNCTION: Token = 324;
/// The `T_DEBUGGER` token.
pub const T_DEBUGGER: Token = 325;
/// The `T_DEFAULT` token.
pub const T_DEFAULT: Token = 326;
/// The `T_CASE` token.
pub const T_CASE: Token = 327;
/// The `T_EXTENDS` token.
pub const T_EXTENDS: Token = 328;
/// The `T_FINALLY` token.
pub const T_FINALLY: Token = 329;
/// The `T_PROPERTY` token.
pub const T_PROPERTY: Token = 330;
/// The `T_PRIVATE` token.
pub const T_PRIVATE: Token = 331;
/// The `T_PUBLIC` token.
pub const T_PUBLIC: Token = 332;
/// The `T_PROTECTED` token.
pub const T_PROTECTED: Token = 333;
/// The `T_STATIC` token.
pub const T_STATIC: Token = 334;
/// The `T_RETURN` token.
pub const T_RETURN: Token = 335;
/// The `T_BREAK` token.
pub const T_BREAK: Token = 336;
/// The `T_EXPORT` token.
pub const T_EXPORT: Token = 337;
/// The `T_IMPORT` token.
pub const T_IMPORT: Token = 338;
/// The `T_SWITCH` token.
pub const T_SWITCH: Token = 339;
/// The `T_IN` token.
pub const T_IN: Token = 340;
/// The `T_INCONTEXTOF` token.
pub const T_INCONTEXTOF: Token = 341;
/// The `T_FOR` token.
pub const T_FOR: Token = 342;
/// The `T_WHILE` token.
pub const T_WHILE: Token = 343;
/// The `T_DO` token.
pub const T_DO: Token = 344;
/// The `T_IF` token.
pub const T_IF: Token = 345;
/// The `T_VAR` token.
pub const T_VAR: Token = 346;
/// The `T_CONST` token.
pub const T_CONST: Token = 347;
/// The `T_ENUM` token.
pub const T_ENUM: Token = 348;
/// The `T_GOTO` token.
pub const T_GOTO: Token = 349;
/// The `T_THROW` token.
pub const T_THROW: Token = 350;
/// The `T_TRY` token.
pub const T_TRY: Token = 351;
/// The `T_SETTER` token.
pub const T_SETTER: Token = 352;
/// The `T_GETTER` token.
pub const T_GETTER: Token = 353;
/// The `T_ELSE` token.
pub const T_ELSE: Token = 354;
/// The `T_CATCH` token.
pub const T_CATCH: Token = 355;
/// The `T_OMIT` token.
pub const T_OMIT: Token = 356;
/// The `T_SYNCHRONIZED` token.
pub const T_SYNCHRONIZED: Token = 357;
/// The `T_WITH` token.
pub const T_WITH: Token = 358;
/// The `T_INT` token.
pub const T_INT: Token = 359;
/// The `T_REAL` token.
pub const T_REAL: Token = 360;
/// The `T_STRING` token.
pub const T_STRING: Token = 361;
/// The `T_OCTET` token.
pub const T_OCTET: Token = 362;
/// The `T_FALSE` token.
pub const T_FALSE: Token = 363;
/// The `T_NULL` token.
pub const T_NULL: Token = 364;
/// The `T_TRUE` token.
pub const T_TRUE: Token = 365;
/// The `T_VOID` token.
pub const T_VOID: Token = 366;
/// The `T_NAN` token.
pub const T_NAN: Token = 367;
/// The `T_INFINITY` token.
pub const T_INFINITY: Token = 368;
/// The `T_UPLUS` token.
pub const T_UPLUS: Token = 369;
/// The `T_UMINUS` token.
pub const T_UMINUS: Token = 370;
/// The `T_EVAL` token.
pub const T_EVAL: Token = 371;
/// The `T_POSTDECREMENT` token.
pub const T_POSTDECREMENT: Token = 372;
/// The `T_POSTINCREMENT` token.
pub const T_POSTINCREMENT: Token = 373;
/// The `T_IGNOREPROP` token.
pub const T_IGNOREPROP: Token = 374;
/// The `T_PROPACCESS` token.
pub const T_PROPACCESS: Token = 375;
/// The `T_ARG` token.
pub const T_ARG: Token = 376;
/// The `T_EXPANDARG` token.
pub const T_EXPANDARG: Token = 377;
/// The `T_INLINEARRAY` token.
pub const T_INLINEARRAY: Token = 378;
/// The `T_ARRAYARG` token.
pub const T_ARRAYARG: Token = 379;
/// The `T_INLINEDIC` token.
pub const T_INLINEDIC: Token = 380;
/// The `T_DICELM` token.
pub const T_DICELM: Token = 381;
/// The `T_WITHDOT` token.
pub const T_WITHDOT: Token = 382;
/// The `T_THIS_PROXY` token.
pub const T_THIS_PROXY: Token = 383;
/// The `T_WITHDOT_PROXY` token.
pub const T_WITHDOT_PROXY: Token = 384;
/// The `T_CONSTVAL` token.
pub const T_CONSTVAL: Token = 385;
/// The `T_SYMBOL` token.
pub const T_SYMBOL: Token = 386;
/// The `T_REGEXP` token.
pub const T_REGEXP: Token = 387;
/// The `T_VARIANT` token.
pub const T_VARIANT: Token = 388;
