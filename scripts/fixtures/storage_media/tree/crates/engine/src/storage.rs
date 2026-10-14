// Fixture: the L2 storage-media registration seam the port serves schemes
// through. It is not itself keyed on a scheme, so it registers nothing.

pub struct Storage;

impl Storage {
    pub fn register_media(&self, _name: String, _bytes: Vec<u8>) {}
}
