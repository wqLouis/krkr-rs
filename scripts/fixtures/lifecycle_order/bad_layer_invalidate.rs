// R3: a layer teardown that invalidates another script object.
//
// The reference severs the children with Part()
// (reference/cpp/core/visual/LayerIntf.cpp:543-549); it never invalidates a
// child script object.
fn layer_invalidate(host: &Host) {
    host.invalidate_retained(1);
}
