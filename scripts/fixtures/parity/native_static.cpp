// Self-test fixture for scripts/native_parity.py's static-kind rule.  It is
// never compiled; the extractor parses it as reference C++.
//
// `mainWindow` is marked static with the trailing `/* static */` comment form
// (reference `WindowIntf.cpp:1737`) and `rasterizer` with the
// `TJS_END_NATIVE_STATIC_PROP_DECL` macro form (`LayerIntf.cpp:12174`).
// `savedWindow` is an ordinary instance property.  The Rust fixtures must
// match: the "good" one registers both statics on the class object, the "bad"
// one registers `mainWindow` on the instance builder (the `Window.mainWindow`
// crash shape).

tTJSNativeClass_StaticWidget::tTJSNativeClass_StaticWidget() :
    tTJSNativeClass(TJS_W("StaticWidget")) {
    TJS_BEGIN_NATIVE_MEMBERS(StaticWidget)
    TJS_BEGIN_NATIVE_PROP_DECL(mainWindow) /* static */
    { TJS_BEGIN_NATIVE_PROP_GETTER {
        return TJS_S_OK;
    }
    TJS_END_NATIVE_PROP_GETTER
    TJS_DENY_NATIVE_PROP_SETTER
    }
    TJS_END_NATIVE_PROP_DECL(mainWindow)

    TJS_BEGIN_NATIVE_PROP_DECL(rasterizer) {
        TJS_BEGIN_NATIVE_PROP_GETTER {
            return TJS_S_OK;
        }
        TJS_END_NATIVE_PROP_GETTER
        TJS_DENY_NATIVE_PROP_SETTER
    }
    TJS_END_NATIVE_STATIC_PROP_DECL(rasterizer)

    TJS_BEGIN_NATIVE_PROP_DECL(savedWindow) {
        TJS_BEGIN_NATIVE_PROP_GETTER {
            return TJS_S_OK;
        }
        TJS_END_NATIVE_PROP_GETTER
        TJS_DENY_NATIVE_PROP_SETTER
    }
    TJS_END_NATIVE_PROP_DECL(savedWindow)
    TJS_END_NATIVE_MEMBERS
}

iTJSDispatch2 *TVPCreateNativeClass_StaticWidget() {
    tTJSNativeClass *cls = new tTJSNativeClass_StaticWidget();
    return cls;
}
