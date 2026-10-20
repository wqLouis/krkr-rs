//! Host-resource capability checks for the platform layer.
//!
//! A game cannot start without host data the layers above L1 never see: the
//! directories a face is resolved from, the audio device, the surface. The
//! parts of that data a host can assert are asserted here, per platform. What
//! is left — a device's `/system/fonts` actually holding a face, an audio
//! device opening, MediaCodec existing — is checked by the device run
//! (`docs.old/migration/README.md` D36), not faked here.
//!
//! Deliberately **not** asserted: that a returned directory exists or holds a
//! face. A missing directory is an absent source, not an error, so only the
//! list's contents are a host fact.

use std::path::Path;

use platform::{ANDROID_SYSTEM_FONT_DIRS, system_font_dirs};

/// The platform directory Android's font scan otherwise misses: `fontdb`'s
/// Unix/X11 scan excludes Android, so this entry is the only route to the
/// platform's CJK face.
const ANDROID_PLATFORM_FONT_DIR: &str = "/system/fonts";

/// Every platform names at least one system font directory, so an empty list
/// can never silently disable system-font discovery.
#[test]
fn every_platform_names_a_system_font_directory() {
    let dirs = system_font_dirs();
    assert!(
        !dirs.is_empty(),
        "an empty font-directory list means no system face can ever resolve"
    );
}

/// Android's list contains the platform directory the text engine has no
/// other way to reach, and every entry is an absolute directory.
#[test]
fn android_names_the_platform_font_directory() {
    assert!(
        ANDROID_SYSTEM_FONT_DIRS.contains(&ANDROID_PLATFORM_FONT_DIR),
        "the Android font scan must include {ANDROID_PLATFORM_FONT_DIR}; \
         fontdb does not supply it"
    );
    for dir in ANDROID_SYSTEM_FONT_DIRS.iter().copied() {
        assert!(
            Path::new(dir).is_absolute(),
            "Android font directory {dir} is relative; the loader would \
             resolve it against the process working directory"
        );
    }
}
