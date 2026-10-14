//! Fixture port for the `beta` domain: a scheme-keyed `register_media` call.

pub fn register_media(storage: &Storage, container: &str, key: &str, bytes: Vec<u8>) {
    storage.register_media(format!("beta://{container}/{key}"), bytes);
}
