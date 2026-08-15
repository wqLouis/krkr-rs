//! MD5 message digest (RFC 1321).
//!
//! The reference `utils/md5.c` is an independent, straight RFC 1321
//! implementation (L. Peter Deutsch's `md5` library) with no custom quirks:
//! no input re-encoding, standard little-endian byte ordering, standard
//! padding and length appending. The `md-5` crate is therefore a drop-in
//! equivalent, and the reference's `md5_init`/`md5_append`/`md5_finish`
//! stateful API is mirrored here by [`Md5`].
//!
//! MD5 is used by the TVP random generator ([`crate::random`]) and by the
//! engine for things such as storage key digests. It is **not** a
//! cryptographically secure hash; do not use it where collision resistance
//! is required.

use std::fmt::Write as _;

use md5::{Digest, Md5 as Md5Core};

/// One-shot MD5 digest of `data` as a 16-byte array.
pub fn digest(data: &[u8]) -> [u8; 16] {
    let mut out = [0u8; 16];
    out.copy_from_slice(&Md5Core::digest(data));
    out
}

/// One-shot MD5 digest of `data` as a lowercase hex string (32 chars).
pub fn hex(data: &[u8]) -> String {
    let mut out = String::with_capacity(32);
    for b in digest(data) {
        write!(out, "{b:02x}").expect("writing to a String cannot fail");
    }
    out
}

/// Incremental MD5 hasher, mirroring the reference's stateful
/// `md5_init`/`md5_append`/`md5_finish` API.
///
/// ```
/// use tvp_util::md5::{hex, Md5};
///
/// let mut h = Md5::new();
/// h.update(b"Hello ");
/// h.update(b"world!");
/// assert_eq!(hex(&h.finalize()), "d522231cf8bd5bd3bceb31c640bcab9a");
/// ```
#[derive(Clone, Default)]
pub struct Md5 {
    inner: Md5Core,
}

impl Md5 {
    /// Creates a new hasher with the RFC 1321 initial state
    /// (equivalent to `md5_init`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds `data` into the digest (equivalent to `md5_append`).
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// Finishes the digest and returns the 16-byte result
    /// (equivalent to `md5_finish`).
    pub fn finalize(self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out.copy_from_slice(&self.inner.finalize());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The RFC 1321 A.5 test suite.
    const RFC1321_CASES: &[(&str, &str)] = &[
        ("", "d41d8cd98f00b204e9800998ecf8427e"),
        ("a", "0cc175b9c0f1b6a831c399e269772661"),
        ("abc", "900150983cd24fb0d6963f7d28e17f72"),
        ("message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
        (
            "abcdefghijklmnopqrstuvwxyz",
            "c3fcd3d76192e4007dfb496cca67e13b",
        ),
        (
            "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
            "d174ab98d277d9f5a5611c2c9f419d9f",
        ),
        (
            "12345678901234567890123456789012345678901234567890123456789012345678901234567890",
            "57edf4a22be3c955ac49da2e2107b67a",
        ),
    ];

    #[test]
    fn rfc1321_known_answer_tests() {
        for (input, expected) in RFC1321_CASES {
            assert_eq!(hex(input.as_bytes()), *expected, "md5({input:?})");
        }
    }

    #[test]
    fn million_a() {
        // RFC 1321 A.5 "one million 'a'" test — the classic length-padding
        // stress case.
        let data = vec![b'a'; 1_000_000];
        assert_eq!(hex(&data), "7707d6ae4e027c70eea2a935c2296f21");
    }

    #[test]
    fn incremental_matches_oneshot() {
        let mut h = Md5::new();
        for chunk in b"The quick brown fox jumps over the lazy dog".chunks(3) {
            h.update(chunk);
        }
        assert_eq!(
            h.finalize(),
            digest(b"The quick brown fox jumps over the lazy dog")
        );
        assert_eq!(
            hex(b"The quick brown fox jumps over the lazy dog"),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
    }

    #[test]
    fn non_ascii_bytes() {
        // md5 operates on raw bytes — no text re-encoding, matching md5.c
        // (value verified against an independent implementation).
        assert_eq!(
            hex(&[0xff, 0xfe, 0x00, 0x01]),
            "8ac6321bafac4886617ba38078a7819d"
        );
    }
}
