//---------------------------------------------------------------------------
/*
        TJS2 Script Engine
        Copyright (C) 2000 W.Dee <dee@kikyou.info> and contributors

        See details of license at "license.txt"
*/
//---------------------------------------------------------------------------
// TJS2 number parsing
//---------------------------------------------------------------------------
#include "tjsCommHead.h"

#include "tjsNumberParse.h"
#include "tjsVariant.h"
#include "tjsError.h"
#include "tjsUtils.h"

namespace TJS {
    //---------------------------------------------------------------------------
    const tjs_char TJS_SKIP_CODE = (tjs_char) ~((tjs_char)0);
    //---------------------------------------------------------------------------

    //---------------------------------------------------------------------------
    static bool TJS_iswspace(tjs_char ch) {
        if(ch & 0xff00)
            return false;
        else
            return 0 != isspace(ch);
    }

    //---------------------------------------------------------------------------
    static bool TJS_iswalpha(tjs_char ch) {
        if(ch & 0xff00)
            return true;
        else
            return 0 != isalpha(ch);
    }

    //---------------------------------------------------------------------------
    static void _TJSNext(const tjs_char **ptr) {
        do {
            (*ptr)++;
        } while(*(*ptr) && *(*ptr) == TJS_SKIP_CODE);
    }

    static bool TJSNext(const tjs_char **ptr) {
        (*ptr)++;
        if(*(*ptr) == TJS_SKIP_CODE)
            _TJSNext(ptr);
        return *(*ptr) != 0;
    }
    //---------------------------------------------------------------------------

    //---------------------------------------------------------------------------
    // TJSSkipSpace
    //---------------------------------------------------------------------------
    static bool TJSSkipSpace(const tjs_char **ptr) {
        while(*(*ptr) && (*(*ptr) == TJS_SKIP_CODE || TJS_iswspace(*(*ptr))))
            (*ptr)++;
        return *(*ptr) != 0;
    }

    //---------------------------------------------------------------------------
    // TJSHexNum
    tjs_int TJSHexNum(tjs_char ch) noexcept {
        if(ch >= TJS_W('a') && ch <= TJS_W('f'))
            return ch - TJS_W('a') + 10;
        if(ch >= TJS_W('A') && ch <= TJS_W('F'))
            return ch - TJS_W('A') + 10;
        if(ch >= TJS_W('0') && ch <= TJS_W('9'))
            return ch - TJS_W('0');
        return -1;
    }

    //---------------------------------------------------------------------------

    //---------------------------------------------------------------------------
    // TJSOctNum
    //---------------------------------------------------------------------------
    static tjs_int TJSOctNum(tjs_char ch) noexcept {
        if(ch >= TJS_W('0') && ch <= TJS_W('7'))
            return ch - TJS_W('0');
        return -1;
    }

    //---------------------------------------------------------------------------

    //---------------------------------------------------------------------------
    // TJSDecNum
    //---------------------------------------------------------------------------
    static tjs_int TJSDecNum(tjs_char ch) noexcept {
        if(ch >= TJS_W('0') && ch <= TJS_W('9'))
            return ch - TJS_W('0');
        return -1;
    }

    //---------------------------------------------------------------------------

    //---------------------------------------------------------------------------
    // TJSBinNum
    //---------------------------------------------------------------------------
    static tjs_int TJSBinNum(tjs_char ch) noexcept {
        if(ch == TJS_W('0'))
            return 0;
        if(ch == TJS_W('1'))
            return 1;
        return -1;
    }

    //---------------------------------------------------------------------------
    // TJSStringMatch
    //---------------------------------------------------------------------------
    static bool TJSStringMatch(const tjs_char **sc, const tjs_char *wrd, bool isword) {
        // compare string with a script starting from sc and wrd.
        // word matching is processed if isword is true.
        const tjs_char *save = (*sc);
        while(*wrd && *(*sc)) {
            if(*(*sc) != *wrd)
                break;
            TJSNext(sc);
            wrd++;
        }

        if(*wrd) {
            (*sc) = save;
            return false;
        }
        if(isword) {
            if(TJS_iswalpha(*(*sc)) || *(*sc) == TJS_W('_')) {
                (*sc) = save;
                return false;
            }
        }
        return true;
    }

    //---------------------------------------------------------------------------
    // TJSParseNumber
    //---------------------------------------------------------------------------
    static tTJSString TJSExtractNumber(tjs_int (*validdigits)(tjs_char ch),
                                       const tjs_char *expmark,
                                       const tjs_char **ptr, bool &isreal) {
        tTJSString tmp;

        bool point_found = false;
        bool exp_found = false;
        while(true) {
            if(validdigits(**ptr) != -1 && !exp_found) {
                tmp += **ptr;
                if(!TJSNext(ptr))
                    break;
            } else if(**ptr == TJS_W('.') && !point_found && !exp_found) {
                point_found = true;
                tmp += **ptr;
                if(!TJSNext(ptr))
                    break;
            } else if((**ptr == expmark[0] || **ptr == expmark[1]) &&
                      !exp_found) {
                exp_found = true;
                tmp += **ptr;
                if(!TJSNext(ptr))
                    break;
                if(!TJSSkipSpace(ptr))
                    break;
                if(**ptr == TJS_W('+')) {
                    tmp += **ptr;
                    if(!TJSNext(ptr))
                        break;
                    if(!TJSSkipSpace(ptr))
                        break;
                } else if(**ptr == TJS_W('-')) {
                    tmp += **ptr;
                    if(!TJSNext(ptr))
                        break;
                    if(!TJSSkipSpace(ptr))
                        break;
                }
            } else if(TJSDecNum(**ptr) != -1 && exp_found) {
                tmp += **ptr;
                if(!TJSNext(ptr))
                    break;
            } else {
                break;
            }
        }

        isreal = point_found || exp_found;

        return tmp;
    }

    static bool TJSParseNonDecimalReal(tTJSVariant &val, const tjs_char **ptr,
                                       tjs_int (*validdigits)(tjs_char ch),
                                       tjs_int basebits) {
        // parse non-decimal(hexiadecimal, octal or binary)
        // floating-point number. this routine heavily depends on IEEE
        // double floating-point number expression.

        tjs_uint64 main = TJS_UI64_VAL(0); // significand
        tjs_int exp = 0; // 2^n exponental
        tjs_int numsignif = 0; // significand bit count (including
                               // leading left-most '1') in "main"
        bool pointpassed = false;

        // scan input
        while(true) {
            if(**ptr == TJS_W('.')) {
                pointpassed = true;
            } else if(**ptr == TJS_W('p') || **ptr == TJS_W('P')) {
                if(!TJSNext(ptr))
                    break;
                if(!TJSSkipSpace(ptr))
                    break;

                bool biassign = false;
                if(**ptr == TJS_W('+')) {
                    biassign = false;
                    if(!TJSNext(ptr))
                        break;
                    if(!TJSSkipSpace(ptr))
                        break;
                }

                if(**ptr == TJS_W('-')) {
                    biassign = true;
                    if(!TJSNext(ptr))
                        break;
                    if(!TJSSkipSpace(ptr))
                        break;
                }

                tjs_int bias = 0;
                while(true) {
                    bias *= 10;
                    bias += TJSDecNum(**ptr);
                    if(!TJSNext(ptr))
                        break;
                }
                if(biassign)
                    bias = -bias;
                exp += bias;
                break;
            } else {
                tjs_int n = validdigits(**ptr);
                if(numsignif == 0) {
                    // find msb flaged bit
                    tjs_int b = basebits - 1;
                    while(b >= 0) {
                        if((1 << b) & n)
                            break;
                        b--;
                    }

                    b++;
                    if(b) {
                        // n is not zero
                        // place it to the main's msb
                        numsignif = b;
                        main |= ((tjs_uint64)n << (64 - numsignif));
                        if(pointpassed)
                            exp -= (basebits - b + 1);
                        else
                            exp = b - 1;
                    } else {
                        // n is zero
                        if(pointpassed)
                            exp -= basebits;
                    }
                } else {
                    // append to main
                    if(numsignif + basebits < 64) {
                        numsignif += basebits;
                        main |= ((tjs_uint64)n << (64 - numsignif));
                    }
                    if(!pointpassed)
                        exp += basebits;
                }
            }
            if(!TJSNext(ptr))
                break;
        }

        main >>= (64 - 1 - TJS_IEEE_D_SIGNIFICAND_BITS);

        if(main == 0) {
            // zero
            val = (tTVReal)0.0;
            return true;
        }

        main &= ((TJS_UI64_VAL(1) << TJS_IEEE_D_SIGNIFICAND_BITS) -
                 TJS_UI64_VAL(1));

        if(exp < TJS_IEEE_D_EXP_MIN) {
            // denormal
            // treat as zero
            val = (tTVReal)0.0;
            return true;
        }

        if(exp > TJS_IEEE_D_EXP_MAX) {
            // too large
            // treat as infinity
            tjs_real d;
            *(tjs_uint64 *)&d = TJS_IEEE_D_P_INF;
            val = d;
            return true;
        }

        // compose IEEE double
        tjs_real d;
        *(tjs_uint64 *)&d = TJS_IEEE_D_MAKE_SIGN(0) | TJS_IEEE_D_MAKE_EXP(exp) |
            TJS_IEEE_D_MAKE_SIGNIFICAND(main);
        val = d;
        return true;
    }

    static bool TJSParseNonDecimalInteger(tTJSVariant &val,
                                          const tjs_char **ptr,
                                          tjs_int (*validdigits)(tjs_char ch),
                                          tjs_int basebits) {
        tjs_int64 v = 0;
        while(true) {
            v <<= basebits;
            v += validdigits(**ptr);
            if(!TJSNext(ptr))
                break;
        }
        val = (tTVInteger)v;
        return true;
    }

    static bool TJSParseNonDecimalNumber(tTJSVariant &val, const tjs_char **ptr,
                                         tjs_int (*validdigits)(tjs_char ch),
                                         tjs_int base) {
        bool isreal = false;
        tTJSString tmp(TJSExtractNumber(validdigits, TJS_W("Pp"), ptr, isreal));

        if(tmp.IsEmpty())
            return false;

        const tjs_char *p = tmp.c_str();
        const tjs_char **pp = &p;

        if(isreal)
            return TJSParseNonDecimalReal(val, pp, validdigits, base);
        else
            return TJSParseNonDecimalInteger(val, pp, validdigits, base);
    }

    static bool TJSParseDecimalReal(tTJSVariant &val, const tjs_char **pp) {
        val = (tTVReal)TJS_strtod(*pp, nullptr);
        return true;
    }

    static bool TJSParseDecimalInteger(tTJSVariant &val, const tjs_char **pp) {
        int n;
        tjs_int64 num = 0;
        while((n = TJSDecNum(**pp)) != -1) {
            num *= 10;
            num += n;
            if(!TJSNext(pp))
                break;
        }
        val = (tTVInteger)num;
        return true;
    }

    static bool TJSParseNumber2(tTJSVariant &val, const tjs_char **ptr) {
        // stage 2

        if(TJSStringMatch(ptr, TJS_W("true"), true)) {
            val = (tjs_int) true;
            return true;
        }
        if(TJSStringMatch(ptr, TJS_W("false"), true)) {
            val = (tjs_int) false;
            return true;
        }
        if(TJSStringMatch(ptr, TJS_W("NaN"), true)) {
            // Not a Number
            tjs_real d;
            *(tjs_uint64 *)&d = TJS_IEEE_D_P_NaN;
            val = d;
            return true;
        }
        if(TJSStringMatch(ptr, TJS_W("Infinity"), true)) {
            // positive inifinity
            tjs_real d;
            *(tjs_uint64 *)&d = TJS_IEEE_D_P_INF;
            val = d;
            return true;
        }

        const tjs_char *ptr_save = *ptr;

        if(**ptr == TJS_W('0')) {
            if(!TJSNext(ptr)) {
                val = (tjs_int)0;
                return true;
            }

            tjs_char mark = **ptr;

            if(mark == TJS_W('X') || mark == TJS_W('x')) {
                // hexadecimal
                if(!TJSNext(ptr))
                    return false;
                return TJSParseNonDecimalNumber(val, ptr, TJSHexNum, 4);
            }

            if(mark == TJS_W('B') || mark == TJS_W('b')) {
                // binary
                if(!TJSNext(ptr))
                    return false;
                return TJSParseNonDecimalNumber(val, ptr, TJSBinNum, 1);
            }

            if(mark == TJS_W('.')) {
                // decimal point
                *ptr = ptr_save;
                goto decimal;
            }

            if(mark == TJS_W('E') || mark == TJS_W('e')) {
                // exp
                *ptr = ptr_save;
                goto decimal;
            }

            if(mark == TJS_W('P') || mark == TJS_W('p')) {
                // 2^n exp
                return false;
            }

            // octal
            *ptr = ptr_save;
            return TJSParseNonDecimalNumber(val, ptr, TJSOctNum, 3);
        }

    // integer decimal or real decimal
    decimal:
        bool isreal = false;
        tTJSString tmp(TJSExtractNumber(TJSDecNum, TJS_W("Ee"), ptr, isreal));

        if(tmp.IsEmpty())
            return false;

        const tjs_char *p = tmp.c_str();
        const tjs_char **pp = &p;

        if(isreal)
            return TJSParseDecimalReal(val, pp);
        else
            return TJSParseDecimalInteger(val, pp);
    }

    bool TJSParseNumber(tTJSVariant &val, const tjs_char **ptr) {
        // parse a number pointed by (*ptr)
        TJSSetFPUE();

        bool sign = false; // true if negative

        if(**ptr == TJS_W('+')) {
            sign = false;
            if(!TJSNext(ptr))
                return false;
            if(!TJSSkipSpace(ptr))
                return false;
        } else if(**ptr == TJS_W('-')) {
            sign = true;
            if(!TJSNext(ptr))
                return false;
            if(!TJSSkipSpace(ptr))
                return false;
        }

        if(TJSParseNumber2(val, ptr)) {
            if(sign)
                val = -val;
            return true;
        }

        return false;
    }
    //---------------------------------------------------------------------------
} // namespace TJS
