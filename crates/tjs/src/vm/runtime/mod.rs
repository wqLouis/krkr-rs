//! The TJS runtime: the value layer, the object model and the interpreter.
//!
//! The script language compiles into intermediate code, and this module is
//! the machine that runs it: the values a script manipulates, the objects
//! that hold its members, and the contexts that execute its code. The front
//! end that produces the intermediate code is `super::codegen`.
//!
//! The runtime is built one part at a time, bottom up, and the module grows
//! with it. What it holds today is the **value layer**:
//!
//! | part | module | what it is |
//! |---|---|---|
//! | the string | [`string`] | the heap string a script's text values are |
//! | the number conversion | [`number`] | the text-to-number rules a string's arithmetic uses |
//! | the member table | [`hash`] | the table an object's members are stored in |
//!
//! Every part is safe Rust with no global state and no raw pointers: a
//! value owns its storage and is borrowed or cloned, never passed as a
//! handle a caller must free.
//!
//! # The order the runtime is built in
//!
//! The value layer comes first, because every other part speaks in its
//! terms: an object's members are keyed by strings, the interpreter's
//! registers hold values, and an error carries a string. The object model
//! and the variant are next, and the interpreter after them, because it
//! owns the runtime's globals and executes the object model's dispatch.
//!
//! # The measurement
//!
//! Each part of the layer is measured against the vendored primitive it
//! stands for, over the same corpus and in the same build: the `bench`
//! module (test only, and ignored) prints one line per primitive with both
//! sides' nanoseconds per operation and the ratio. It is a measurement, not
//! a test, and its documentation carries the command and the corpus.
//!
//! # The unsafe code
//!
//! This module denies `unsafe`: the value layer is ordinary safe Rust, and
//! a part that cannot be written without `unsafe` is a part whose shape is
//! wrong. The measurement is no exception: `bench` calls the C entry points
//! through the safe wrapper at the crate root, which is where the one
//! `unsafe` block and its safety note live, so this module and everything
//! under it stay free of `unsafe`.
#![deny(unsafe_code)]

pub mod array;
pub mod dictionary;
pub mod exec;
pub mod hash;
pub mod member;
pub mod natives;
pub mod number;
pub mod object;
pub mod octet;
pub mod string;
pub mod variant;

#[cfg(test)]
mod bench;
