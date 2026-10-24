// R4: a window teardown that touches only the window registry.
fn window_destroy(state: &mut VisualState) {
    let _windows = &state.objects.windows;
}
