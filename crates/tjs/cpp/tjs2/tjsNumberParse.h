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

#include "tjsConfig.h"
#include "tjsVariant.h"

namespace TJS {
    //---------------------------------------------------------------------------
    extern tjs_int TJSHexNum(tjs_char ch) noexcept;

    bool TJSParseNumber(tTJSVariant &val, const tjs_char **ptr);
    //---------------------------------------------------------------------------
} // namespace TJS

#endif
