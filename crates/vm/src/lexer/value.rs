//! The values a literal token carries.

/// A script string: a sequence of code units.
///
/// A script string is not a sequence of bytes and not a sequence of
/// characters: it is a sequence of code units, and its length is the number
/// of code units in it. A lone surrogate is a code unit like any other.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ScriptString {
    units: Vec<u16>,
}

impl ScriptString {
    /// An empty string.
    pub fn new() -> Self {
        Self::default()
    }

    /// A string holding the given code units.
    pub fn from_units(units: Vec<u16>) -> Self {
        Self { units }
    }

    /// Append one code unit.
    pub fn push(&mut self, unit: u16) {
        self.units.push(unit);
    }

    /// The code units of the string.
    pub fn units(&self) -> &[u16] {
        &self.units
    }

    /// The number of code units in the string.
    pub fn len(&self) -> usize {
        self.units.len()
    }

    /// Whether the string holds no code unit.
    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }
}

/// A literal value.
///
/// The kinds are the ones a literal token can carry: the `null` literal, an
/// integer, a real, a string and a binary buffer.
#[derive(Clone, Debug, PartialEq)]
pub enum Literal {
    /// The `null` literal.
    Null,
    /// An integer literal.
    Integer(i64),
    /// A real literal.
    Real(f64),
    /// A string literal.
    String(ScriptString),
    /// A binary literal.
    Octet(Vec<u8>),
}
