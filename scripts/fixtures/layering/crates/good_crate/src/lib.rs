//! Guard self-test near miss: a crate lib.rs that enables the missing-docs
//! lint is accepted.
#![warn(missing_docs)]

pub fn documented() -> i32 {
    0
}
