// Fixture for the native-arity self-test.
//
// `Layer.setClip` defines a zero-argument path that resets the clip rectangle
// and only rejects the one-through-three argument range
// (reference/cpp/core/visual/LayerIntf.cpp:8635-8658). `Layer.setSize`
// requires two arguments and has no zero-argument path, so it must not be
// treated as one.
TJS_BEGIN_NATIVE_MEMBERS(Layer)
    TJS_BEGIN_NATIVE_METHOD_DECL(/*func. name*/ setClip) {
        TJS_GET_NATIVE_INSTANCE(/*var. name*/ _this,
                                /*var. type*/ tTJSNI_Layer);
        if(numparams == 0) {
            _this->ResetClip();
        } else {
            if(numparams < 4)
                return TJS_E_BADPARAMCOUNT;
            _this->SetClip((tjs_int)*param[0], (tjs_int)*param[1],
                           (tjs_int)*param[2], (tjs_int)*param[3]);
        }
        return TJS_S_OK;
    }
    TJS_END_NATIVE_METHOD_DECL(/*func. name*/ setClip)
    TJS_BEGIN_NATIVE_METHOD_DECL(/*func. name*/ setSize) {
        TJS_GET_NATIVE_INSTANCE(/*var. name*/ _this,
                                /*var. type*/ tTJSNI_Layer);
        if(numparams < 2)
            return TJS_E_BADPARAMCOUNT;
        _this->SetSize((tjs_int)*param[0], (tjs_int)*param[1]);
        return TJS_S_OK;
    }
    TJS_END_NATIVE_METHOD_DECL(/*func. name*/ setSize)
TJS_END_NATIVE_MEMBERS
