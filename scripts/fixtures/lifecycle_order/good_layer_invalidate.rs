// R3: a layer teardown that touches only its own record.
//
// The children are detached, not invalidated.
fn layer_invalidate(scene: &mut Scene) {
    scene.invalidate_layer(1);
}
