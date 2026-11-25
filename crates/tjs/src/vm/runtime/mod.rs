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
//! # The unsafe code
//!
//! This module denies `unsafe`: the value layer is ordinary safe Rust, and
//! a part that cannot be written without `unsafe` is a part whose shape is
//! wrong.
#![deny(unsafe_code)]

pub mod hash;
pub mod number;
pub mod string;
