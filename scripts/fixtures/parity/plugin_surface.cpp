// Self-test fixture for scripts/plugin_parity.py.  It is never compiled; the
// extractor parses it as reference plugin C++ so a regex that silently stops
// matching fails the `--self-test` run instead of quietly shrinking the
// surface.
//
// It exercises every plugin registration mechanism the extractor claims:
//   * NCB_REGISTER_CLASS / NCB_REGISTER_SUBCLASS / NCB_SUBCLASS
//   * NCB_METHOD / NCB_PROPERTY
//   * NCB_ATTACH_FUNCTION / NCB_REGISTER_FUNCTION
//   * global->PropSet free function, namespace->PropSet class attachment
//   * a runtime-built name (must be unrecoverable)
//   * a static member on an unresolved helper parameter (must be unrecoverable)

static void Install(iTJSDispatch2 *global) {
    tTJSVariant val;
    global->PropSet(TJS_MEMBERENSURE, TJS_W("getDirList"), nullptr, &val,
                    global);

    tTJSVariant motionVar;
    global->PropGet(0, TJS_W("Motion"), nullptr, &motionVar, global);
    iTJSDispatch2 *motion = motionVar.AsObjectNoAddRef();
    motion->PropSet(TJS_MEMBERENSURE, TJS_W("Player"), nullptr, &val, motion);

    // A name built at runtime cannot be recovered; the extractor must report
    // the site rather than silently drop it.
    ttstr dynamicName = TJS_W("dynamic");
    global->PropSet(TJS_MEMBERENSURE, dynamicName.c_str(), nullptr, &val,
                    global);
}

NCB_REGISTER_CLASS(Widget) {
    NCB_METHOD(load);
    NCB_PROPERTY(width, getWidth, setWidth);
    NCB_SUBCLASS(Part, Part);
}

NCB_REGISTER_SUBCLASS(Part) {
    NCB_METHOD(poke);
}

NCB_ATTACH_FUNCTION(ping, Gadget, ping);

NCB_REGISTER_FUNCTION(helper, helper_impl);

TVPExecuteScript(TJS_W("function drawFFTGraph(){}"));

static void PostRegist(iTJSDispatch2 *playerClass) {
    tTJSVariant marker;
    // ``playerClass`` is a helper parameter; the class is not statically bound.
    playerClass->PropSet(TJS_MEMBERENSURE | TJS_STATICMEMBER,
                         TJS_W("useD3D"), nullptr, &marker, playerClass);
}
