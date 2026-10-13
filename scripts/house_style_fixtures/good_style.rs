// Guard self-test fixture: the near misses below MUST NOT be reported by
// scripts/check-house-style.sh.  Not compiled and not part of any crate.

use crate::some::*; // a private glob import is not a re-export
pub use self::inner::*; // a module glob, not a whole-crate re-export

pub struct Widget {
    value: u32,
    ready: bool,
    started: bool,
}

impl Widget {
    pub fn get_value(&self) -> u32 {
        self.value + 1 // does real work
    }

    pub fn set_value(&mut self, value: u32) {
        self.value = value.max(1); // clamps; not a bare move
    }

    pub fn is_ready(&self) -> bool {
        self.ready && self.started // does real work
    }
}

pub fn take(values: &[u8], name: &str, boxed: &dyn Fn(), path: &Path) {
    let _ = (values, name, boxed, path);
}

// Hex-looking runs are not decision numbers: 0xD800 and U+D800 are scalars.
pub fn d_in_string() -> &'static str {
    "D7 is not a decision number in a string"
}

pub fn announce(count: u32) {
    log::info!("loaded {} items", count);
}
