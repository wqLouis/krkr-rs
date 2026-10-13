// Guard self-test near miss: a file-level allow inside the reviewed FFI
// boundary (compat source) is accepted.
#![allow(unsafe_code)]

pub fn harmless() {}
