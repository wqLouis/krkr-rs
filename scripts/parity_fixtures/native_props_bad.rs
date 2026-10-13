// Self-test fixture for scripts/native_parity.py's getter/setter rule: the
// failing case.  It is never compiled; the extractor parses it as Rust
// source.  `readWrite` is registered without its setter even though the
// reference declares one — the `Layer.imageWidth` shape the rule catches.

fn register_prop_widget(ctx: &mut CompatCtx) {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "PropWidget",
        methods: vec![],
        properties: vec![
            NativeInstancePropertyDef {
                name: "readWrite",
                get: Some(prop_read_write_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "readOnly",
                get: Some(prop_read_only_get),
                set: None,
            },
        ],
    });
}
