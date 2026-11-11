//! The values a literal token carries.
//!
//! The analyser keeps the values it produces in one place: the string data
//! in one buffer, the binary data in another, and the numbers beside them.
//! A token names its value by its index, so a token costs eight bytes and
//! producing one costs no allocation.

/// A value the analyser keeps.
///
/// The string and the binary data are offsets into the analyser's buffers,
/// so the buffers hold the data once.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Value {
    /// The null object.
    Null,
    /// An integer.
    Integer(i64),
    /// A real.
    Real(f64),
    /// A string: the offset and the length in the string buffer.
    String { offset: u32, len: u32 },
    /// A binary buffer: the offset and the length in the byte buffer.
    Octet { offset: u32, len: u32 },
}

impl Value {
    /// The value with its sign flipped.
    pub fn negated(self) -> Self {
        match self {
            Self::Integer(value) => Self::Integer(value.wrapping_neg()),
            Self::Real(value) => Self::Real(-value),
            other => other,
        }
    }
}

/// A value the analyser produced, borrowed from its buffers.
///
/// A script string is not a sequence of bytes and not a sequence of
/// characters: it is a sequence of code units, and its length is the number
/// of code units in it. A lone surrogate is a code unit like any other.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ValueRef<'a> {
    /// The null object.
    Null,
    /// An integer.
    Integer(i64),
    /// A real.
    Real(f64),
    /// A string: the code units.
    String(&'a [u16]),
    /// A binary buffer: the bytes.
    Octet(&'a [u8]),
}
