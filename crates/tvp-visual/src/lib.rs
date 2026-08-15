//! TVP visual native classes (Window/Layer/Bitmap/Font/Timer) + logical
//! scene model. No Bevy dependency: natives mutate [`scene::Scene`], the
//! `render` crate turns it into Bevy entities. See WAVE3.md.

pub mod bitmap;
pub mod scene;
