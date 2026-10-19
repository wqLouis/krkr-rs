// Guard self-test fixture: compat naming the engine crate directly MUST be
// flagged.  The seam module (`engine_api`) is the only place allowed to do
// this.
use engine::Scene;

pub fn layer_count(scene: &Scene) -> usize {
    scene.layers.len()
}
