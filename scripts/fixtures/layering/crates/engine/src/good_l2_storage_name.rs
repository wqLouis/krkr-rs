//! Near miss: L2 may name a generic archive path, mention `file://` in prose,
//! and log a sentence that contains the media prefix — none is a storage-name
//! literal.  The rule matches only a literal whose content *begins* with the
//! `file:` prefix.

pub fn archive_name() -> &'static str {
    "archive.xp3>data/script.ks"
}

pub fn log_missing(path: &str) {
    log::debug!("could not open file:// resource {path}");
}
