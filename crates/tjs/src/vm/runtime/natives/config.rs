//! The `Config` class's members: there are none.
//!
//! The engine's global object answers `undefined` for `Config`, and the file
//! the class was expected to come from holds text and number helpers, not a
//! native class. A game's `Config` is a dictionary its own script creates;
//! the settings the compatibility layer owns are members of `System`. Neither
//! is a member of this table.
//!
//! [`ConfigMember::ALL`] is empty. The names a script may expect are listed by
//! [`EXPECTED_NAMES`], and [`ConfigMember::lookup`] answers `None` for every
//! one; the check beside this module pins that emptiness against the engine's
//! recorded answer.

use crate::vm::runtime::string::TjsString;

/// The names a script may expect to find on a `Config` object. None of them is
/// a native member; the list exists so the absence is checked rather than
/// assumed.
pub const EXPECTED_NAMES: &[&str] = &[
    "defaultFont",
    "fullScreen",
    "graphicCacheLimit",
    "screenHeight",
    "screenWidth",
    "throttle",
    "title",
    "windowHeight",
    "windowWidth",
];

/// A member of the `Config` class.
///
/// The class publishes no member, so [`ConfigMember::ALL`] is empty. The shape
/// is the one the other native tables use, and it keeps a future member from
/// being added without a recorded answer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigMember {
    /// The name the class publishes the member under.
    name: &'static str,
}

impl ConfigMember {
    /// Every member, in the order the class registers them: none.
    pub const ALL: &'static [ConfigMember] = &[];

    /// The member `name` denotes, or `None` when it names no member.
    pub fn lookup(name: &TjsString) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|member| name.len() == member.name.len() && name.eq_str(member.name))
    }

    /// The name the class publishes the member under.
    pub fn name(self) -> &'static str {
        self.name
    }
}

#[cfg(test)]
mod tests {
    use super::{ConfigMember, EXPECTED_NAMES};
    use crate::vm::runtime::string::TjsString;

    // reference/krkr2/cpp/core/tjs2/tjs.cpp:139-191
    // the global object registers Array, Dictionary, Date, Math, RandomGenerator,
    // Exception and RegExp; no Config class exists.
    #[test]
    fn the_class_publishes_no_member() {
        assert!(ConfigMember::ALL.is_empty());
        for name in EXPECTED_NAMES {
            assert_eq!(ConfigMember::lookup(&TjsString::from(*name)), None);
        }
    }
}
