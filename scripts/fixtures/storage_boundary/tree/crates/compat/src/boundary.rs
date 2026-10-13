//! Fixture for the storage-boundary check.
//!
//! It exercises the four shapes the check must tell apart: a raw read, a raw
//! `game_dir().join`, a name routed through `strip_file_media`, and a literal
//! constant. Only the first two are findings.

use crate::storage::strip_file_media;

const LITERAL_NAME: &str = "literal.tjs";

struct Storage;

impl Storage {
    fn read(&self, _name: &str) -> Vec<u8> {
        Vec::new()
    }

    fn game_dir(&self) -> std::path::PathBuf {
        std::path::PathBuf::new()
    }
}

fn raw_read(storage: &Storage, name: &str) {
    let _ = storage.read(name);
}

fn raw_join(storage: &Storage, name: &str) {
    let _ = storage.game_dir().join(name);
}

fn stripped_read(storage: &Storage, name: &str) {
    let lookup = strip_file_media(name);
    let _ = storage.read(lookup);
}

fn literal_read(storage: &Storage) {
    let _ = storage.read(LITERAL_NAME);
    let _ = storage.read("inline.tjs");
}
