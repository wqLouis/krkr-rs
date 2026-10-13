// Guard self-test near miss: an unsafe block with its SAFETY note and an
// unsafe fn with a `# Safety` doc are both accepted.
pub fn read(p: *const u8) -> u8 {
    // SAFETY: the caller guarantees `p` is valid for one byte.
    unsafe { *p }
}

pub fn read_inline(p: *const u8) -> u8 {
    unsafe { *p } // SAFETY: the caller guarantees `p` is valid for one byte.
}

/// # Safety
///
/// `p` must be valid for one byte.
pub unsafe fn read_fn(p: *const u8) -> u8 {
    *p
}
