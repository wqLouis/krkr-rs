//! The CPU-presented frame of the null backend.
//!
//! The null backend composites the presented scene with the software
//! rasterizer, and the caller reads the frame through a slot it owns: the
//! slot is installed into the render state before the state is handed to the
//! middle layer, so the composition root that owns the window sees the frame
//! without the middle layer knowing about it.

use std::sync::{Arc, Mutex, MutexGuard};

use api::error::RenderError;

/// One presented frame's pixels.
///
/// The frame is RGBA8, tightly packed and row-major, and it is the CPU
/// compositing of the presented scene: the same traversal and the same blend
/// formulas the GPU path uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresentedFrame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// The frame's pixels, `width * height * 4` bytes long.
    pub pixels: Vec<u8>,
}

/// The shared slot the null backend composites each presented frame into.
///
/// The caller creates the slot, installs it into the render state and keeps
/// it, so the frame reaches the layer that owns the window. Cloning the slot
/// shares the same frame.
#[derive(Clone, Debug, Default)]
pub struct PresentedFrameSlot {
    inner: Arc<Mutex<Option<PresentedFrame>>>,
}

impl PresentedFrameSlot {
    /// Creates an empty slot.
    pub fn new() -> Self {
        Self::default()
    }

    /// Calls `visit` with the most recent frame, if the backend has presented
    /// one.
    pub fn with_frame<R>(&self, visit: impl FnOnce(&PresentedFrame) -> R) -> Option<R> {
        let guard = self.lock();
        guard.as_ref().map(visit)
    }

    /// Locks the slot and hands the frame to the backend that fills it.
    pub(crate) fn frame_mut(&self) -> MutexGuard<'_, Option<PresentedFrame>> {
        self.lock()
    }

    /// Locks the slot, recovering the frame when the lock was poisoned.
    ///
    /// A panic while the frame was held leaves the frame behind, not the slot
    /// unusable: the next frame replaces it.
    fn lock(&self) -> MutexGuard<'_, Option<PresentedFrame>> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

/// Reports that a frame does not fit the host address space.
pub(crate) fn frame_too_large() -> RenderError {
    RenderError::OutOfMemory {
        message: "the presented frame does not fit the host address space".to_owned(),
    }
}
