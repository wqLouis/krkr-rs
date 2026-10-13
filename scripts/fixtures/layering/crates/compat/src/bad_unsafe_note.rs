// Guard self-test fixture: an unsafe block and an unsafe fn with no SAFETY
// note MUST be flagged.
pub fn read(p: *const u8) -> u8 {
    unsafe { *p }
}

pub unsafe fn read_fn(p: *const u8) -> u8 {
    *p
}
