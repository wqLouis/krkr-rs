// R4: a window teardown that reaches into the layer registry.
//
// The window's teardown invalidates only the Window.add registry
// (reference/cpp/core/visual/WindowIntf.cpp:208-222).
fn window_destroy(state: &mut VisualState) {
    let _layers = &state.objects.layers;
}
