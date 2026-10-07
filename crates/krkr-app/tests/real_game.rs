//! Real-game integration tests, ignored by default.
//!
//! These mount a real KiriKiri game through the new middle layer and exercise
//! archive reading, image decoding and audio probing. They are `#[ignore]`d
//! because they need a game on disk; set `KRKR_TEST_GAME` to the game
//! directory and run:
//!
//! ```text
//! KRKR_TEST_GAME=/path/to/game cargo test -p krkr-app --test real_game -- --ignored --nocapture
//! ```

use krkr_engine::storage::Storage;
use krkr_fs::FileSystem;
use krkr_media::{Media, decode_image, probe_audio};

fn game_dir() -> Option<String> {
    match std::env::var("KRKR_TEST_GAME") {
        Ok(dir) if !dir.is_empty() => Some(dir),
        _ => {
            eprintln!("skipping: set KRKR_TEST_GAME to a game directory");
            None
        }
    }
}

fn mount(dir: &str) -> Storage {
    Storage::mount(FileSystem::host(dir), dir).expect("mount game")
}

#[test]
#[ignore = "needs KRKR_TEST_GAME pointing at a real game"]
fn mounts_archives_and_reads_scripts() {
    let Some(dir) = game_dir() else { return };
    let storage = mount(&dir);

    assert!(storage.archive_count() >= 1, "at least one archive");

    // A KiriKiri game always has startup.tjs (on disk or in an archive).
    let startup = storage.read("startup.tjs").expect("read startup.tjs");
    assert!(!startup.is_empty());
    println!("startup.tjs: {} bytes", startup.len());

    // patch.tjs ships next to the archives in the reference test game.
    if let Ok(patch) = storage.read("patch.tjs") {
        println!("patch.tjs: {} bytes", patch.len());
    }
}

#[test]
#[ignore = "needs KRKR_TEST_GAME pointing at a real game"]
fn decodes_real_images_and_probes_audio() {
    let Some(dir) = game_dir() else { return };
    let storage = mount(&dir);
    let media = Media::new();

    // Images: TLG is the KiriKiri native format.
    let mut images = storage.get_file_list("*.tlg", 0);
    if images.is_empty() {
        images = storage.get_file_list("*.png", 0);
    }
    assert!(!images.is_empty(), "game should ship some images");
    let mut decoded = 0usize;
    for name in images.iter().take(8) {
        let bytes = storage.read(name).expect("read image");
        let image = decode_image(&media, &bytes, Some(name)).expect("decode image");
        assert!(image.width > 0 && image.height > 0);
        assert_eq!(
            image.rgba.len(),
            image.width as usize * image.height as usize * 4
        );
        decoded += 1;
    }
    println!("decoded {decoded} images");
    assert!(decoded > 0);

    // Audio: Ogg/Opus voices and BGM.
    let mut audio = storage.get_file_list("*.ogg", 0);
    if audio.is_empty() {
        audio = storage.get_file_list("*.opus", 0);
    }
    assert!(!audio.is_empty(), "game should ship some audio");
    let bytes = storage.read(&audio[0]).expect("read audio");
    let spec = probe_audio(&media, &bytes, Some(&audio[0])).expect("probe audio");
    assert!(spec.sample_rate > 0 && spec.channels > 0);
    println!(
        "{}: {} Hz, {} ch, frames={:?}",
        audio[0], spec.sample_rate, spec.channels, spec.total_frames
    );
}
