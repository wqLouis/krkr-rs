// Guard self-test fixture: L1 TVP/TJS/KAG vocabulary in code MUST be flagged.
// The scan ignores this comment, so only the identifiers below matter.
pub const BUTTON: i32 = LT_LEFT_BUTTON;
pub fn sample() -> i32 {
    TVP_sample_flag
}
pub mod KAG {
    // A `kag` module name is the same violation.
}
