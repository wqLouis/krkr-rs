// Self-test fixture for scripts/native_parity.py's getter/setter rule.  It is
// never compiled; the extractor parses it as reference C++.
//
// `readWrite` has both a getter and a setter; `readOnly` has only a getter
// (`TJS_DENY_NATIVE_PROP_SETTER`).  The Rust fixtures must match: the "good"
// one does, the "bad" one leaves `readWrite` without its setter (the
// `Layer.imageWidth` shape the rule exists to catch).

tTJSNativeClass_PropWidget::tTJSNativeClass_PropWidget() :
    tTJSNativeClass(TJS_W("PropWidget")) {
    TJS_BEGIN_NATIVE_MEMBERS(PropWidget)
    TJS_BEGIN_NATIVE_PROP_DECL(readWrite) {
        TJS_BEGIN_NATIVE_PROP_GETTER {
            return TJS_S_OK;
        }
        TJS_END_NATIVE_PROP_GETTER
        TJS_BEGIN_NATIVE_PROP_SETTER {
            return TJS_S_OK;
        }
        TJS_END_NATIVE_PROP_SETTER
    }
    TJS_END_NATIVE_PROP_DECL(readWrite)

    TJS_BEGIN_NATIVE_PROP_DECL(readOnly) {
        TJS_BEGIN_NATIVE_PROP_GETTER {
            return TJS_S_OK;
        }
        TJS_END_NATIVE_PROP_GETTER
        TJS_DENY_NATIVE_PROP_SETTER
    }
    TJS_END_NATIVE_PROP_DECL(readOnly)
    TJS_END_NATIVE_MEMBERS
}

iTJSDispatch2 *TVPCreateNativeClass_PropWidget() {
    tTJSNativeClass *cls = new tTJSNativeClass_PropWidget();
    return cls;
}
