// Guard self-test fixture: the `native-preconstruct` rule MUST flag the
// `widget_create` callback below. Not compiled and not part of any crate.
//
// The shape being caught: the VM calls `widget_create` (CreateNativeInstance)
// before the script constructor, a member helper raises on a payload whose id
// is still `None`, and the create callback leaves it `None`.

pub struct WidgetInst {
    id: Option<u32>,
}

impl Default for WidgetInst {
    fn default() -> Self {
        Self { id: None }
    }
}

extern "C" fn widget_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(WidgetInst::default())) as *mut c_void
}

extern "C" fn widget_ctor(instance: *mut c_void) {
    // … the constructor is where the payload would be completed.
    let _ = instance;
}

fn widget_label_get(instance: *mut c_void) -> Result<u32, String> {
    let inst = unsafe { &*(instance as *const WidgetInst) };
    inst.id
        .ok_or_else(|| "Widget: this widget is not constructed".to_string())
}
