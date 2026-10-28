//! Touch contacts and presentation outcomes.
//!
//! These are plain values produced by the platform layer and consumed by the
//! engine. The engine owns the semantic input state (focus, hit-testing,
//! gestures); the platform layer only captures raw device input and reduces a
//! presented frame to an outcome.
//!
//! The raw per-frame input events live in [`crate::input`]; the values here
//! are the shapes the engine stores.

/// Phase of a touch contact point.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TouchPhase {
    /// The contact began.
    Started,
    /// The contact moved.
    Moved,
    /// The contact ended normally.
    Ended,
    /// The contact was cancelled by the system.
    Cancelled,
}

/// A single touch contact point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TouchPoint {
    /// Stable identifier for the contact across its lifetime.
    pub id: u64,
    /// Position in logical window coordinates.
    pub pos: (f32, f32),
    /// Current phase of the contact.
    pub phase: TouchPhase,
}

/// A change to the presentation surface reported by the render pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SurfaceEvent {
    /// The surface was resized to the given physical size.
    Resized(u32, u32),
    /// The surface was lost and must be recreated.
    Lost,
    /// The surface was restored after a loss.
    Restored,
    /// The surface is out of date and should be recreated.
    OutOfDate,
}

/// Why a backend abandoned a frame without a surface fault.
///
/// A skip is not an error: the backend performed no work that could fail and
/// the previous surface contents stay on screen. The caller counts the skip
/// and reports it, so a display that stops releasing images is visible in the
/// log instead of looking like a stall.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PresentSkip {
    /// The display did not release a swapchain image within the frame's
    /// presentation budget.
    AcquireTimeout,
}

/// Result of a render/present action.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Presentation {
    /// Whether a frame was actually presented.
    pub presented: bool,
    /// A surface change the caller must react to, if any.
    pub surface_event: Option<SurfaceEvent>,
    /// Why the backend skipped this frame, or `None` when it presented the
    /// frame or reported a surface fault instead.
    pub skip: Option<PresentSkip>,
}
