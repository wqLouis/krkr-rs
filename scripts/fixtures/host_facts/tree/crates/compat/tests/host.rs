// An integration test caller of an otherwise-unwired seam.
fn only_a_test(host: &CompatHost) {
    host.set_unwired();
}
