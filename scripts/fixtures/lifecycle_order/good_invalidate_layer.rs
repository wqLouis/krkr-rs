// R2: a layer teardown that detaches the layer and severs its children.
//
// remove_layer() releases this layer's own record; the children's records
// stay alive.
fn invalidate_layer(scene: &mut Scene) {
    scene.remove_layer(1);
}
