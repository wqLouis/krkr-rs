// Guard self-test fixture: every pattern below MUST be flagged by the pointer
// rules.  Not compiled and not part of any crate.
use std::ffi::c_char;

struct Node {
    rgba: Vec<u8>,
}

fn ptr_flow(instance: *mut Node) {
    let handle = instance as usize as i64;
    if handle > 0 {
        let _ = handle;
    }
}

fn addr_narrow(rgba: &mut Vec<u8>) {
    let raw = rgba.as_mut_ptr() as usize;
    let low = raw as u32;
    let _ = low;
}

fn ptr_compare(payload: *const Node, other: *const Node) {
    if payload == other {
        let _ = payload;
    }
}

fn char_roundtrip(value: u8) {
    let signed = value as i8 as c_char;
    let _ = signed;
}

fn char_compare(c: c_char) {
    if c < 0 {
        let _ = c;
    }
}

// Interprocedural: a helper whose name says "address" returns the raw address.
fn image_pixel_buffer(data: &[u8]) -> usize {
    data.as_ptr() as usize
}

fn helper_flow(data: &[u8]) {
    let base = image_pixel_buffer(data);
    if base > 0 {
        let _ = base;
    }
}

// Opaque-handle construction and sign test on a later line.
fn opaque_flow(instance: *mut Node) {
    let h = instance as usize;
    let _ = std::num::NonZeroUsize::new(h);
    let _ = u32::try_from(h);
    let _ = h.max(0);
}
