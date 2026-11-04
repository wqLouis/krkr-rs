//! The frame dump: the presented frame written to a file.
//!
//! The dump is the instrument a frame can be looked at with: it writes the
//! frame the CPU rasterizer composited, so the picture the window shows can
//! be read back without a display.

use std::path::PathBuf;

use api::config::RuntimeConfig;
use app::{BackendKind, Launch, RunMode};
use media::Media;

/// A unique path under the system temp directory for one dump.
fn dump_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "krkr-frame-dump-{}-{}.png",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or_default()
    ))
}

/// The dump holds the presented frame.
///
/// The run presents an empty scene, so the frame is the cleared background:
/// the dump is that frame, with the frame's size.
#[test]
fn the_dump_holds_the_presented_frame() {
    let path = dump_path();
    let launch = Launch {
        runtime: RuntimeConfig::default(),
        backend: BackendKind::Null,
        game: None,
        mode: RunMode::Headless { frames: 2 },
        dump_frame: Some(path.clone()),
    };
    app::run(launch).expect("headless run");

    let bytes = std::fs::read(&path).expect("the dump file");
    std::fs::remove_file(&path).ok();
    assert!(!bytes.is_empty(), "the dump file is empty");

    let image = media::decode_image(&Media::new(), &bytes, Some("png")).expect("a PNG");
    // The host's surface is 1280×720, and the frame is the size of the
    // surface.
    assert_eq!((image.width, image.height), (1280, 720));
    // The empty scene is the cleared background: opaque black.
    assert_eq!(&image.rgba[0..4], &[0u8, 0, 0, 255]);
}

/// A run without a dump writes no file.
#[test]
fn a_run_without_a_dump_writes_no_file() {
    let path = dump_path();
    let launch = Launch {
        runtime: RuntimeConfig::default(),
        backend: BackendKind::Null,
        game: None,
        mode: RunMode::Headless { frames: 1 },
        dump_frame: None,
    };
    app::run(launch).expect("headless run");
    assert!(
        !path.exists(),
        "the dump file was written without being asked for"
    );
}
