// Guard self-test near-miss: the module has the same "not constructed" guard
// as the bad fixture, but its create callback completes the payload before it
// returns. The `native-preconstruct` rule must trip nothing here.

pub struct GadgetInst {
    id: Option<u32>,
}

fn scene_id() -> Option<u32> {
    Some(1)
}

extern "C" fn gadget_create(_engine: *mut c_void) -> *mut c_void {
    let mut inst = GadgetInst { id: None };
    if let Some(id) = scene_id() {
        inst.id = Some(id);
    }
    Box::into_raw(Box::new(inst)) as *mut c_void
}

fn gadget_label_get(instance: *mut c_void) -> Result<u32, String> {
    let inst = unsafe { &*(instance as *const GadgetInst) };
    inst.id
        .ok_or_else(|| "Gadget: this gadget is not constructed".to_string())
}
