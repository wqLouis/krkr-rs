//! Shared helpers for the `krkr-vm` integration tests.

#![allow(dead_code)]

use std::sync::{Mutex, MutexGuard, OnceLock};

fn lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Serialize tests that touch the process-global TJS VM.
pub fn vm_lock() -> MutexGuard<'static, ()> {
    lock().lock().unwrap_or_else(|poison| poison.into_inner())
}
