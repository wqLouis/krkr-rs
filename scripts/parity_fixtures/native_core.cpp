// Self-test fixture for scripts/native_parity.py.  It is never compiled; the
// extractor parses it as reference C++ so a regex that silently stops matching
// fails the `--self-test` run instead of quietly shrinking the surface.
//
// It exercises every core registration mechanism the extractor claims:
//   * TJS_BEGIN_NATIVE_MEMBERS + METHOD/PROP/STATIC method/prop declarations
//   * TJS_END_NATIVE_CONSTRUCTOR_DECL
//   * registerObject(TJS_W("X"), (var = TVPCreateNativeClass_X()))
//   * var->PropSet(TJS_MEMBERENSURE, TJS_W("name"), ...)
//   * var->PropSet with a runtime-built name (must be unrecoverable)
//   * var->PropSetByNum (numeric name, must be unrecoverable)

tTJSNativeClass_Widget::tTJSNativeClass_Widget() :
    tTJSNativeClass(TJS_W("Widget")) {
    TJS_BEGIN_NATIVE_MEMBERS(Widget)
    TJS_BEGIN_NATIVE_METHOD_DECL(load) {
        return TJS_S_OK;
    }
    TJS_END_NATIVE_METHOD_DECL(load)

    TJS_BEGIN_NATIVE_PROP_DECL(width) {
        return TJS_S_OK;
    }
    TJS_END_NATIVE_PROP_DECL(width)

    TJS_BEGIN_NATIVE_STATIC_METHOD_DECL(create) {
        return TJS_S_OK;
    }
    TJS_END_NATIVE_STATIC_METHOD_DECL(create)

    TJS_BEGIN_NATIVE_STATIC_PROP_DECL(count) {
        return TJS_S_OK;
    }
    TJS_END_NATIVE_STATIC_PROP_DECL(count)

    TJS_BEGIN_NATIVE_CONSTRUCTOR_DECL(Widget) {
        return TJS_S_OK;
    }
    TJS_END_NATIVE_CONSTRUCTOR_DECL(Widget)
    TJS_END_NATIVE_MEMBERS
}

iTJSDispatch2 *TVPCreateNativeClass_Widget() {
    tTJSNativeClass *cls = new tTJSNativeClass_Widget();
    return cls;
}

void InstallWidgets(iTJSDispatch2 *global) {
    iTJSDispatch2 *widgetclass = nullptr;
    registerObject(TJS_W("Widget"),
                   (widgetclass = TVPCreateNativeClass_Widget()));

    tTJSVariant val;
    iTJSDispatch2 *dsp = new tTJSNC_MenuItem();
    val = tTJSVariant(dsp);
    dsp->Release();

    widgetclass->PropSet(TJS_MEMBERENSURE, TJS_W("menu"), nullptr, &val,
                         widgetclass);
    widgetclass->PropSet(TJS_MEMBERENSURE | TJS_STATICMEMBER,
                         TJS_W("SubClass"), nullptr, &val, widgetclass);

    // A name built at runtime cannot be recovered from the source; the
    // extractor must report the site rather than silently drop it.
    ttstr name = TJS_W("dynamic");
    widgetclass->PropSet(TJS_MEMBERENSURE, name.c_str(), nullptr, &val,
                         widgetclass);
    // Numeric members are not named members either.
    widgetclass->PropSetByNum(TJS_MEMBERENSURE, 0, &val, widgetclass);

    iTJSDispatch2 *gclass = nullptr;
    registerObject(TJS_W("Gadget"),
                   (gclass = TVPCreateNativeClass_Gadget()));
    gclass->PropSet(TJS_MEMBERENSURE, TJS_W("ping"), nullptr, &val, gclass);
}
