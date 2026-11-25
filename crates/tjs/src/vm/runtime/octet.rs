//! The octet: the language's raw byte value.
//!
//! An octet is an immutable sequence of bytes held by a shared handle, so a
//! clone never copies the bytes. It is built from one span of bytes or from
//! two, it is compared by its bytes, and it is written as a list of
//! hexadecimal byte pairs — the one form the language gives it. The language
//! itself converts an octet neither to a text nor to a number, so the variant
//! refuses both conversions and this list form belongs to messages.

use std::cmp::Ordering;
use std::fmt;
use std::rc::Rc;

/// A script octet: an immutable sequence of bytes.
#[derive(Clone)]
pub struct Octet {
    /// The bytes, shared between the values that hold them.
    bytes: Rc<[u8]>,
}

impl Octet {
    /// The empty octet.
    pub fn new() -> Self {
        Self {
            bytes: Rc::from(Vec::new()),
        }
    }

    /// The octet of one span of bytes.
    pub fn from_slice(bytes: &[u8]) -> Self {
        Self {
            bytes: Rc::from(bytes),
        }
    }

    /// The octet of two spans of bytes, the second after the first.
    pub fn from_two(first: &[u8], second: &[u8]) -> Self {
        if first.is_empty() {
            return Self::from_slice(second);
        }
        if second.is_empty() {
            return Self::from_slice(first);
        }
        let mut bytes = Vec::with_capacity(first.len() + second.len());
        bytes.extend_from_slice(first);
        bytes.extend_from_slice(second);
        Self {
            bytes: Rc::from(bytes),
        }
    }

    /// The octet of the bytes a vector owns.
    pub fn from_vec(bytes: Vec<u8>) -> Self {
        Self {
            bytes: Rc::from(bytes),
        }
    }

    /// The bytes.
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    /// The number of bytes.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether the octet holds no byte.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl Default for Octet {
    fn default() -> Self {
        Self::new()
    }
}

impl From<&[u8]> for Octet {
    fn from(bytes: &[u8]) -> Self {
        Self::from_slice(bytes)
    }
}

impl From<Vec<u8>> for Octet {
    fn from(bytes: Vec<u8>) -> Self {
        Self::from_vec(bytes)
    }
}

impl PartialEq for Octet {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl Eq for Octet {}

impl PartialOrd for Octet {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Octet {
    fn cmp(&self, other: &Self) -> Ordering {
        self.bytes.cmp(&other.bytes)
    }
}

impl fmt::Display for Octet {
    /// Writes the list form: each byte as two upper-case hexadecimal digits,
    /// separated by one space, and nothing for an empty octet.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        const DIGITS: [u8; 16] = *b"0123456789ABCDEF";
        let mut text = String::with_capacity(self.bytes.len() * 3);
        for (index, byte) in self.bytes.iter().enumerate() {
            if index != 0 {
                text.push(' ');
            }
            text.push(char::from(DIGITS[usize::from(byte >> 4)]));
            text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
        }
        formatter.write_str(&text)
    }
}

impl fmt::Debug for Octet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Octet({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::Octet;

    /// An octet holds the bytes it was built from, and its length counts
    /// them.
    ///
    /// reference/cpp/core/tjs2/tjsVariant.cpp:23-31
    #[test]
    fn an_octet_holds_its_bytes() {
        let octet = Octet::from_slice(&[0x01, 0x02, 0xff]);
        assert_eq!(octet.len(), 3);
        assert!(!octet.is_empty());
        assert_eq!(octet.as_slice(), &[0x01, 0x02, 0xff]);
        assert!(Octet::new().is_empty());
        assert_eq!(Octet::default().len(), 0);
        assert_eq!(Octet::from(vec![0x10u8]).as_slice(), &[0x10]);
    }

    /// An octet is built from two spans, the second after the first.
    ///
    /// reference/cpp/core/tjs2/tjsVariant.cpp:32-42
    /// reference/cpp/core/tjs2/tjsVariant.cpp:77-92
    #[test]
    fn an_octet_is_built_from_two_spans() {
        let joined = Octet::from_two(&[0x01], &[0x02, 0x03]);
        assert_eq!(joined.as_slice(), &[0x01, 0x02, 0x03]);
        assert_eq!(Octet::from_two(&[], &[0x02]).as_slice(), &[0x02]);
        assert_eq!(Octet::from_two(&[0x01], &[]).as_slice(), &[0x01]);
        assert!(Octet::from_two(&[], &[]).is_empty());
    }

    /// Two octets are equal when their bytes are, and they order by their
    /// bytes.
    ///
    /// reference/cpp/core/tjs2/tjsVariant.cpp:700-716
    #[test]
    fn an_octet_compares_by_its_bytes() {
        assert_eq!(Octet::from_slice(&[1, 2]), Octet::from_slice(&[1, 2]));
        assert_ne!(Octet::from_slice(&[1, 2]), Octet::from_slice(&[1, 3]));
        assert_ne!(Octet::from_slice(&[1, 2]), Octet::from_slice(&[1, 2, 0]));
        assert!(Octet::from_slice(&[1, 2]) < Octet::from_slice(&[1, 3]));
        assert!(Octet::from_slice(&[1]) < Octet::from_slice(&[1, 0]));
        assert!(Octet::from_slice(&[0xff]) > Octet::from_slice(&[0x01]));
    }

    /// The text form is the list of upper-case hexadecimal pairs, one space
    /// between them and none at either end.
    ///
    /// reference/cpp/core/tjs2/tjsVariant.cpp:114-140
    #[test]
    fn the_text_form_is_the_list_of_hexadecimal_pairs() {
        assert_eq!(Octet::from_slice(&[0x01]).to_string(), "01");
        assert_eq!(Octet::from_slice(&[0x01, 0x02]).to_string(), "01 02");
        assert_eq!(
            Octet::from_slice(&[0x00, 0xab, 0xff]).to_string(),
            "00 AB FF"
        );
        assert_eq!(Octet::new().to_string(), "");
        assert_eq!(format!("{:?}", Octet::from_slice(&[0x0f])), "Octet(0F)");
    }
}
