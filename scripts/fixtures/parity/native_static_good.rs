// Self-test fixture for scripts/native_parity.py's static-kind rule: the
// matching case.  It is never compiled; the extractor parses it as Rust
// source.  `mainWindow` and `rasterizer` are class-object (static) members and
// `savedWindow` an instance member, matching the reference.

fn register_static_widget(ctx: &mut CompatCtx) {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "StaticWidget",
        methods: vec![],
        properties: vec![NativeInstancePropertyDef {
            name: "savedWindow",
            get: Some(static_widget_saved_window_get),
            set: None,
        }],
    });
    ctx.register_static_members(&NativeStaticMembers {
        class_name: "StaticWidget",
        methods: vec![],
        properties: vec![
            NativePropertyDef {
                name: "mainWindow",
                get: Some(static_widget_main_window_get),
                set: None,
            },
            NativePropertyDef {
                name: "rasterizer",
                get: Some(static_widget_rasterizer_get),
                set: None,
            },
        ],
    });
}
