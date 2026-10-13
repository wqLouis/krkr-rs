// Guard self-test fixture: near-miss patterns that MUST NOT be flagged by the
// `error-swallow` rule.  Not compiled and not part of any crate.

use std::io;

fn cited_false(result: io::Result<bool>) -> bool {
    match result {
        Ok(is_dir) => is_dir,
        // A stat failure is `false`, matching the reference predicate
        // (`reference/cpp/fixture/example.cpp:1-2`).
        Err(_) => false,
    }
}

fn propagated(result: io::Result<u8>) -> io::Result<u8> {
    match result {
        Ok(value) => Ok(value),
        Err(error) => Err(error),
    }
}

fn optional(value: Option<u8>) -> u8 {
    match value {
        Some(inner) => inner,
        None => 0,
    }
}

fn logged(result: io::Result<u8>) -> u8 {
    match result {
        Ok(value) => value,
        Err(_) => {
            log::warn!("read failed");
            0
        }
    }
}
