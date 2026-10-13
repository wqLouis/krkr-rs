// Guard self-test fixture: a file-level allow outside the reviewed FFI
// locations MUST be flagged.
#![allow(unsafe_code)]

pub fn harmless() {}
