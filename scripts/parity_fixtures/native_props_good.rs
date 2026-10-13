// Self-test fixture for scripts/native_parity.py's getter/setter rule: the
// matching case.  It is never compiled; the extractor parses it as Rust
// source.  `readWrite` carries both accessors and `readOnly` the getter the
// reference keeps, so the rule reports nothing.

fn register_prop_widget(ctx: &mut CompatCtx) {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "PropWidget",
        methods: vec![],
        properties: vec![
            NativeInstancePropertyDef {
                name: "readWrite",
                get: Some(prop_read_write_get),
                set: Some(prop_read_write_set),
            },
            NativeInstancePropertyDef {
                name: "readOnly",
                get: Some(prop_read_only_get),
                set: None,
            },
        ],
    });
}
