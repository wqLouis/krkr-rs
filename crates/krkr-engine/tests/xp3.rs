//! Integration tests for XP3 archive name normalization.
//!
//! Relocated from the inline `#[cfg(test)]` module in `src/archive/xp3.rs`.

use krkr_engine::normalize_in_archive_name;

#[test]
fn normalize_names() {
    assert_eq!(normalize_in_archive_name("Startup.tjs"), "startup.tjs");
    assert_eq!(normalize_in_archive_name(r"data\bg\a.jpg"), "data/bg/a.jpg");
    assert_eq!(
        normalize_in_archive_name("data//bg///a.jpg"),
        "data/bg/a.jpg"
    );
    assert_eq!(
        normalize_in_archive_name("//leading/slash"),
        "leading/slash"
    );
    assert_eq!(normalize_in_archive_name("/leading"), "leading");
    assert_eq!(normalize_in_archive_name("/"), "");
    assert_eq!(normalize_in_archive_name("a//b"), "a/b");
}
