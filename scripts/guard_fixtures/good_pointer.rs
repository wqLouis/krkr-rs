// Guard self-test fixture: near-miss patterns that MUST NOT be flagged.
// Not compiled and not part of any crate.

struct Node {
    rgba: Vec<u8>,
}

struct Wrapper {
    value: *const Node,
    count: u32,
}

fn numeric_compare(n: usize) -> bool {
    n > 0
}

fn slice_compare(bytes: &[u8]) -> bool {
    bytes.len() > 0
}

fn numeric_narrow(value: u64) -> u32 {
    value as u32
}

fn signed_ok(value: i32) -> i64 {
    value as i64
}

fn byte_ok(value: u8) -> u8 {
    let byte = value as u8;
    byte
}

fn address_kept(payload: *const Node) -> usize {
    payload as usize
}

fn fft_window(window: i32) -> i64 {
    window as i64
}

// A numeric field read through a pointer-holding struct is not the address.
fn field_ok(wrapper: Wrapper) -> bool {
    wrapper.count > 0
}

// A helper without an address-suggesting name does not taint its result.
fn buffer_len(data: &[u8]) -> usize {
    data.len()
}

fn helper_ok(data: &[u8]) -> bool {
    let n = buffer_len(data);
    n > 0
}
