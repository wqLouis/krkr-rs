//! Near miss: compat source that reaches the engine through its seam only.
//! Naming `engine_api` is not naming the `engine` crate.

use crate::engine_api::Scene;

pub fn layer_count(scene: &Scene) -> usize {
    scene.layers.len()
}
