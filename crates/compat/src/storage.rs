//! The script-name → VFS-name boundary for storage-backed natives.
//!
//! L3 owns the TVP storage-name grammar; the L2 `engine_api::Storage` namespace
//! takes a bare name within a mount. A script name may carry the local `file`
//! media prefix (`file://.`, `file://` or `file:`) which the namespace does not
//! understand, so every native that hands a script-supplied name to L2 routes
//! it through [`strip_file_media`] first.
//!
//! A name that carries **another** scheme (`var://…`, `psb://…`) crosses the
//! boundary unchanged: the L2 namespace dispatches a registered scheme to the
//! provider that serves it, and the provider owns the scheme's own path
//! grammar.

/// Strips the local `file` media prefix from a storage name, returning the
/// bare VFS name the L2 `engine_api::Storage` namespace accepts.
///
/// The prefixes are tried longest first so `file://.` wins over `file://` and
/// `file:`. A name without a prefix is returned unchanged.
pub(crate) fn strip_file_media(name: &str) -> &str {
    for prefix in ["file://.", "file://", "file:"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            return rest;
        }
    }
    name
}

#[cfg(test)]
mod tests {
    use super::strip_file_media;

    #[test]
    fn strips_the_local_file_media_prefix() {
        // The reference parses `media://domain/path` and hands the path to the
        // media (`reference/cpp/core/base/StorageIntf.cpp:240-430`); the port
        // accepts the `file://.` application form and the shorter `file://` /
        // `file:` spellings.
        assert_eq!(strip_file_media("file://./tmp/a.tjs"), "/tmp/a.tjs");
        assert_eq!(strip_file_media("file:///tmp/a.tjs"), "/tmp/a.tjs");
        assert_eq!(strip_file_media("file:a.tjs"), "a.tjs");
        assert_eq!(strip_file_media("data/a.tjs"), "data/a.tjs");
    }

    #[test]
    fn another_media_scheme_crosses_unchanged() {
        // The `var://` and `psb://` media are served by providers the
        // namespace knows by their scheme
        // (`reference/cpp/core/base/StorageIntf.cpp:240-439`); the name
        // reaches them with its scheme intact.
        assert_eq!(
            strip_file_media("var://./packImageMap/ev121a.pimg/root/3797.tlg"),
            "var://./packImageMap/ev121a.pimg/root/3797.tlg"
        );
        assert_eq!(
            strip_file_media("psb://quickmenu.pimg/11632.tlg"),
            "psb://quickmenu.pimg/11632.tlg"
        );
    }
}
