//---------------------------------------------------------------------------
/*
        TJS2 Script Engine
        Copyright (C) 2000 W.Dee <dee@kikyou.info> and contributors

        See details of license at "license.txt"
*/
//---------------------------------------------------------------------------
// TJS2 number parsing
//---------------------------------------------------------------------------
#ifndef tjsNumberParseH
#define tjsNumberParseH

#include "tjs2_abi.h"

#include "tjsConfig.h"
#include "tjsVariant.h"

namespace TJS {
    //---------------------------------------------------------------------------
    // The hexadecimal value of one code unit, or -1 when it is not a digit.
    inline tjs_int TJSHexNum(tjs_char ch) noexcept {
        if(ch >= TJS_W('a') && ch <= TJS_W('f'))
            return ch - TJS_W('a') + 10;
        if(ch >= TJS_W('A') && ch <= TJS_W('F'))
            return ch - TJS_W('A') + 10;
        if(ch >= TJS_W('0') && ch <= TJS_W('9'))
            return ch - TJS_W('0');
        return -1;
    }

    //---------------------------------------------------------------------------
    // The number at the pointer's text, read by the engine's rule in Rust, and
    // the pointer moved past it. False when the text begins with no number.
    inline bool TJSParseNumber(tTJSVariant &val, const tjs_char **ptr) {
        int kind = 0;
        long long integer = 0;
        double real = 0.0;
        size_t consumed = 0;
        if(!tjs2_number_parse(
               reinterpret_cast<const uint16_t *>(*ptr), &kind, &integer, &real,
               &consumed))
            return false;
        if(kind == TJS2_NUMBER_INTEGER)
            val = (tTVInteger)integer;
        else
            val = (tTVReal)real;
        *ptr += consumed;
        return true;
    }
    //---------------------------------------------------------------------------
} // namespace TJS

#endif
