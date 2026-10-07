//! Host-produced input, window and process snapshots.
//!
//! These are plain values produced by the host/frontend layer (L4) and
//! consumed by the middle layer (L2). The middle layer owns the semantic
//! input state (focus, hit-testing, gestures); the host only captures raw
//! device input and window/process facts.

use std::path::PathBuf;

/// Maximum number of distinct key codes tracked by [`KeyState`].
pub const MAX_KEY_CODE: u32 = 256;

/// A compact, allocation-free set of pressed keys.
///
/// Up to [`MAX_KEY_CODE`] (256) key codes are tracked in a four-word bitmap.
/// Codes outside that range are ignored by [`KeyState::set_down`] and report
/// `false` from [`KeyState::is_down`]. The representation deliberately avoids
/// a `HashSet` so the snapshot is cheap to clone and copy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct KeyState {
    bits: [u64; 4],
}

impl KeyState {
    /// Creates an empty key state.
    pub const fn new() -> Self {
        Self { bits: [0; 4] }
    }

    /// Returns `true` if `code` is currently pressed.
    ///
    /// Codes at or above [`MAX_KEY_CODE`] return `false`.
    pub fn is_down(&self, code: u32) -> bool {
        if code >= MAX_KEY_CODE {
            return false;
        }
        let word = (code / 64) as usize;
        let bit = 1u64 << (code % 64);
        self.bits[word] & bit != 0
    }

    /// Sets or clears the pressed state of `code`.
    ///
    /// Codes at or above [`MAX_KEY_CODE`] are ignored.
    pub fn set_down(&mut self, code: u32, down: bool) {
        if code >= MAX_KEY_CODE {
            return;
        }
        let word = (code / 64) as usize;
        let bit = 1u64 << (code % 64);
        if down {
            self.bits[word] |= bit;
        } else {
            self.bits[word] &= !bit;
        }
    }

    /// Marks `code` as pressed.
    pub fn press(&mut self, code: u32) {
        self.set_down(code, true);
    }

    /// Marks `code` as released.
    pub fn release(&mut self, code: u32) {
        self.set_down(code, false);
    }

    /// Clears every key.
    pub fn clear(&mut self) {
        self.bits = [0; 4];
    }

    /// Returns `true` if no key is pressed.
    pub fn is_empty(&self) -> bool {
        self.bits.iter().all(|word| *word == 0)
    }

    /// Iterates the codes of all pressed keys in ascending order.
    pub fn iter_down(&self) -> impl Iterator<Item = u32> + '_ {
        self.bits.iter().enumerate().flat_map(|(word, bits)| {
            (0..64).filter_map(move |bit| {
                if bits & (1u64 << bit) != 0 {
                    Some((word as u32) * 64 + bit as u32)
                } else {
                    None
                }
            })
        })
    }
}

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

/// A snapshot of all raw input produced by the host for one frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InputState {
    /// Mouse position in logical window coordinates.
    pub mouse_pos: (f32, f32),
    /// Left/middle/right mouse button states.
    pub mouse_buttons: [bool; 3],
    /// Pressed keys.
    pub keys: KeyState,
    /// Accumulated wheel delta for the frame.
    pub wheel: f32,
    /// Active touch contacts.
    pub touch: Vec<TouchPoint>,
    /// Text entered this frame (for IME/text fields).
    pub text_input: String,
}

/// Process-level environment facts supplied by the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessEnv {
    /// Directory containing the game data.
    pub project_dir: PathBuf,
    /// Writable per-user application data directory.
    pub app_data_dir: PathBuf,
    /// Optional directory for persistent state files.
    pub state_dir: Option<PathBuf>,
    /// Screen size in physical pixels.
    pub screen_size: (u32, u32),
    /// Whether the device reports a touch input source.
    pub touch_device: bool,
}

/// A change to the presentation surface reported by the render pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SurfaceEvent {
    /// The surface was resized to the given physical size.
    Resized(u32, u32),
    /// The surface was lost and must be recreated.
    Lost,
    /// A previously lost surface was restored.
    Restored,
    /// The surface is out of date and should be recreated.
    OutOfDate,
}

/// Result of a render/present action.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Presentation {
    /// Whether a frame was actually presented.
    pub presented: bool,
    /// A surface change the caller must react to, if any.
    pub surface_event: Option<SurfaceEvent>,
}
