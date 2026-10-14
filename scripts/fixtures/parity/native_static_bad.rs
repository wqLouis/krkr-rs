// Self-test fixture for scripts/native_parity.py's static-kind rule: the
// failing case.  It is never compiled; the extractor parses it as Rust source.
// `mainWindow` is registered on the *instance* builder even though the
// reference declares it static — the `Window.mainWindow` crash shape — and
// `savedWindow` is registered on the class object even though the reference
// declares it an instance member, the reverse direction.

fn register_static_widget(ctx: &mut CompatCtx) {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "StaticWidget",
        methods: vec![],
        properties: vec![NativeInstancePropertyDef {
            name: "mainWindow",
            get: Some(static_widget_main_window_get),
            set: None,
        }],
    });
    ctx.register_static_members(&NativeStaticMembers {
        class_name: "StaticWidget",
        methods: vec![],
        properties: vec![
            NativePropertyDef {
                name: "rasterizer",
                get: Some(static_widget_rasterizer_get),
                set: None,
            },
            NativePropertyDef {
                name: "savedWindow",
                get: Some(static_widget_saved_window_get),
                set: None,
            },
        ],
    });
}
