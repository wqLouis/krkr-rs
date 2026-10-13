// Guard self-test fixture: L2 TVP storage-name syntax MUST be flagged.
// The vocabulary token scan masks string contents, so these literals are
// invisible to it; the storage-name rule reads the literal content instead.
pub fn media_prefixes() -> [&'static str; 3] {
    ["file://.", "file://", "file:"]
}
