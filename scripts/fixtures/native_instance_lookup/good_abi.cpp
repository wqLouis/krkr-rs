// Good fixture for the native-instance lookup gate.
//
// The class lookup resolves an invalidated instance; direct dispatch does not.
// Both accessors and all four dispatch functions must keep that split.

struct tjs2_native_instance {
    bool valid;
    bool invalidated;
    void *native_ptr;

    // Dispatch accessor: a finalized instance is not callable.
    void *GetNativePtr() const {
        return (valid && !invalidated) ? native_ptr : nullptr;
    }

    // Class lookup: NativeInstanceSupport still finds the instance after
    // Invalidate, so an argument lookup must see the class.
    void *GetNativePtrForLookup() const {
        return valid ? native_ptr : nullptr;
    }
};

tjs_error tjs2_dispatch_native_instance_method(
    tjs2_native_instance_method_dispatch *self, tTJSVariant *result,
    tjs_int numparams, tTJSVariant **param, iTJSDispatch2 *objthis) {
    void *instance =
        static_cast<tjs2_native_instance *>(native)->GetNativePtr();
    return TJS_S_OK;
}

tjs_error tjs2_dispatch_native_instance_property_get(
    tjs2_native_instance_property_dispatch *self, tTJSVariant *result,
    iTJSDispatch2 *objthis) {
    void *instance =
        static_cast<tjs2_native_instance *>(native)->GetNativePtr();
    return TJS_S_OK;
}

tjs_error tjs2_dispatch_native_instance_property_set(
    tjs2_native_instance_property_dispatch *self, const tTJSVariant *param,
    iTJSDispatch2 *objthis) {
    void *instance =
        static_cast<tjs2_native_instance *>(native)->GetNativePtr();
    return TJS_S_OK;
}

tjs_error tjs2_native_instance_constructor_dispatch::FuncCall(
    tjs_uint32 flag, const tjs_char *membername, tjs_uint32 *hint,
    tTJSVariant *result, tjs_int numparams, tTJSVariant **param,
    iTJSDispatch2 *objthis) {
    void *instance =
        static_cast<tjs2_native_instance *>(native)->GetNativePtr();
    return TJS_S_OK;
}

void *tjs2_get_native_instance(tjs2_engine *e, tjs2_value_id obj,
                               const char *class_name_utf8) {
    TJS::iTJSNativeInstance *native = nullptr;
    // NativeInstanceSupport(TJS_NIS_GETINSTANCE, classid) has already found
    // the live instance; the payload is the class identity.
    return static_cast<tjs2_native_instance *>(native)
        ->GetNativePtrForLookup();
}
