// Guard self-test fixture: compat platform code MUST be flagged.
pub fn stat(name: &str) -> u64 {
    std::fs::metadata(name).map(|m| m.len()).unwrap_or(0)
}
pub fn open_window() {
    let _ = winit::window::WindowAttributes::default();
}
pub fn use_jni() {
    let _ = core::mem::size_of::<jni::sys::jint>();
}
