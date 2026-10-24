// R2: a layer teardown that destroys the whole subtree.
//
// The reference severs the children with Part()
// (reference/cpp/core/visual/LayerIntf.cpp:543-549); their records stay
// alive until their own script objects are finalized.
fn invalidate_layer(scene: &mut Scene) {
    scene.destroy_layer(1);
}
