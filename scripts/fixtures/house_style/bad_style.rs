// Guard self-test fixture: every rule below MUST be reported by
// scripts/check-house-style.sh.  Not compiled and not part of any crate.

// A code comment citing a decision number (D9) is a house-style violation.
pub use crate::some::*;

pub struct Widget {
    value: u32,
    ready: bool,
}

impl Widget {
    pub fn get_value(&self) -> u32 {
        self.value
    }

    pub fn get_result(&self) -> Result<u32, ()> {
        Ok(self.value)
    }

    pub fn set_value(&mut self, value: u32) {
        self.value = value;
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }
}

pub fn take(values: &String, items: &Vec<u8>, boxed: &Box<dyn Fn()>, path: &PathBuf) {
    let _ = (values, items, boxed, path);
}

pub fn announce(level: u8) {
    if level > 0 {
        log::warn!("internal L2 detail");
    }
}
