// Guard self-test near miss: a compat test may use `std::fs` to build
// fixtures; the compat platform rule deliberately excludes `crates/compat/tests/`.
#[test]
fn reads_fixture() {
    let _ = std::fs::metadata("fixture");
}
