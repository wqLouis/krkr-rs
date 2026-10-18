fn good_layer(host: &Host, scene: &mut Scene) {
    host.invalidate_retained(1);
    scene.invalidate_layer(1);
}
