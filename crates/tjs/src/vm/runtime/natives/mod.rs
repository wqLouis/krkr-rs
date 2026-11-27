//! The built-in classes' rules.
//!
//! Each module here is a table of what the class publishes and the function
//! behind each entry, not a class object: the object itself — its statics, its
//! construction, its class name — belongs to the layer that registers classes,
//! which is [`registry`]. What is here is the part a script's answer depends
//! on, and every answer is recorded from the engine before it is written.
//!
//! [`string`] is the same table for the two values that are not objects: the
//! engine registers no class for a text or a byte string, and the two of them
//! answer a call from their own surface.

pub mod config;
pub mod date;
pub mod date_parse;
pub mod exception;
pub mod math;
pub mod random;
pub mod regexp;
pub mod registry;
pub mod string;
