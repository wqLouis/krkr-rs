// Guard self-test fixture: every pattern below MUST be flagged by the
// `error-swallow` rule.  Not compiled and not part of any crate.

use std::io;

fn wildcard_arm(result: io::Result<u8>) -> u8 {
    match result {
        Ok(value) => value,
        Err(_) => 0,
    }
}

fn wildcard_rest(result: io::Result<()>) -> bool {
    match result {
        Ok(()) => true,
        Err(..) => false,
    }
}

fn let_body(result: io::Result<u8>) {
    if let Err(_) = result {
        return;
    }
}
