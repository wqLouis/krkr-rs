// Guard self-test near-miss: a bare `Default` create callback is legitimate
// when no member helper raises on the unconstructed payload, so this file must
// trip nothing. The turbofish form is exercised here.

pub struct PlainInst {
    value: u32,
}

impl Default for PlainInst {
    fn default() -> Self {
        Self { value: 0 }
    }
}

extern "C" fn plain_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::<PlainInst>::default()) as *mut c_void
}
