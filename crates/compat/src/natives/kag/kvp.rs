//! String encodings for the dictionary-shaped data the parser keeps as
//! strings: the `store`/`restore` save container and the macro bodies. The
//! native's object-shaped results (`getNextTag`, `macros`, `macroParams`) are
//! real TJS Dictionaries, not strings, so this codec is no longer on that path.
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

#![allow(dead_code)]

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
