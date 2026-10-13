// Self-test fixture for scripts/native_parity.py's getter/setter rule: the
// extra-accessor case.  It is never compiled; the extractor parses it as Rust
// source.  `readWrite` matches the reference, but `readOnly` is registered
// with a setter the reference denies (`TJS_DENY_NATIVE_PROP_SETTER`) — the
// reverse of the `Layer.imageWidth` shape.

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
                set: Some(prop_read_only_set),
            },
        ],
    });
}
