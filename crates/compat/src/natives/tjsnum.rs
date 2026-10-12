//! The TJS `string -> integer` coercion.
//!
//! `TJSStringToInteger` (`tjs2/tjsVariant.cpp:301-307`) parses the leading
//! number token with the lexer's grammar (`tjs2/tjsLex.cpp`), ignoring trailing
//! garbage. The KAG attribute walk (`natives::kag::state`) includes this module
//! directly, so it stays free of imports beyond `std`; the real-number
//! counterpart lives in `natives::system`.
//!
//! [`parse_radix`] is shared with that real parser.

/// The reference `TJSStringToInteger` (`tjs2/tjsVariant.cpp:301-307`): parse
/// the leading number token of `str` with the lexer's number grammar
/// (`tjs2/tjsLex.cpp:611-743`) and integer-ise it — an optional sign (plus
/// following spaces), the `true`/`false` keywords, `0x`/`0b`/leading-zero
/// octal forms, and decimal integer/real literals (including `e` exponents).
/// Anything unparsable yields `0`; trailing garbage (`"12abc"`) is ignored.
pub(crate) fn tjs_string_to_integer(s: &str) -> i64 {
    let bytes = s.as_bytes();
    let mut pos = 0usize;
    let mut negative = false;
    if pos < bytes.len() && (bytes[pos] == b'+' || bytes[pos] == b'-') {
        negative = bytes[pos] == b'-';
        pos += 1;
        // The lexer skips spaces after the sign (`TJSSkipSpace`,
        // `tjsLex.cpp:720-730`).
        while pos < bytes.len() && bytes[pos] == b' ' {
            pos += 1;
        }
    }
    let rest = &s[pos..];
    let apply_sign = |value: i64| {
        if negative {
            value.wrapping_neg()
        } else {
            value
        }
    };
    // Keyword forms (`tjsLex.cpp:614-634`), matched like `TJSStringMatch`
    // (case-sensitive, and not followed by an alphabetic character or `_`).
    let word_follows = |after: &str| {
        after
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
    };
    for (keyword, value) in [
        ("true", 1i64),
        ("false", 0i64),
        // `NaN`/`Infinity` are real literals; the reference's
        // `(tTVInteger)` cast of them is platform-dependent and yields the
        // integer-indefinite value on its primary (x86) target.
        ("NaN", i64::MIN),
        ("Infinity", i64::MIN),
    ] {
        if let Some(after) = rest.strip_prefix(keyword)
            && !word_follows(after)
        {
            return apply_sign(value);
        }
    }
    let digits = rest.as_bytes();
    // `0x`/`0X` hexadecimal (`tjsLex.cpp:626-630`).
    if digits.len() >= 2 && digits[0] == b'0' && (digits[1] == b'x' || digits[1] == b'X') {
        return apply_sign(parse_radix(&rest[2..], 16));
    }
    // `0b`/`0B` binary (`tjsLex.cpp:632-636`).
    if digits.len() >= 2 && digits[0] == b'0' && (digits[1] == b'b' || digits[1] == b'B') {
        return apply_sign(parse_radix(&rest[2..], 2));
    }
    // A leading zero that is neither `0x`, `0b`, `0.` nor `0e` is octal
    // (`tjsLex.cpp:638-655`).
    if digits.first() == Some(&b'0') && digits.len() > 1 {
        match digits[1] {
            b'.' | b'e' | b'E' => {}
            b'p' | b'P' => return 0,
            _ => return apply_sign(parse_radix(rest, 8)),
        }
    }
    // Decimal integer or real (`tjsLex.cpp:686-698`).
    apply_sign(parse_decimal(rest))
}

/// Parse leading digits of `s` in `radix` (reference
/// `TJSParseNonDecimalNumber`), accumulating with the reference's wrapping
/// 64-bit arithmetic (`tjsLex.cpp:288-310`); `0` when no digit parses.
pub(crate) fn parse_radix(s: &str, radix: u32) -> i64 {
    let mut value: i64 = 0;
    let mut any = false;
    for c in s.chars() {
        let Some(digit) = c.to_digit(radix) else {
            break;
        };
        any = true;
        value = value
            .wrapping_mul(i64::from(radix))
            .wrapping_add(i64::from(digit));
    }
    if any { value } else { 0 }
}

/// Parse a leading decimal number token (`TJSExtractNumber` +
/// `TJSParseDecimalInteger`/`TJSParseDecimalReal`, `tjsLex.cpp:382-429` and
/// `447-470`): digits with at most one `.` and at most one `e`/`E` exponent
/// (optional spaces and sign after the exponent marker). A token with a
/// point or exponent is real and truncates towards zero via `as i64`.
fn parse_decimal(s: &str) -> i64 {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    let mut seen_point = false;
    let mut seen_exp = false;
    let mut end = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_digit() {
            i += 1;
            end = i;
        } else if c == b'.' && !seen_point && !seen_exp {
            seen_point = true;
            i += 1;
        } else if (c == b'e' || c == b'E') && !seen_exp {
            seen_exp = true;
            i += 1;
            while i < bytes.len() && bytes[i] == b' ' {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
                i += 1;
            }
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
                end = i;
            }
        } else {
            break;
        }
    }
    if end == 0 {
        return 0;
    }
    let token = &s[..end];
    if seen_point || seen_exp {
        // Re-slicing to `end` keeps a valid `f64` literal; a dangling `e`
        // exponent marker without digits (`"1e"`) falls back to the mantissa
        // prefix, matching `strtod`'s behaviour in
        // `TJSParseDecimalReal` (`tjsLex.cpp:472-509`).
        token
            .parse::<f64>()
            .or_else(|_| match token.find(['e', 'E']) {
                Some(exp) if exp > 0 => token[..exp].parse::<f64>().map_err(|_| ()),
                _ => Err(()),
            })
            .map_or(0, |real| real as i64)
    } else {
        token
            .bytes()
            .filter(u8::is_ascii_digit)
            .fold(0i64, |value, digit| {
                value.wrapping_mul(10).wrapping_add(i64::from(digit - b'0'))
            })
    }
}
