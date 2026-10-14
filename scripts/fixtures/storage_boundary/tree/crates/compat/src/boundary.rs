//! Fixture for the storage-boundary check.
//!
//! It exercises the shapes the check must tell apart: a raw read, a raw
//! `game_dir().join`, a name routed through `strip_file_media`, a literal
//! constant, a raw write seam, a write seam whose own body strips the prefix,
//! and a raw host write. Only the unstripped calls are findings.

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

struct Resolver;

impl Resolver {
    fn save(&self, _name: &str) {}

    fn save_image(&self, name: &str) {
        let _ = strip_file_media(name);
    }
}

struct Fs;

fn raw_read(storage: &Storage, name: &str) {
    let _ = storage.read(name);
}

fn raw_join(storage: &Storage, name: &str) {
    let _ = storage.game_dir().join(name);
}

fn raw_save(resolver: &Resolver, name: &str) {
    resolver.save(name);
}

fn raw_host_write(fs: &Fs, name: &str, bytes: &[u8]) {
    fs::write(fs, name, bytes);
}

fn raw_host_path(name: &str) {
    let _ = FsPath::host(name);
}

fn stripped_read(storage: &Storage, name: &str) {
    let lookup = strip_file_media(name);
    let _ = storage.read(lookup);
}

fn stripped_save(resolver: &Resolver, name: &str) {
    resolver.save_image(name);
}

fn stripped_host_write(fs: &Fs, name: &str, bytes: &[u8]) {
    let lookup = strip_file_media(name);
    fs::write(fs, lookup, bytes);
}

fn literal_read(storage: &Storage) {
    let _ = storage.read(LITERAL_NAME);
    let _ = storage.read("inline.tjs");
}
