//! String encodings for the dictionary-shaped data the reference native
//! returns as TJS objects. The tjs2-sys C ABI can only marshal
//! void/int/real/string results, so every object-shaped result is encoded
//! as a documented string that the script-side wrapper parses back into a
//! dictionary (see [`crate`] for the exact formats).
//!
//! # The line format
//!
//! A dictionary becomes one line per entry: `key=value`, lines joined with
//! `\n`. Keys never contain a raw `=` (tag attribute names cannot contain
//! `=` by KAG syntax, and structural keys are fixed), so a decoder splits
//! on the first `=` of each line.
//!
//! Field escaping (`\` → `\\`, `\n` → `\n`, `\r` → `\r`) makes the format
//! lossless for arbitrary text. Scenario tag content never contains raw
//! newlines (the parser splits lines first), but macro bodies and saved
//! state can, so escaping is applied everywhere for uniformity.

/// Escape one field: `\` → `\\`, newline → `\n`, CR → `\r`.
pub fn escape_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

/// Undo [`escape_field`].
pub fn unescape_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                // A trailing backslash or an unknown escape is kept as-is.
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Encode an ordered dictionary as `key=value` lines. Keys and values are
/// escaped with [`escape_field`].
pub fn encode_dict(entries: &[(String, String)]) -> String {
    let mut out = String::new();
    for (i, (k, v)) in entries.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&escape_field(k));
        out.push('=');
        out.push_str(&escape_field(v));
    }
    out
}

/// Decode the output of [`encode_dict`] back into ordered entries.
pub fn decode_dict(s: &str) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    for line in s.split('\n') {
        let Some(eq) = line.find('=') else { continue };
        entries.push((unescape_field(&line[..eq]), unescape_field(&line[eq + 1..])));
    }
    entries
}

/// Encode a tag dictionary: the tag name on the first line, then
/// `key=value` lines for each attribute (in source order). This is the
/// `getNextTag` return format.
pub fn encode_tag(tagname: &str, params: &[(String, String)]) -> String {
    let mut out = escape_field(tagname);
    for (k, v) in params {
        out.push('\n');
        out.push_str(&escape_field(k));
        out.push('=');
        out.push_str(&escape_field(v));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_round_trip() {
        for s in [
            "",
            "plain",
            "back\\slash",
            "line\nbreak",
            "cr\rreturn",
            "混ざり\\\n\rテキスト",
            "\\n literal backslash-n",
        ] {
            assert_eq!(unescape_field(&escape_field(s)), s, "round trip {s:?}");
        }
    }

    #[test]
    fn escaping_is_unambiguous() {
        // A value containing backslash-n must not collide with an escaped
        // newline.
        assert_eq!(escape_field("a\nb"), "a\\nb");
        assert_eq!(escape_field("a\\nb"), "a\\\\nb");
        assert_eq!(unescape_field("a\\nb"), "a\nb");
        assert_eq!(unescape_field("a\\\\nb"), "a\\nb");
    }

    #[test]
    fn dict_round_trip() {
        let entries = vec![
            ("tagname".to_string(), "bg".to_string()),
            ("storage".to_string(), "chapter1/f01.jpg".to_string()),
            ("path".to_string(), "a=b\\c\nd".to_string()),
        ];
        let s = encode_dict(&entries);
        assert_eq!(decode_dict(&s), entries);
    }

    #[test]
    fn dict_values_keep_equals() {
        // Values may contain raw `=`; only the first `=` of a line splits.
        let entries = vec![("a".to_string(), "x=y=z".to_string())];
        let s = encode_dict(&entries);
        assert_eq!(s, "a=x=y=z");
        assert_eq!(decode_dict(&s), entries);
    }

    #[test]
    fn tag_encoding() {
        let s = encode_tag("bg", &[("storage".into(), "a.jpg".into())]);
        assert_eq!(s, "bg\nstorage=a.jpg");

        // no params → tagname only
        assert_eq!(encode_tag("wait", &[]), "wait");

        // escaped tagname/values
        let s = encode_tag("a\\b", &[("k".into(), "v\nw".into())]);
        assert_eq!(s, "a\\\\b\nk=v\\nw");
    }

    #[test]
    fn empty_dict() {
        assert_eq!(encode_dict(&[]), "");
        assert_eq!(decode_dict(""), Vec::<(String, String)>::new());
    }
}
