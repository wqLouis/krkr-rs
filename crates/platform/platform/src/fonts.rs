//! Host system-font directories.
//!
//! The engine (L2) may not name a platform path, so the composition root
//! supplies this list to the text engine. [`system_font_dirs`] returns the
//! directories the host installs fonts into.
//!
//! `fontdb`'s own `load_system_fonts` already knows the Windows, macOS and
//! Unix/X11 locations, but it deliberately excludes Android from its Unix
//! scan, so on Android this list is the only source of system faces.

use std::path::PathBuf;

/// Directories Android mounts its system fonts into.
///
/// `/system/fonts` holds the platform's `NotoSansCJK-Regular.ttc`; the
/// `product` and `vendor` partitions carry OEM/vendor aliases on some
/// devices. The list is target-independent so it is testable on a desktop
/// host.
pub const ANDROID_SYSTEM_FONT_DIRS: &[&str] = &["/system/fonts", "/product/fonts", "/vendor/fonts"];

/// System-wide font directories on a Unix-like desktop.
const UNIX_SYSTEM_FONT_DIRS: &[&str] = &["/usr/share/fonts", "/usr/local/share/fonts"];

/// Returns the host's system font directories.
///
/// On Android this is [`ANDROID_SYSTEM_FONT_DIRS`]. On a Unix-like desktop it
/// is the system directories plus the per-user XDG locations
/// (`$XDG_DATA_HOME/fonts`, falling back to `$HOME/.local/share/fonts`, and
/// the legacy `$HOME/.fonts`).
///
/// A returned directory may not exist; the caller treats a missing entry as
/// an absent source rather than an error.
pub fn system_font_dirs() -> Vec<PathBuf> {
    if cfg!(target_os = "android") {
        return ANDROID_SYSTEM_FONT_DIRS.iter().map(PathBuf::from).collect();
    }
    let mut dirs: Vec<PathBuf> = UNIX_SYSTEM_FONT_DIRS.iter().map(PathBuf::from).collect();
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")));
    if let Some(data_home) = data_home {
        dirs.push(data_home.join("fonts"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".fonts"));
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Android list must name the platform directory the text engine has
    /// no other way to reach; `fontdb`'s own Unix scan excludes Android.
    #[test]
    fn android_font_dirs_include_system_fonts() {
        assert!(!ANDROID_SYSTEM_FONT_DIRS.is_empty());
        assert!(ANDROID_SYSTEM_FONT_DIRS.contains(&"/system/fonts"));
    }

    /// The host always names at least one directory, so an empty list can
    /// never silently disable system-font discovery.
    #[test]
    fn system_font_dirs_are_not_empty() {
        assert!(!system_font_dirs().is_empty());
    }
}
