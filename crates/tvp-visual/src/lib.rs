//! TVP visual native classes (Window/Layer/Bitmap/Font/Timer) + logical
//! scene model. No Bevy dependency: natives mutate [`scene::Scene`], the
//! `render` crate turns it into Bevy entities. See WAVE3.md.
//!
//! # Architecture
//!
//! The C ABI (`tjs2-sys`) cannot intercept **property sets** on native
//! instances (instance classes register methods only), and object values
//! cannot cross the boundary (no object handles). The natives therefore
//! register *implementation* classes (`__TvpWindow`, `__TvpLayer`,
//! `__TvpBitmap`, `__TvpFont`, `__TvpTimer`) whose methods operate on scene
//! ids, and a setup script (run by [`register_visual`]) defines the
//! game-visible classes (`Window`, `Layer`, ...) as script subclasses that
//! add:
//!
//! * constructor argument translation — object arguments (`new Layer(win,
//!   par)`) are converted to their `__id` scene ids before reaching the
//!   native constructor (the native sees plain integers),
//! * `property` accessors (`visible`, `opacity`, `width`, ...) whose
//!   getters/setters delegate to native getter/setter methods,
//! * per-class object registries (`global.__tvp_*`) so object-returning
//!   properties (`window.primaryLayer`, `layer.window`, `layer.parent`)
//!   can resolve a scene id back to its script object.
//!
//! See the module docs in `natives/mod.rs` for the exact native surface,
//! the rationale and the ABI limitations this design works around.

pub mod bitmap;
pub mod natives;
pub mod scene;
pub mod tlg;

pub use natives::{register_visual, timer_poll};
