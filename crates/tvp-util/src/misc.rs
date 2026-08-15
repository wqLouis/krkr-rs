//! String and path helpers ported from the reference's utils module:
//!
//! - `utils/StringUtil.h` — `Trim`, `icomp`;
//! - `utils/FilePathUtil.h` — `IncludeTrailingBackslash`,
//!   `ExcludeTrailingBackslash`, `ExtractFileDir`, `ExtractFileName`;
//! - `environ/Application.cpp` — `ExtractFileDir` (via `av_dirname`);
//! - TJS `ttstr::Replace` — the `ReplaceStringAll` helper games use.
//!
//! Path helpers treat both `/` and `\` as separators (the reference engine
//! on Android uses `/` but storage names may contain `\`); directory
//! results are normalized to `/`. This is slightly more lenient than the
//! reference's `av_dirname`, which only splits on `/`.

/// `ReplaceStringAll`: replaces every (non-overlapping) occurrence of
/// `from` in `s` with `to` — the same semantics as the TJS `ttstr::Replace`
/// the reference implements it with (`str::replace`).
///
/// An empty `from` inserts `to` at every character boundary (including the
/// ends), matching `str::replace`'s documented behavior.
pub fn replace_all(s: &str, from: &str, to: &str) -> String {
    s.replace(from, to)
}

/// Characters trimmed by the reference `Trim` (`StringUtil.h`): space plus
/// all C0 controls and DEL (`\x01`..`\x1F`, `\x7F`).
const TRIM_CHARS: &str = " \x01\x02\x03\x04\x05\x06\x07\x08\t\n\x0b\x0c\r\x0e\x0f\x10\x11\x12\
\x13\x14\x15\x16\x17\x18\x19\x1a\x1b\x1c\x1d\x1e\x1f\x7f";

/// Trims the `TRIM_CHARS` set from both ends.
///
/// Note: the C++ `Trim` has an off-by-one quirk — a string consisting only
/// of trim characters is returned unchanged (both `find_first_not_of` and
/// `find_last_not_of` return `npos`, and `pos == lastpos` matches). We
/// implement the intended behavior and return an empty string instead.
pub fn trim(s: &str) -> &str {
    s.trim_matches(|c: char| TRIM_CHARS.contains(c))
}

/// ASCII case-insensitive string equality — a port of `icomp`
/// (`StringUtil.h`, which compares with `std::tolower` in the C locale).
///
/// Non-ASCII characters compare byte-wise (case folding is not applied),
/// exactly like the reference's locale-based comparison does for `char`.
pub fn ieq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// `IncludeTrailingBackslash`: appends a trailing `/` if `path` does not
/// already end with `/` or `\`; an empty path becomes `"/"`.
pub fn include_trailing_slash(path: &str) -> String {
    if path.is_empty() {
        return "/".to_string();
    }
    if path.ends_with(['/', '\\']) {
        path.to_string()
    } else {
        format!("{path}/")
    }
}

/// `ExcludeTrailingBackslash`: strips **one** trailing `\`.
///
/// Faithful quirk kept from the reference: only `\` is stripped, not `/`.
/// Use [`trim_end_matches`](str::trim_end_matches) yourself if you want
/// both. (The C++ reads `path[path.len()-1]` on an empty string, which is
/// UB; we simply return the input unchanged.)
pub fn exclude_trailing_slash(path: &str) -> &str {
    path.strip_suffix('\\').unwrap_or(path)
}

/// `ExtractFileDir`: the directory part of `path`, with `\` normalized to
/// `/` in the result. Mirrors the reference `av_dirname` semantics:
///
/// - trailing separators are stripped first;
/// - no separator left → `"."` (empty input → `"."`, `"/"`-only → `"/"`);
/// - the separator itself is included for root paths (`"/a"` → `"/"`).
pub fn extract_file_dir(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let trimmed = path.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        // "/" or "\" or any mix — the root.
        return "/".to_string();
    }
    match trimmed.rfind(['/', '\\']) {
        None => ".".to_string(),
        Some(0) => "/".to_string(),
        Some(i) => trimmed[..i].replace('\\', "/"),
    }
}

/// `ExtractFileName`: the final path component (after the last `/` or
/// `\`), with trailing separators stripped. `""` stays `""`; a path made
/// only of separators yields `"/"`.
pub fn extract_file_name(path: &str) -> &str {
    if path.is_empty() {
        return "";
    }
    let trimmed = path.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return "/";
    }
    match trimmed.rfind(['/', '\\']) {
        None => trimmed,
        Some(i) => &trimmed[i + 1..],
    }
}

/// `ExtractFileExt`: the extension of the final component, **including the
/// dot** (Delphi convention), or `""` when there is none. A leading dot
/// does not count (`".bashrc"` → `""`); `"a."` → `"."`.
pub fn extract_file_ext(path: &str) -> &str {
    let name = extract_file_name(path);
    match name.rfind('.') {
        Some(0) => "",
        Some(i) => &name[i..],
        None => "",
    }
}

/// `ChangeFileExt`: replaces the extension of the final component with
/// `new_ext` (which should include the dot, e.g. `".tjs"`). Files with no
/// extension (or only a leading-dot name) get `new_ext` appended.
pub fn change_file_ext(path: &str, new_ext: &str) -> String {
    let name = extract_file_name(path);
    let stem = match name.rfind('.') {
        Some(0) => name,
        Some(i) => &name[..i],
        None => name,
    };
    let dir = extract_file_dir(path);
    if dir == "." {
        format!("{stem}{new_ext}")
    } else {
        format!("{dir}/{stem}{new_ext}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_all_basic() {
        assert_eq!(
            replace_all("a-b-c-d", "-", "+"),
            "a+b+c+d",
            "all occurrences are replaced"
        );
        assert_eq!(replace_all("hello", "l", "L"), "heLLo");
        assert_eq!(replace_all("no match", "x", "y"), "no match");
        assert_eq!(
            replace_all("aaa", "aa", "b"),
            "ba",
            "non-overlapping left-to-right"
        );
    }

    #[test]
    fn replace_all_japanese() {
        assert_eq!(replace_all("はろーはろー", "はろー", "やあ"), "やあやあ");
    }

    #[test]
    fn trim_control_chars() {
        assert_eq!(trim("  hello  "), "hello");
        assert_eq!(trim("\t\n  spaced\t\n"), "spaced");
        // Control chars 0x01..0x1F and DEL are trimmed, matching the C++ set.
        assert_eq!(trim("\x01\x02\x03value\x1f\x7f"), "value");
        assert_eq!(trim("   "), "");
        assert_eq!(trim(""), "");
        // The C++ Trim returns the input unchanged for all-trim strings
        // (a quirk); we trim to empty instead — see doc comment.
        assert_eq!(trim("\t\t"), "");
    }

    #[test]
    fn ieq_ascii_case_insensitive() {
        assert!(ieq("Startup.tjs", "startup.tjs"));
        assert!(ieq("ABC", "abc"));
        assert!(!ieq("abc", "abd"));
        assert!(!ieq("abc", "abc d"));
        // Non-ASCII is compared byte-wise, like the reference's C-locale
        // std::tolower: identical bytes pass, but there is no Unicode case
        // folding.
        assert!(ieq("日本語", "日本語"));
        assert!(!ieq("Ω", "ω"));
    }

    #[test]
    fn include_exclude_trailing_slash() {
        assert_eq!(include_trailing_slash("game"), "game/");
        assert_eq!(include_trailing_slash("game/"), "game/");
        assert_eq!(
            include_trailing_slash("game\\"),
            "game\\",
            "already has a separator"
        );
        assert_eq!(include_trailing_slash(""), "/");

        assert_eq!(exclude_trailing_slash("game\\"), "game");
        // Faithful quirk: only backslash is stripped, not '/'.
        assert_eq!(exclude_trailing_slash("game/"), "game/");
        assert_eq!(exclude_trailing_slash(""), "");
    }

    #[test]
    fn extract_file_dir_cases() {
        assert_eq!(extract_file_dir("a/b/c.tjs"), "a/b");
        assert_eq!(
            extract_file_dir("a/b/"),
            "a",
            "trailing slash is stripped first"
        );
        assert_eq!(extract_file_dir("a"), ".");
        assert_eq!(extract_file_dir(""), ".");
        assert_eq!(extract_file_dir("/"), "/");
        assert_eq!(extract_file_dir("/a"), "/");
        assert_eq!(extract_file_dir("/a/b"), "/a");
        assert_eq!(
            extract_file_dir("a\\b\\c"),
            "a/b",
            "backslashes normalize to '/'"
        );
        assert_eq!(extract_file_dir("C:\\game\\arc.xp3"), "C:/game");
    }

    #[test]
    fn extract_file_name_cases() {
        assert_eq!(extract_file_name("a/b/c.tjs"), "c.tjs");
        assert_eq!(extract_file_name("a/b/"), "b");
        assert_eq!(extract_file_name("a"), "a");
        assert_eq!(extract_file_name(""), "");
        assert_eq!(extract_file_name("/"), "/");
        assert_eq!(extract_file_name("C:\\game\\arc.xp3"), "arc.xp3");
    }

    #[test]
    fn extract_file_ext_cases() {
        assert_eq!(extract_file_ext("a/b/startup.tjs"), ".tjs");
        assert_eq!(extract_file_ext("archive.xp3"), ".xp3");
        assert_eq!(extract_file_ext("noext"), "");
        assert_eq!(extract_file_ext("a.b.c"), ".c");
        assert_eq!(
            extract_file_ext(".hidden"),
            "",
            "leading dot is not an extension"
        );
        assert_eq!(extract_file_ext("trailing."), ".");
        assert_eq!(
            extract_file_ext("dir/"),
            "",
            "directories have no extension"
        );
    }

    #[test]
    fn change_file_ext_cases() {
        assert_eq!(change_file_ext("a/b/startup.tjs", ".ks"), "a/b/startup.ks");
        assert_eq!(change_file_ext("startup.tjs", ".ks"), "startup.ks");
        assert_eq!(change_file_ext("noext", ".tjs"), "noext.tjs");
        assert_eq!(change_file_ext("a.b.c", ".d"), "a.b.d");
        assert_eq!(change_file_ext("arc.xp3", ".zip"), "arc.zip");
        assert_eq!(change_file_ext(".hidden", ".txt"), ".hidden.txt");
    }

    #[test]
    fn path_helpers_agree_on_trailing_separator() {
        // include + extract compose like the reference's usage in
        // SysInitImpl.cpp (ExePath() + IncludeTrailingBackslash(...)).
        let dir = extract_file_dir("/data/game/arc.xp3");
        assert_eq!(include_trailing_slash(&dir), "/data/game/");
    }
}
