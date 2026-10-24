// R1: a window teardown that takes the window's layer records with it.
//
// The reference unregisters the window and invalidates only the Window.add
// registry (reference/cpp/core/visual/WindowIntf.cpp:175-241, :208-222); a
// layer record belongs to the layer's own script object, so dropping the
// window must not remove it.
fn remove_window(scene: &mut Scene) {
    scene.destroy_layer(1);
    scene.remove_layer(2);
}
