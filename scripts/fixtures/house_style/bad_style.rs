// Guard self-test fixture: every rule below MUST be reported by
// scripts/check-house-style.sh.  Not compiled and not part of any crate.

#![allow(clippy::pedantic)]

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

// --- reference-citation and history-comment fixtures -----------------------

// Ported from the C++ TVP/KiriKiri original implementation; this port of the
// reference engine keeps its upstream names and cites
// (reference/cpp/core/base/StorageIntf.cpp:1061) and the parenthetical form
// (psbfile/main.cpp + PSBFile.cpp).

// The reference fires the event before the frame ends.
pub fn fire_event() {}

// The mixer used to run inside the hook; it was previously replaced by the
// engine pump. Reads the pending flag before this step.
pub fn pump() {}

// --- clippy-allow fixtures --------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub fn wide_a(a: u32, b: u32, c: u32, d: u32, e: u32, f: u32) {
    let _ = (a, b, c, d, e, f);
}

#[expect(clippy::type_complexity)]
pub fn complex_type() -> Box<dyn Fn()> {
    Box::new(|| {})
}

#[cfg(test)]
mod clippy_in_tests {
    // The clippy-allow rule applies to test code too.
    #[allow(clippy::needless_range_loop)]
    fn helper() {}
}
