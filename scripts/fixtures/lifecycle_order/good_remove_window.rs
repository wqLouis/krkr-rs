// R1: a window teardown that drops the window record only.
//
// The layer records stay with their own script objects.
fn remove_window(scene: &mut Scene) {
    scene.windows.retain(|window| window.id != 1);
}
