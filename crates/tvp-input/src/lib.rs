//! tvp-input — shared input state + the `Mouse` and `Key` TJS natives.
//!
//! This crate defines the **input state** the whole engine reads from and
//! writes to, and registers the two input native classes KiriKiri scripts
//! call: `Mouse` and `Key`.
//!
//! The state is **not** fed by OS input capture here — the app (Bevy
//! wiring in `crates/render/src/input_bridge.rs`) writes into the shared
//! state every frame; the natives only read it when scripts call them. The
//! frame protocol below is exactly what that bridge drives.
//!
//! # Frame protocol (the Bevy wiring API)
//!
//! The state is process-global, inside `Arc<Mutex<InputState>>`. The app
//! obtains it once ([`input_state`], or installs its own instance with
//! [`set_context`]) and drives it once per frame:
//!
//! ```rust,ignore
//! use tvp_input::{input_state, key_code, MB_LEFT};
//!
//! let state = input_state();              // Arc<Mutex<InputState>>
//! let mut s = state.lock().unwrap();
//!
//! s.begin_frame();                        // clears per-frame data
//! s.set_mouse_pos(300, 200);              // absolute cursor position
//! s.add_wheel(0, 120, 0);                 // wheel deltas this frame
//! s.set_mouse_button(MB_LEFT, true);      // button held state (mbLeft..mbX2)
//! s.set_mouse_visible(true);
//! s.set_key_down(key_code("kReturn").unwrap()); // key went down
//! s.set_key_up(key_code("kShift").unwrap());    // key went up
//! s.end_frame();                          // commit held counters, frame += 1
//! ```
//!
//! * `begin_frame()` clears the per-frame data: the mouse release flags,
//!   the accumulated wheel deltas, the key `released` map and
//!   `last_pressed`. Call it once at the top of the frame, after the
//!   scripts have seen the previous frame's state.
//! * The `set_*` calls record what happened *this* frame (events the app
//!   drained from Bevy).
//! * `end_frame()` commits: it bumps the held-frame counters (`KeyState`
//!   `pressed`/`repeat`, `MouseState` `hold`) and advances [`InputState::frame`].
//!
//! Scripts read the state through the natives at any time — there is no
//! "script tick"; the natives just return the current state.
//!
//! # Native surface
//!
//! `Mouse` (static class):
//!
//! | method | returns |
//! |---|---|
//! | `getCursorX()` / `getCursorY()` | cursor position (int) |
//! | `getCursorPos()` | `"x,y"` string (see the note below) |
//! | `setCursorPos(x, y)` | void; writes the cursor position back into the
//!   state (the app may read it back or push it to the OS) |
//! | `getWheelRot()` / `getWheelRotX()` / `getWheelRotY()` /
//!   `getWheelRotZ()` | accumulated wheel delta since the last
//!   `begin_frame` (int; `getWheelRot` is the classic vertical Y axis) |
//! | `isVisible()` / `setVisible(bool)` | cursor visibility |
//! | `getPressed(button)` / `getReleased(button)` | bool — held /
//!   released-this-frame state of one of the TVP mouse buttons |
//! | `getRepeat(button)` | frames the button has been held |
//! | `getClickCount(button)` | presses in the current multi-click sequence
//!   (1 = single, 2 = double, ...; 0 once the sequence times out) |
//!
//! Buttons are indexed by the TVP button constants from
//! `reference/cpp/core/visual/tvpinputdefs.h`
//! (`enum tTVPMouseButton { mbLeft, mbRight, mbMiddle, mbX1, mbX2 }`):
//! [`MB_LEFT`]=0, [`MB_RIGHT`]=1, [`MB_MIDDLE`]=2, [`MB_X1`]=3, [`MB_X2`]=4.
//!
//! `Key` (static class):
//!
//! | member | returns |
//! |---|---|
//! | `getPressed(code)` | bool — whether the key is currently held |
//! | `getReleased(code)` | bool — whether the key was released this frame |
//! | `getRepeat(code)` | frames the key has been held (increments while
//!   held, 0 when not held) |
//! | `kBack`, `kTab`, `kReturn`, `kShift`, `kControl`, `kMenu`,
//!   `kEscape`, `kSpace`, `kLeft`, `kUp`, `kRight`, `kDown`, `kA`..`kZ`,
//!   `k0`..`k9`, `kF1`..`kF24`, `kNumPad0`..`kNumPad9`, the mouse-button
//!   VKs (`kLButton`, `kRButton`, `kMButton`, `kXButton1`, `kXButton2`) and
//!   the KiriKiri gamepad VKs (`kPadLeft`..`kPadAny`), ... | get-only
//!   constant properties: the Windows virtual-key code (see
//!   [`KEY_CODE_TABLE`]) |
//!
//! Key codes are Windows virtual-key codes (`VK_*`), the same codes
//! `System.getKeyState` uses — see
//! `reference/cpp/core/environ/vkdefine.h`, `ScriptMgnIntf.cpp` (the global
//! `VK_*` table, incl. `VK_CANCEL` and `VK_PAD*`) and `tvpinputdefs.h`.
//!
//! # Relationship to the reference
//!
//! The provided reference subset has **no** `Mouse`/`Key` native class.
//! The reference input surface is:
//!
//! * `reference/cpp/core/visual/tvpinputdefs.h` — `enum tTVPMouseButton`
//!   (`mbLeft`..`mbX2`), the `TVP_SS_*` shift-state flags, the IME modes and
//!   the KiriKiri-specific `VK_PAD*` gamepad virtual-key codes (incl.
//!   `VK_PADANY`);
//! * `System.getKeyState(code[, getcurrent=true])` — registered by
//!   `tvp-natives`; reference `reference/cpp/core/base/impl/SystemImpl.cpp:634`
//!   (`TVPGetAsyncKeyState`, `:43`) over the host scancode array
//!   (`reference/cpp/core/environ/impl/TVPWindow.h:213`,
//!   `reference/cpp/core/visual/impl/DInputMgn.cpp:696` for the pad path);
//! * `Window` mouse/key/touch events plus `mouseCursorState` /
//!   `hideMouseCursor` (`reference/cpp/core/visual/WindowIntf.cpp`).
//!
//! `Mouse`/`Key` are a **port compatibility layer** over the same shared
//! state (their member set follows the common KiriKiri script idiom, not a
//! reference C++ class). They deliberately share the reference key-code
//! space, so `Key.getPressed(code)` understands the mouse-button VKs (mapped
//! to the mouse state, like the reference's scancode array) and `VK_PADANY`
//! (true when any gamepad button is held), exactly as
//! `System.getKeyState(code)` does.
//!
//! # Deviations from the reference (documented)
//!
//! * `Mouse.getCursorPos()` with no argument returns the position as an
//!   `"x,y"` string; with an object argument it fills the object's `x`/`y`
//!   properties (via `Tjs2Engine::set_member`) and returns void, which is
//!   the conventional out-parameter form. The string form is a port
//!   convenience for callers that expect a value.
//! * `Mouse.setCursorPos` records the position and a *warp request*
//!   ([`InputState::take_mouse_warp`]); the host input bridge must consume it
//!   to move the OS cursor.
//! * Unknown key codes / out-of-range mouse buttons return 0/false rather
//!   than raising. This matches the reference `TVPGetAsyncKeyState`, which
//!   bounds-checks the scancode array and returns false.
//! * `Mouse.getClickCount` counts presses within
//!   [`MOUSE_CLICK_SEQUENCE_FRAMES`] frames and [`MOUSE_CLICK_MAX_MOVE`]
//!   pixels; the reference delegates to the OS double-click time and
//!   cursor region, which this frame-based state cannot query.
//!
//! # Testing
//!
//! The state is process-global, so tests must run single-threaded:
//! `cargo test -p tvp-input -- --test-threads=1`.

mod key;
mod mouse;

pub use key::{KEY_CODE_TABLE, key_code, register_key};
pub use mouse::register_mouse;

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_int};
use std::ptr;
use std::slice;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use tjs2_sys::{Tjs2Engine, VAL_INTEGER, VAL_REAL, VAL_STRING, VAL_VOID, Value, tjs2_malloc};

/// Register both input native classes (`Mouse`, `Key`) on `engine`.
///
/// Call once per engine, from the thread that owns the engine (the VM is
/// single-threaded). The app calls this next to the other
/// `register_*` calls before running `startup.tjs`.
pub fn register_all(engine: &Tjs2Engine) -> Result<(), String> {
    install_engine(engine);
    register_mouse(engine)?;
    register_key(engine)?;
    Ok(())
}

/// The engine the input natives were registered on.
///
/// Native callbacks receive only the raw `tjs2_engine*` ABI pointer (which
/// is *not* a `Tjs2Engine*`), but `Mouse.getCursorPos(obj)` must call
/// [`Tjs2Engine::set_member`] to fill the object. Store the registered
/// engine address at setup like `tvp-natives`/`tvp-visual` do; the host
/// keeps the engine alive for the process, so the address stays valid for
/// every callback. The address is the stable heap allocation the caller
/// passes (an `Arc<Tjs2Engine>` target, or a boxed engine in tests).
static ENGINE: OnceLock<Mutex<Option<usize>>> = OnceLock::new();

/// Record `engine` as the context for the input natives (idempotent).
fn install_engine(engine: &Tjs2Engine) {
    let cell = ENGINE.get_or_init(|| Mutex::new(None));
    *lock_ok(cell) = Some(engine as *const Tjs2Engine as usize);
}

/// The registered engine as a shared reference (see [`ENGINE`]).
///
/// Panics only if called before [`register_all`], which cannot happen for a
/// native callback: the callback exists only because registration ran.
pub(crate) fn context_engine() -> &'static Tjs2Engine {
    let cell = ENGINE.get_or_init(|| Mutex::new(None));
    let ptr = lock_ok(cell).expect("tvp-input: engine context not set");
    // SAFETY: the address was stored by register_all from a stable
    // `&Tjs2Engine`; the host keeps that engine alive while callbacks run.
    unsafe { &*(ptr as *const Tjs2Engine) }
}

// ---------------------------------------------------------------------------
// Shared input state
// ---------------------------------------------------------------------------

/// Mouse button indices — port of `enum tTVPMouseButton` from
/// `reference/cpp/core/visual/tvpinputdefs.h` (`mbLeft`..`mbX2`).
pub mod buttons {
    /// Left button (`mbLeft`).
    pub const MB_LEFT: usize = 0;
    /// Right button (`mbRight`).
    pub const MB_RIGHT: usize = 1;
    /// Middle button (`mbMiddle`).
    pub const MB_MIDDLE: usize = 2;
    /// First extra button (`mbX1`).
    pub const MB_X1: usize = 3;
    /// Second extra button (`mbX2`).
    pub const MB_X2: usize = 4;
    /// Number of tracked buttons.
    pub const MOUSE_BUTTONS: usize = 5;

    /// Windows virtual-key codes for the mouse buttons (`VK_*` from
    /// `reference/cpp/core/environ/vkdefine.h` / the Windows headers), for
    /// the Bevy wiring mapping events to button indices.
    pub const VK_LBUTTON: u32 = 0x01;
    pub const VK_RBUTTON: u32 = 0x02;
    pub const VK_MBUTTON: u32 = 0x04;
    pub const VK_XBUTTON1: u32 = 0x05;
    pub const VK_XBUTTON2: u32 = 0x06;

    /// Map a Windows virtual-key code to the TVP button index
    /// ([`MB_LEFT`]..[`MB_X2`]); `None` for anything that is not a mouse
    /// button code.
    pub fn from_vk(vk: u32) -> Option<usize> {
        match vk {
            VK_LBUTTON => Some(MB_LEFT),
            VK_RBUTTON => Some(MB_RIGHT),
            VK_MBUTTON => Some(MB_MIDDLE),
            VK_XBUTTON1 => Some(MB_X1),
            VK_XBUTTON2 => Some(MB_X2),
            _ => None,
        }
    }
}

pub use buttons::{
    MB_LEFT, MB_MIDDLE, MB_RIGHT, MB_X1, MB_X2, MOUSE_BUTTONS, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
    VK_XBUTTON1, VK_XBUTTON2,
};

/// Gamepad (joypad) virtual-key codes — the KiriKiri-specific `VK_PAD*`
/// range from `reference/cpp/core/visual/tvpinputdefs.h` (`VK_PAD_FIRST` is
/// `0x1B0`, `VK_PAD_LAST`/`VK_PADANY` are `0x1DF`).
///
/// The pad path is the reference `TVPGetJoyPadAsyncState`
/// (`reference/cpp/core/visual/impl/DInputMgn.cpp:696`): a plain button code
/// maps to one pad flag, and [`VK_PADANY`] is true when *any* pad button is
/// held. The host input bridge feeds these through
/// [`InputState::set_key_down`]/[`InputState::set_key_up`] like any other
/// VK code; [`InputState::vk_pressed`] and the `Key` natives treat
/// [`VK_PADANY`] specially.
pub mod pad {
    /// First KiriKiri pad virtual-key code (`VK_PAD_FIRST`).
    pub const VK_PAD_FIRST: u32 = 0x1B0;
    pub const VK_PADLEFT: u32 = 0x1B5;
    pub const VK_PADUP: u32 = 0x1B6;
    pub const VK_PADRIGHT: u32 = 0x1B7;
    pub const VK_PADDOWN: u32 = 0x1B8;
    pub const VK_PAD1: u32 = 0x1C0;
    pub const VK_PAD2: u32 = 0x1C1;
    pub const VK_PAD3: u32 = 0x1C2;
    pub const VK_PAD4: u32 = 0x1C3;
    pub const VK_PAD5: u32 = 0x1C4;
    pub const VK_PAD6: u32 = 0x1C5;
    pub const VK_PAD7: u32 = 0x1C6;
    pub const VK_PAD8: u32 = 0x1C7;
    pub const VK_PAD9: u32 = 0x1C8;
    pub const VK_PAD10: u32 = 0x1C9;
    /// "Any pad button held" pseudo-key (`VK_PADANY`).
    pub const VK_PADANY: u32 = 0x1DF;
    /// Last KiriKiri pad virtual-key code (`VK_PAD_LAST`).
    pub const VK_PAD_LAST: u32 = 0x1DF;

    /// Every concrete pad button code (the direction pad plus pad buttons
    /// 1..10), in `tvpinputdefs.h` order. [`VK_PADANY`] is not included: it
    /// is a query pseudo-code, not a button.
    pub const PAD_CODES: [u32; 14] = [
        VK_PADLEFT,
        VK_PADUP,
        VK_PADRIGHT,
        VK_PADDOWN,
        VK_PAD1,
        VK_PAD2,
        VK_PAD3,
        VK_PAD4,
        VK_PAD5,
        VK_PAD6,
        VK_PAD7,
        VK_PAD8,
        VK_PAD9,
        VK_PAD10,
    ];

    /// Whether `code` lies in the KiriKiri pad virtual-key range
    /// (`VK_PAD_FIRST..=VK_PAD_LAST`), i.e. the reference
    /// `keycode >= VK_PAD_FIRST && keycode <= VK_PAD_LAST` branch of
    /// `TVPGetAsyncKeyState` (`SystemImpl.cpp:47`).
    pub fn is_pad_code(code: u32) -> bool {
        (VK_PAD_FIRST..=VK_PAD_LAST).contains(&code)
    }
}

pub use pad::{
    PAD_CODES, VK_PAD_FIRST, VK_PAD_LAST, VK_PAD1, VK_PAD2, VK_PAD3, VK_PAD4, VK_PAD5, VK_PAD6,
    VK_PAD7, VK_PAD8, VK_PAD9, VK_PAD10, VK_PADANY, VK_PADDOWN, VK_PADLEFT, VK_PADRIGHT, VK_PADUP,
};

/// Shift-state flag bits — port of the `TVP_SS_*` defines from
/// `reference/cpp/core/visual/tvpinputdefs.h:37`. These are the `shift`
/// argument of the `Window` `onMouseDown`/`onMouseUp`/`onMouseMove`/
/// `onMouseWheel`/`onKeyDown`/`onKeyUp` events.
pub mod shift_state {
    /// Shift key held (`TVP_SS_SHIFT`).
    pub const TVP_SS_SHIFT: u32 = 0x01;
    /// Alt/Menu key held (`TVP_SS_ALT`).
    pub const TVP_SS_ALT: u32 = 0x02;
    /// Control key held (`TVP_SS_CTRL`).
    pub const TVP_SS_CTRL: u32 = 0x04;
    /// Left mouse button held (`TVP_SS_LEFT`).
    pub const TVP_SS_LEFT: u32 = 0x08;
    /// Right mouse button held (`TVP_SS_RIGHT`).
    pub const TVP_SS_RIGHT: u32 = 0x10;
    /// Middle mouse button held (`TVP_SS_MIDDLE`).
    pub const TVP_SS_MIDDLE: u32 = 0x20;
    /// Event is the second click of a double-click (`TVP_SS_DOUBLE`).
    pub const TVP_SS_DOUBLE: u32 = 0x40;
    /// Event is an auto-repeat (`TVP_SS_REPEAT`).
    pub const TVP_SS_REPEAT: u32 = 0x80;

    /// `TVPIsAnyMouseButtonPressedInShiftStateFlags`
    /// (`reference/cpp/core/visual/tvpinputdefs.h:51`): whether any of the
    /// mouse-button bits is set.
    pub fn any_mouse_button_pressed(state: u32) -> bool {
        state & (TVP_SS_LEFT | TVP_SS_RIGHT | TVP_SS_MIDDLE | TVP_SS_DOUBLE) != 0
    }
}

pub use shift_state::{
    TVP_SS_ALT, TVP_SS_CTRL, TVP_SS_DOUBLE, TVP_SS_LEFT, TVP_SS_MIDDLE, TVP_SS_REPEAT,
    TVP_SS_RIGHT, TVP_SS_SHIFT,
};

/// The Windows virtual-key codes for the keyboard modifiers the shift-state
/// mask reports (`VK_SHIFT`/`VK_CONTROL`/`VK_MENU` from
/// `reference/cpp/core/environ/vkdefine.h`).
pub const VK_SHIFT: u32 = 0x10;
pub const VK_CONTROL: u32 = 0x11;
pub const VK_MENU: u32 = 0x12;

/// Frames within which a follow-up press counts as the next click of a
/// multi-click sequence (`Mouse.getClickCount`). At the usual 60 fps this
/// is ~500 ms, the Windows double-click time.
pub const MOUSE_CLICK_SEQUENCE_FRAMES: u64 = 30;
/// Maximum cursor movement (device pixels) between presses for them to
/// still count as one multi-click sequence.
pub const MOUSE_CLICK_MAX_MOVE: i32 = 4;

/// Mouse state shared with the app.
#[derive(Debug, Clone)]
pub struct MouseState {
    /// Cursor position (primary-layer / window logical coordinates), as
    /// reported by the app. `Mouse.getCursorX()` / `getCursorY()` read it;
    /// `Mouse.setCursorPos` writes it.
    pub x: i32,
    /// Cursor Y position — see [`MouseState::x`].
    pub y: i32,
    /// Accumulated wheel deltas `(x, y, z)` since the last
    /// `begin_frame`. `y` is the classic vertical wheel (one notch of a
    /// typical mouse is ±120), `x` a horizontal wheel, `z` a tilt axis.
    /// `Mouse.getWheelRot` reads `.1`, the X/Y/Z getters read the matching
    /// axis. Cleared by `begin_frame`.
    pub wheel: (i32, i32, i32),
    /// Per-button held state, indexed by the TVP button constants
    /// ([`MB_LEFT`]..[`MB_X2`]).
    pub buttons: Vec<bool>,
    /// Per-button "released this frame" flags, cleared by `begin_frame`.
    /// `Mouse.getReleased(button)` reads this.
    pub released: Vec<bool>,
    /// Per-button consecutive frames held, incremented by `end_frame`
    /// while the button stays down. `Mouse.getRepeat(button)` reads this.
    pub hold: Vec<u32>,
    /// Per-button count of presses in the current multi-click sequence
    /// (1 for a single click, 2 for a double-click, ...). Maintained by
    /// [`InputState::set_mouse_button`] on each rising edge; read through
    /// [`InputState::mouse_click_count`] / `Mouse.getClickCount`.
    pub click_count: Vec<u32>,
    /// Frame index of the last press of the current sequence
    /// (`u64::MAX` = never pressed); used to break the sequence on a timeout.
    pub last_click_frame: Vec<u64>,
    /// Cursor position of the last press of the current sequence; used to
    /// break the sequence on movement.
    pub last_click_pos: Vec<(i32, i32)>,
    /// Cursor visibility — `Mouse.isVisible()` / `Mouse.setVisible`.
    pub visible: bool,
    /// Pending OS-cursor warp requested by `Mouse.setCursorPos` (the host
    /// bridge consumes it via [`InputState::take_mouse_warp`]). `None` when
    /// no warp is pending. This is separate from [`MouseState::x`]/`y` so the
    /// state lookup is unconditional even if the host never applies it.
    pub warp_request: Option<(i32, i32)>,
}

impl Default for MouseState {
    fn default() -> Self {
        MouseState {
            x: 0,
            y: 0,
            wheel: (0, 0, 0),
            buttons: vec![false; MOUSE_BUTTONS],
            released: vec![false; MOUSE_BUTTONS],
            hold: vec![0; MOUSE_BUTTONS],
            click_count: vec![0; MOUSE_BUTTONS],
            last_click_frame: vec![u64::MAX; MOUSE_BUTTONS],
            last_click_pos: vec![(0, 0); MOUSE_BUTTONS],
            visible: true,
            warp_request: None,
        }
    }
}

/// Keyboard state shared with the app.
#[derive(Debug, Clone, Default)]
pub struct KeyState {
    /// keycode → consecutive frames the key has been held. Present while
    /// the key is down (the count starts at 0 on the press frame and is
    /// bumped by `end_frame`). `Key.getPressed(code)` is true iff the code
    /// is present.
    pub pressed: HashMap<u32, u32>,
    /// keycode → the held-frame count at the moment the key was released.
    /// Cleared by `begin_frame`, so an entry is visible for exactly one
    /// frame (the frame of the release). `Key.getReleased(code)` is true
    /// iff present.
    pub released: HashMap<u32, u32>,
    /// keycode → repeat count for keys currently held. Maintained by
    /// [`InputState::end_frame`] in lockstep with [`KeyState::pressed`]
    /// (same value while held; dropped together with `pressed` on release)
    /// and kept as a separate map so the wiring can treat it
    /// independently. `Key.getRepeat(code)` reads this; 0 when not held.
    pub repeat: HashMap<u32, u32>,
    /// Keycodes that transitioned to down since the last `begin_frame`, in
    /// press order (for click counting / `last_pressed` consumers).
    pub last_pressed: Vec<u32>,
}

/// The shared input state. The app writes to it every frame (see the crate
/// docs for the frame protocol); the `Mouse`/`Key` natives read it.
#[derive(Debug, Clone, Default)]
pub struct InputState {
    /// Mouse state.
    pub mouse: MouseState,
    /// Keyboard state.
    pub keys: KeyState,
    /// Monotonic frame counter, incremented by [`InputState::end_frame`].
    pub frame: u64,
}

impl InputState {
    /// Create a fresh, empty input state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin a new frame: clear everything that is only meaningful within
    /// one frame — the mouse release flags, the accumulated wheel deltas,
    /// the key `released` map and `last_pressed`. Call once at the top of
    /// the frame, after scripts have seen the previous frame's state.
    pub fn begin_frame(&mut self) {
        for r in &mut self.mouse.released {
            *r = false;
        }
        self.mouse.wheel = (0, 0, 0);
        self.keys.released.clear();
        self.keys.last_pressed.clear();
    }

    /// End the current frame: bump the held counters (`pressed`/`repeat`
    /// for held keys, `hold` for held mouse buttons) and advance
    /// [`InputState::frame`].
    pub fn end_frame(&mut self) {
        for (code, count) in &mut self.keys.pressed {
            *count += 1;
            self.keys.repeat.insert(*code, *count);
        }
        for (button, held) in self.mouse.buttons.iter().enumerate() {
            if *held {
                self.mouse.hold[button] += 1;
            }
        }
        self.frame += 1;
    }

    // -- mouse wiring API --------------------------------------------------

    /// Set the absolute cursor position (`Mouse.getCursorX/Y` read it).
    pub fn set_mouse_pos(&mut self, x: i32, y: i32) {
        self.mouse.x = x;
        self.mouse.y = y;
    }

    /// `Mouse.setCursorPos(x, y)`: move the logical cursor *and* queue an
    /// OS-cursor warp request for the host bridge
    /// ([`InputState::take_mouse_warp`]). The position is applied
    /// immediately so scripts that read `getCursorX/Y` back see it even when
    /// the host never consumes the warp.
    pub fn warp_mouse_pos(&mut self, x: i32, y: i32) {
        self.mouse.x = x;
        self.mouse.y = y;
        self.mouse.warp_request = Some((x, y));
    }

    /// Take (and clear) the pending OS-cursor warp requested by
    /// `Mouse.setCursorPos`, if any. Returns `None` when no warp is pending.
    pub fn take_mouse_warp(&mut self) -> Option<(i32, i32)> {
        self.mouse.warp_request.take()
    }

    /// Accumulate wheel deltas for this frame (`Mouse.getWheelRot*` read
    /// them; `begin_frame` resets them). Typical vertical notches are
    /// ±120.
    pub fn add_wheel(&mut self, x: i32, y: i32, z: i32) {
        self.mouse.wheel.0 += x;
        self.mouse.wheel.1 += y;
        self.mouse.wheel.2 += z;
    }

    /// Set a mouse button's held state. `button` is a TVP button index
    /// ([`MB_LEFT`]..[`MB_X2`]); out-of-range indices are ignored. A
    /// rising `down` edge resets the button's hold counter and advances the
    /// multi-click sequence; a falling edge sets its released-this-frame
    /// flag and clears the hold counter. Redundant calls (no state change)
    /// leave the hold counter intact. Returns false when `button` is out of
    /// range.
    pub fn set_mouse_button(&mut self, button: usize, down: bool) -> bool {
        if button >= MOUSE_BUTTONS {
            return false;
        }
        let was_down = self.mouse.buttons[button];
        if down == was_down {
            // No edge: keep the held-frame counter so a repeated "down"
            // report does not restart the repeat count.
            return true;
        }
        self.mouse.buttons[button] = down;
        if down {
            self.mouse.hold[button] = 0;
            // Count only a real rising edge as a click, and extend the
            // current multi-click sequence when it is close enough in time
            // and space (the reference `Mouse.getClickCount` gesture).
            let last_frame = self.mouse.last_click_frame[button];
            let (lx, ly) = self.mouse.last_click_pos[button];
            let close_in_time = last_frame != u64::MAX
                && self.frame.saturating_sub(last_frame) <= MOUSE_CLICK_SEQUENCE_FRAMES;
            let close_in_space = (self.mouse.x - lx).abs() <= MOUSE_CLICK_MAX_MOVE
                && (self.mouse.y - ly).abs() <= MOUSE_CLICK_MAX_MOVE;
            if close_in_time && close_in_space {
                self.mouse.click_count[button] = self.mouse.click_count[button].saturating_add(1);
            } else {
                self.mouse.click_count[button] = 1;
            }
            self.mouse.last_click_frame[button] = self.frame;
            self.mouse.last_click_pos[button] = (self.mouse.x, self.mouse.y);
        } else {
            self.mouse.released[button] = true;
            self.mouse.hold[button] = 0;
        }
        true
    }

    /// Set the cursor visibility (`Mouse.isVisible`/`setVisible`).
    pub fn set_mouse_visible(&mut self, visible: bool) {
        self.mouse.visible = visible;
    }

    // -- keyboard wiring API -----------------------------------------------

    /// Record that `code` went down this frame. A repeated `down` for an
    /// already-held key is a no-op (the held count keeps increasing).
    pub fn set_key_down(&mut self, code: u32) {
        if self.keys.pressed.contains_key(&code) {
            return;
        }
        self.keys.pressed.insert(code, 0);
        self.keys.last_pressed.push(code);
    }

    /// Record that `code` went up this frame. Moves the held-frame count
    /// into the `released` map (visible for one frame) and drops the
    /// `repeat` entry; releasing a key that was not held is a no-op.
    pub fn set_key_up(&mut self, code: u32) {
        if let Some(held) = self.keys.pressed.remove(&code) {
            self.keys.released.insert(code, held);
        }
        self.keys.repeat.remove(&code);
    }

    /// Whether `code` is currently held (`Key.getPressed`).
    pub fn is_key_down(&self, code: u32) -> bool {
        self.keys.pressed.contains_key(&code)
    }

    /// Whether `code` was released this frame (`Key.getReleased`).
    pub fn is_key_released(&self, code: u32) -> bool {
        self.keys.released.contains_key(&code)
    }

    /// Frames `code` has been held (`Key.getRepeat`); 0 when not held.
    pub fn key_repeat(&self, code: u32) -> u32 {
        self.keys.repeat.get(&code).copied().unwrap_or(0)
    }

    /// Whether `button` is currently held (`Mouse.getPressed`); false for
    /// out-of-range indices.
    pub fn is_mouse_button_down(&self, button: usize) -> bool {
        self.mouse.buttons.get(button).copied().unwrap_or(false)
    }

    /// Whether `button` was released this frame (`Mouse.getReleased`);
    /// false for out-of-range indices.
    pub fn is_mouse_button_released(&self, button: usize) -> bool {
        self.mouse.released.get(button).copied().unwrap_or(false)
    }

    /// Frames `button` has been held (`Mouse.getRepeat`); 0 for an
    /// out-of-range button or while the button is not held.
    pub fn mouse_button_repeat(&self, button: usize) -> u32 {
        if !self.mouse.buttons.get(button).copied().unwrap_or(false) {
            return 0;
        }
        self.mouse.hold.get(button).copied().unwrap_or(0)
    }

    // -- reference virtual-key lookup --------------------------------------

    /// Whether Windows virtual-key `code` is currently held, spanning the
    /// same code space as the reference `System.getKeyState`:
    ///
    /// * mouse-button VKs (`VK_LBUTTON`, `VK_RBUTTON`, `VK_MBUTTON`,
    ///   `VK_XBUTTON1/2`) map to the mouse-button state (the reference
    ///   scancode array is indexed by VK and covers them);
    /// * `VK_PADANY` is true when *any* concrete pad code is held
    ///   (`DInputMgn.cpp:696`, `bit = -1`);
    /// * everything else is a keyboard key from [`KeyState::pressed`].
    pub fn vk_pressed(&self, code: u32) -> bool {
        if let Some(b) = buttons::from_vk(code) {
            return self.mouse.buttons.get(b).copied().unwrap_or(false);
        }
        if code == VK_PADANY {
            return PAD_CODES
                .iter()
                .any(|&c| self.keys.pressed.contains_key(&c));
        }
        self.keys.pressed.contains_key(&code)
    }

    /// Whether virtual-key `code` was released this frame; same mapping as
    /// [`InputState::vk_pressed`].
    pub fn vk_released(&self, code: u32) -> bool {
        if let Some(b) = buttons::from_vk(code) {
            return self.mouse.released.get(b).copied().unwrap_or(false);
        }
        if code == VK_PADANY {
            return PAD_CODES
                .iter()
                .any(|&c| self.keys.released.contains_key(&c));
        }
        self.keys.released.contains_key(&code)
    }

    /// Held-frame count for virtual-key `code` (`Key.getRepeat`); 0 while the
    /// key/button is not held. `VK_PADANY` reports the longest-held pad
    /// button.
    pub fn vk_repeat(&self, code: u32) -> u32 {
        if let Some(b) = buttons::from_vk(code) {
            return self.mouse_button_repeat(b);
        }
        if code == VK_PADANY {
            return PAD_CODES
                .iter()
                .map(|&c| self.keys.repeat.get(&c).copied().unwrap_or(0))
                .max()
                .unwrap_or(0);
        }
        self.keys.repeat.get(&code).copied().unwrap_or(0)
    }

    /// The reference `TVP_SS_*` shift mask for the currently held state —
    /// the `shift` argument of the `Window` `onMouseDown`/`onMouseUp`/
    /// `onMouseMove`/`onMouseWheel`/`onKeyDown`/`onKeyUp` events. Sets the
    /// held-modifier and held-mouse-button bits; `TVP_SS_DOUBLE` and
    /// `TVP_SS_REPEAT` are event-specific and are left to the caller
    /// (e.g. from [`InputState::mouse_click_count`] and
    /// [`InputState::vk_repeat`]).
    pub fn shift_flags(&self) -> u32 {
        let mut flags = 0;
        if self.vk_pressed(VK_SHIFT) {
            flags |= TVP_SS_SHIFT;
        }
        if self.vk_pressed(VK_MENU) {
            flags |= TVP_SS_ALT;
        }
        if self.vk_pressed(VK_CONTROL) {
            flags |= TVP_SS_CTRL;
        }
        if self.is_mouse_button_down(MB_LEFT) {
            flags |= TVP_SS_LEFT;
        }
        if self.is_mouse_button_down(MB_RIGHT) {
            flags |= TVP_SS_RIGHT;
        }
        if self.is_mouse_button_down(MB_MIDDLE) {
            flags |= TVP_SS_MIDDLE;
        }
        flags
    }

    /// The number of presses in the current multi-click sequence for
    /// `button` (`Mouse.getClickCount`): 1 for a single click, 2 for a
    /// double-click, and so on. Returns 0 for out-of-range buttons or once
    /// the sequence has timed out (no click within
    /// [`MOUSE_CLICK_SEQUENCE_FRAMES`]).
    pub fn mouse_click_count(&self, button: usize) -> u32 {
        if button >= MOUSE_BUTTONS {
            return 0;
        }
        let last = self.mouse.last_click_frame[button];
        if last == u64::MAX || self.frame.saturating_sub(last) > MOUSE_CLICK_SEQUENCE_FRAMES {
            0
        } else {
            self.mouse.click_count[button]
        }
    }
}

// ---------------------------------------------------------------------------
// Process-global context
// ---------------------------------------------------------------------------

/// The process-wide input state slot. The app installs its instance with
/// [`set_context`]; when none is installed, [`input_state`] creates a
/// default. (`OnceLock<Mutex<Option<...>>>` because `set_context(None)`
/// must be able to clear the slot between tests.)
static INPUT_CONTEXT: OnceLock<Mutex<Option<Arc<Mutex<InputState>>>>> = OnceLock::new();

/// Install (or clear, with `None`) the shared input state the natives read.
///
/// The app calls this once at startup with the `Arc<Mutex<InputState>>` it
/// writes to every frame; passing `None` resets to the default-created
/// state. Tests install a fresh state per test so they do not leak input
/// between cases.
pub fn set_context(state: Option<Arc<Mutex<InputState>>>) {
    let cell = INPUT_CONTEXT.get_or_init(|| Mutex::new(None));
    *lock_ok(cell) = state;
}

/// The shared input state, creating a fresh default instance when none was
/// installed. The returned `Arc` is process-global (the natives read the
/// same instance), so it is safe to lock from any thread — though the VM
/// thread is the only one that ever runs the natives.
pub fn input_state() -> Arc<Mutex<InputState>> {
    let cell = INPUT_CONTEXT.get_or_init(|| Mutex::new(None));
    let mut guard = lock_ok(cell);
    let arc = guard.get_or_insert_with(|| Arc::new(Mutex::new(InputState::new())));
    Arc::clone(arc)
}

/// Run `f` with the shared input state locked (read or write). Recovers
/// from a poisoned mutex instead of panicking across the FFI boundary.
pub(crate) fn with_state<T>(f: impl FnOnce(&mut InputState) -> T) -> T {
    let state = input_state();
    let mut guard = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&mut guard)
}

// ---------------------------------------------------------------------------
// Shared native-callback helpers (same pattern as tvp-natives)
// ---------------------------------------------------------------------------

thread_local! {
    /// Scratch buffer for string return values (`out.string`). Stays valid
    /// until the next native call on this thread, which is long enough: the
    /// C++ trampoline copies the string immediately after the callback
    /// returns.
    static STRING_OUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// View the `argc` callback arguments as a slice. Returns an empty slice
/// for a null pointer / zero count (the C++ side passes a null `argv` when
/// a method is called without arguments).
pub(crate) fn args<'a>(argv: *const Value, argc: c_int) -> &'a [Value] {
    if argc <= 0 || argv.is_null() {
        return &[];
    }
    // SAFETY: the C++ trampoline guarantees `argc` valid Value entries at
    // `argv` for the duration of the call.
    unsafe { slice::from_raw_parts(argv, argc as usize) }
}

/// Convert a callback argument to a boolean (TJS `operator bool`
/// semantics): integers/reals are non-zero, strings are non-empty, void is
/// false, objects are true.
pub(crate) fn value_as_bool(v: &Value) -> bool {
    match v.ty {
        VAL_INTEGER => v.integer != 0,
        VAL_REAL => v.real != 0.0,
        VAL_STRING => !value_as_string(v).is_empty(),
        VAL_VOID => false,
        _ => true,
    }
}

/// Convert a callback argument to an integer (TJS `AsInteger` semantics).
pub(crate) fn value_as_i64(v: &Value) -> i64 {
    match v.ty {
        VAL_INTEGER => v.integer,
        VAL_REAL => v.real as i64,
        VAL_STRING => value_as_string(v).parse().unwrap_or(0),
        _ => 0,
    }
}

/// Convert a callback argument to its string form (strings pass through,
/// integers/reals use their decimal representation, void and objects
/// become `""`).
fn value_as_string(v: &Value) -> String {
    match v.ty {
        VAL_STRING if v.string.is_null() => String::new(),
        VAL_STRING => {
            // SAFETY: the C++ side guarantees a NUL-terminated UTF-8 string
            // valid for the duration of the call.
            let s = unsafe { CStr::from_ptr(v.string) };
            s.to_string_lossy().into_owned()
        }
        VAL_INTEGER => v.integer.to_string(),
        VAL_REAL => v.real.to_string(),
        _ => String::new(),
    }
}

/// Write `s` into `*out` as a string return value.
pub(crate) fn set_string_out(out: *mut Value, s: &str) {
    STRING_OUT.with(|buf| {
        let mut buf = buf.borrow_mut();
        buf.clear();
        buf.extend_from_slice(s.as_bytes());
        buf.push(0);
        // SAFETY: out is a valid return slot for the duration of the call.
        unsafe {
            (*out).ty = VAL_STRING;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = buf.as_ptr() as *const c_char;
        }
    });
}

/// Write an integer return value into `*out`.
pub(crate) fn set_int_out(out: *mut Value, v: i64) {
    // SAFETY: out is a valid return slot for the duration of the call.
    unsafe {
        (*out).ty = VAL_INTEGER;
        (*out).integer = v;
        (*out).real = 0.0;
        (*out).string = ptr::null();
    }
}

/// Clear the return slot (void return, matching the reference's
/// `result->Clear()`).
pub(crate) fn set_void_out(out: *mut Value) {
    // SAFETY: out is a valid return slot for the duration of the call.
    unsafe {
        (*out).ty = VAL_VOID;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = ptr::null();
    }
}

/// Report a native error: point `*out_error` at a malloc'd NUL-terminated
/// message (the C++ side frees it with `tjs2_free_string`) and return the
/// non-zero status code.
pub(crate) fn report_error(out_error: *mut *mut c_char, msg: &str) -> c_int {
    // SAFETY: out_error points at a valid char* slot for the duration of
    // the call.
    unsafe { *out_error = alloc_error_string(msg) };
    1
}

/// Build a malloc'd NUL-terminated UTF-8 error message (freed on the C++
/// side with `tjs2_free_string`).
fn alloc_error_string(msg: &str) -> *mut c_char {
    let bytes = msg.as_bytes();
    // SAFETY: tjs2_malloc is malloc-compatible; we write a NUL-terminated
    // copy and the C++ trampoline frees it with tjs2_free_string.
    unsafe {
        let buf = tjs2_malloc(bytes.len() + 1) as *mut u8;
        if buf.is_null() {
            return ptr::null_mut();
        }
        ptr::copy_nonoverlapping(bytes.as_ptr(), buf, bytes.len());
        *buf.add(bytes.len()) = 0;
        buf as *mut c_char
    }
}

/// Lock a global mutex, recovering from poisoning (a previous panic while
/// holding it) instead of propagating the poison error across the FFI
/// boundary (a panic on the VM thread aborts the process).
pub(crate) fn lock_ok<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    /// `input_state` is process-global; parallel tests race on it (segfault).
    /// Serialize the crate's tests with one lock; all other crates stay
    /// fully parallel.
    static VM_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn vm_lock() -> std::sync::MutexGuard<'static, ()> {
        VM_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    use super::*;
    use tjs2_sys::TjsValue;

    /// Fresh engine with `Mouse`/`Key` registered and a fresh, installed
    /// input state (tests must run single-threaded — the state is
    /// process-global).
    fn test_engine() -> (Box<Tjs2Engine>, Arc<Mutex<InputState>>) {
        // Box the engine so its address is stable for the natives that call
        // back into `Tjs2Engine::set_member` (`context_engine`).
        let e = Box::new(Tjs2Engine::new().expect("create engine"));
        register_all(&e).expect("register Mouse + Key");
        let state = Arc::new(Mutex::new(InputState::new()));
        set_context(Some(state.clone()));
        (e, state)
    }

    fn eval_i(e: &Tjs2Engine, expr: &str) -> i64 {
        match e.eval(expr, "test").expect("eval succeeds") {
            TjsValue::Integer(v) => v,
            other => panic!("expected Integer for {expr:?}, got {other:?}"),
        }
    }

    // -- Test 1: simulated input is read back ------------------------------

    #[test]
    fn mouse_and_key_read_back_simulated_input() {
        let _vm_lock = vm_lock();
        let (e, state) = test_engine();
        {
            let mut s = state.lock().unwrap();
            s.begin_frame();
            s.set_mouse_pos(300, 200);
            s.set_mouse_button(MB_LEFT, true);
            s.set_key_down(key_code("kReturn").expect("kReturn in table"));
            s.end_frame();
        }

        assert_eq!(eval_i(&e, "Mouse.getCursorX()"), 300);
        assert_eq!(eval_i(&e, "Mouse.getCursorY()"), 200);
        assert_eq!(
            e.eval("Mouse.getCursorPos()", "test").unwrap(),
            TjsValue::String("300,200".into())
        );
        assert_eq!(eval_i(&e, "Mouse.getPressed(0)"), 1); // mbLeft
        assert_eq!(eval_i(&e, "Key.getPressed(Key.kReturn)"), 1);
        assert_eq!(eval_i(&e, "Key.kReturn"), 0x0D);
    }

    // -- Test 2: released / repeat semantics -------------------------------

    #[test]
    fn key_released_and_repeat_semantics() {
        let _vm_lock = vm_lock();
        let (e, state) = test_engine();
        let code = key_code("kReturn").unwrap();

        // frame N: press; after the frame the key has been held 1 frame.
        {
            let mut s = state.lock().unwrap();
            s.begin_frame();
            s.set_key_down(code);
            s.end_frame();
        }
        assert_eq!(eval_i(&e, "Key.getPressed(Key.kReturn)"), 1);
        assert_eq!(eval_i(&e, "Key.getRepeat(Key.kReturn)"), 1);

        // frame N+1: still held — the repeat count increments.
        {
            let mut s = state.lock().unwrap();
            s.begin_frame();
            s.end_frame();
        }
        assert_eq!(eval_i(&e, "Key.getPressed(Key.kReturn)"), 1);
        assert_eq!(eval_i(&e, "Key.getRepeat(Key.kReturn)"), 2);

        // frame N+2: release — getReleased is true, getPressed false.
        {
            let mut s = state.lock().unwrap();
            s.begin_frame();
            s.set_key_up(code);
            s.end_frame();
        }
        assert_eq!(eval_i(&e, "Key.getPressed(Key.kReturn)"), 0);
        assert_eq!(eval_i(&e, "Key.getReleased(Key.kReturn)"), 1);

        // frame N+3: nothing — the release flag is gone.
        {
            let mut s = state.lock().unwrap();
            s.begin_frame();
            s.end_frame();
        }
        assert_eq!(eval_i(&e, "Key.getReleased(Key.kReturn)"), 0);
        assert_eq!(eval_i(&e, "Key.getRepeat(Key.kReturn)"), 0);
    }

    // -- Test 3: visible toggle + wheel accumulation ------------------------

    #[test]
    fn mouse_visible_toggle_and_wheel_accumulation() {
        let _vm_lock = vm_lock();
        let (e, state) = test_engine();

        // visible defaults to true; setVisible toggles the shared state.
        assert_eq!(eval_i(&e, "Mouse.isVisible()"), 1);
        assert_eq!(
            e.eval("Mouse.setVisible(false)", "test").unwrap(),
            TjsValue::Void
        );
        assert_eq!(eval_i(&e, "Mouse.isVisible()"), 0);
        assert_eq!(
            e.eval("Mouse.setVisible(true)", "test").unwrap(),
            TjsValue::Void
        );
        assert_eq!(eval_i(&e, "Mouse.isVisible()"), 1);

        // wheel deltas accumulate across events within a frame.
        {
            let mut s = state.lock().unwrap();
            s.add_wheel(0, 120, 0);
            s.add_wheel(0, 60, 0);
            s.add_wheel(1, 0, -2);
        }
        assert_eq!(eval_i(&e, "Mouse.getWheelRot()"), 180);
        assert_eq!(eval_i(&e, "Mouse.getWheelRotX()"), 1);
        assert_eq!(eval_i(&e, "Mouse.getWheelRotY()"), 180);
        assert_eq!(eval_i(&e, "Mouse.getWheelRotZ()"), -2);

        // begin_frame resets the accumulation.
        {
            let mut s = state.lock().unwrap();
            s.begin_frame();
        }
        assert_eq!(eval_i(&e, "Mouse.getWheelRot()"), 0);

        // no click happened, so getClickCount is 0.
        assert_eq!(eval_i(&e, "Mouse.getClickCount(0)"), 0);
    }

    // -- getClickCount: single/double click sequences and timeout ------------

    #[test]
    fn mouse_click_count_tracks_multi_click_sequences() {
        let _vm_lock = vm_lock();
        let (e, state) = test_engine();

        // Frame 0: a single left click at (10, 10).
        {
            let mut s = state.lock().unwrap();
            s.begin_frame();
            s.set_mouse_pos(10, 10);
            s.set_mouse_button(MB_LEFT, true);
            s.end_frame();
        }
        assert_eq!(eval_i(&e, "Mouse.getClickCount(0)"), 1);

        // Next frame: release then click again at the same spot within the
        // window -> double click.
        {
            let mut s = state.lock().unwrap();
            s.begin_frame();
            s.set_mouse_button(MB_LEFT, false);
            s.set_mouse_button(MB_LEFT, true);
            s.end_frame();
        }
        assert_eq!(eval_i(&e, "Mouse.getClickCount(0)"), 2);

        // A click far away starts a fresh sequence.
        {
            let mut s = state.lock().unwrap();
            s.begin_frame();
            s.set_mouse_button(MB_LEFT, false);
            s.set_mouse_pos(500, 500);
            s.set_mouse_button(MB_LEFT, true);
            s.end_frame();
        }
        assert_eq!(eval_i(&e, "Mouse.getClickCount(0)"), 1);

        // Idle past the sequence window: the count decays to 0.
        {
            let mut s = state.lock().unwrap();
            for _ in 0..=MOUSE_CLICK_SEQUENCE_FRAMES {
                s.begin_frame();
                s.end_frame();
            }
        }
        assert_eq!(eval_i(&e, "Mouse.getClickCount(0)"), 0);

        // Out-of-range buttons return 0 (documented deviation).
        assert_eq!(eval_i(&e, "Mouse.getClickCount(99)"), 0);
        assert_eq!(eval_i(&e, "Mouse.getClickCount(-1)"), 0);
    }

    // -- Test 4: unknown key codes are false, not errors --------------------

    #[test]
    fn unknown_key_codes_return_false_not_error() {
        let _vm_lock = vm_lock();
        let (e, _state) = test_engine();
        assert_eq!(eval_i(&e, "Key.getPressed(0x9999)"), 0);
        assert_eq!(eval_i(&e, "Key.getReleased(0x9999)"), 0);
        assert_eq!(eval_i(&e, "Key.getRepeat(0x9999)"), 0);
        // out-of-range / negative codes behave the same (no TJS error)
        assert_eq!(eval_i(&e, "Key.getPressed(-1)"), 0);
        assert_eq!(eval_i(&e, "Key.getPressed(256)"), 0);
        // releasing a key that was never pressed is a no-op, not an error
        assert_eq!(eval_i(&e, "Key.getReleased(0x41)"), 0);
    }

    // -- mouse buttons: out-of-range is false, not an error -----------------

    #[test]
    fn mouse_button_out_of_range_returns_false() {
        let _vm_lock = vm_lock();
        let (e, _state) = test_engine();
        assert_eq!(eval_i(&e, "Mouse.getPressed(5)"), 0);
        assert_eq!(eval_i(&e, "Mouse.getPressed(-1)"), 0);
        assert_eq!(eval_i(&e, "Mouse.getReleased(99)"), 0);
        assert_eq!(eval_i(&e, "Mouse.getRepeat(99)"), 0);
    }

    // -- setCursorPos writes back into the state ----------------------------

    #[test]
    fn mouse_set_cursor_pos_writes_state() {
        let _vm_lock = vm_lock();
        let (e, state) = test_engine();
        assert_eq!(
            e.eval("Mouse.setCursorPos(50, 60)", "test").unwrap(),
            TjsValue::Void
        );
        assert_eq!(eval_i(&e, "Mouse.getCursorX()"), 50);
        assert_eq!(eval_i(&e, "Mouse.getCursorY()"), 60);
        assert_eq!(
            state.lock().unwrap().mouse.x,
            50,
            "the shared state must reflect Mouse.setCursorPos"
        );
        // setCursorPos also queues an OS-cursor warp for the host bridge.
        assert_eq!(
            state.lock().unwrap().take_mouse_warp(),
            Some((50, 60)),
            "Mouse.setCursorPos must queue a cursor warp"
        );
        assert_eq!(
            state.lock().unwrap().take_mouse_warp(),
            None,
            "the warp request is consumed once"
        );
        // the arg-count check matches the reference (TJS_E_BADPARAMCOUNT)
        assert!(e.eval("Mouse.setCursorPos(1)", "test").is_err());
    }

    // -- getCursorPos tolerates an (unfillable) object argument -------------

    #[test]
    fn mouse_get_cursor_pos_fills_an_object_argument() {
        let _vm_lock = vm_lock();
        let (e, state) = test_engine();
        state.lock().unwrap().set_mouse_pos(7, 9);

        // Object form: fill obj.x / obj.y in place and return void.
        e.exec_script("global.__pos = %[x:0, y:0];", "test")
            .expect("define pos");
        assert_eq!(
            e.eval("Mouse.getCursorPos(global.__pos)", "test").unwrap(),
            TjsValue::Void,
            "Mouse.getCursorPos(obj) returns void"
        );
        assert_eq!(eval_i(&e, "global.__pos.x"), 7);
        assert_eq!(eval_i(&e, "global.__pos.y"), 9);
        // set_member uses TJS_MEMBERENSURE: a missing member is created.
        e.exec_script("global.__empty = %[];", "test").unwrap();
        e.eval("Mouse.getCursorPos(global.__empty)", "test")
            .unwrap();
        assert_eq!(eval_i(&e, "global.__empty.x"), 7);
        assert_eq!(eval_i(&e, "global.__empty.y"), 9);

        // No-argument form returns the "x,y" string.
        assert_eq!(
            e.eval("Mouse.getCursorPos()", "test").unwrap(),
            TjsValue::String("7,9".into())
        );
        // A non-object argument falls back to the string form.
        assert_eq!(
            e.eval("Mouse.getCursorPos(123)", "test").unwrap(),
            TjsValue::String("7,9".into())
        );
    }

    // -- mouse hold/repeat resets on release, redundant down keeps it -------

    #[test]
    fn mouse_repeat_resets_on_release_and_ignores_redundant_down() {
        let _vm_lock = vm_lock();
        let (e, state) = test_engine();

        // frame 1: press; end_frame bumps hold to 1.
        {
            let mut s = state.lock().unwrap();
            s.begin_frame();
            s.set_mouse_button(MB_LEFT, true);
            s.end_frame();
        }
        assert_eq!(eval_i(&e, "Mouse.getRepeat(0)"), 1);

        // frame 2: a redundant down (same state) must not reset the count;
        // end_frame bumps it to 2.
        {
            let mut s = state.lock().unwrap();
            s.begin_frame();
            s.set_mouse_button(MB_LEFT, true);
            s.end_frame();
        }
        assert_eq!(eval_i(&e, "Mouse.getRepeat(0)"), 2);

        // frame 3: release — repeat drops to 0 immediately.
        {
            let mut s = state.lock().unwrap();
            s.begin_frame();
            s.set_mouse_button(MB_LEFT, false);
            s.end_frame();
        }
        assert_eq!(eval_i(&e, "Mouse.getRepeat(0)"), 0);
        assert_eq!(eval_i(&e, "Mouse.getReleased(0)"), 1);
        assert_eq!(state.lock().unwrap().mouse_button_repeat(MB_LEFT), 0);

        // a spurious release (button already up) is not a release edge.
        {
            let mut s = state.lock().unwrap();
            s.begin_frame();
            s.set_mouse_button(MB_LEFT, false);
            s.end_frame();
        }
        assert_eq!(eval_i(&e, "Mouse.getReleased(0)"), 0);
    }

    // -- mouse-button VKs resolve through Key (reference scancode array) ----

    #[test]
    fn key_resolves_mouse_button_vks() {
        let _vm_lock = vm_lock();
        let (e, state) = test_engine();
        {
            let mut s = state.lock().unwrap();
            s.begin_frame();
            s.set_mouse_button(MB_LEFT, true);
            s.end_frame();
        }
        // The reference scancode array is indexed by VK and covers the mouse
        // buttons, so System.getKeyState(VK_LBUTTON) sees them; Key mirrors it.
        assert_eq!(eval_i(&e, "Key.getPressed(Key.kLButton)"), 1);
        assert_eq!(eval_i(&e, "Key.getPressed(Key.kRButton)"), 0);
        assert_eq!(eval_i(&e, "Key.kLButton"), 0x01);
        assert_eq!(eval_i(&e, "Key.kCancel"), 0x03);

        {
            let mut s = state.lock().unwrap();
            s.begin_frame();
            s.set_mouse_button(MB_LEFT, false);
            s.end_frame();
        }
        assert_eq!(eval_i(&e, "Key.getReleased(Key.kLButton)"), 1);
    }

    // -- gamepad VK_PAD and VK_PADANY aggregation ---------------------------

    #[test]
    fn key_aggregates_gamepad_buttons_and_pad_any() {
        let _vm_lock = vm_lock();
        let (e, state) = test_engine();
        {
            let mut s = state.lock().unwrap();
            s.begin_frame();
            // feed two concrete pad codes like the host bridge would
            s.set_key_down(VK_PAD1);
            s.set_key_down(VK_PADUP);
            s.end_frame();
        }
        assert_eq!(eval_i(&e, "Key.getPressed(Key.kPad1)"), 1);
        assert_eq!(eval_i(&e, "Key.getPressed(Key.kPadUp)"), 1);
        assert_eq!(eval_i(&e, "Key.getPressed(Key.kPad2)"), 0);
        // VK_PADANY is true while any pad button is held (DInputMgn.cpp:696).
        assert_eq!(eval_i(&e, "Key.getPressed(Key.kPadAny)"), 1);
        assert_eq!(eval_i(&e, "Key.getRepeat(Key.kPadAny)"), 1);
        assert_eq!(eval_i(&e, "Key.kPadAny"), 0x1DF);
        assert_eq!(eval_i(&e, "Key.kPad1"), 0x1C0);

        {
            let mut s = state.lock().unwrap();
            s.begin_frame();
            s.set_key_up(VK_PAD1);
            s.set_key_up(VK_PADUP);
            s.end_frame();
        }
        assert_eq!(eval_i(&e, "Key.getPressed(Key.kPadAny)"), 0);
        assert_eq!(eval_i(&e, "Key.getReleased(Key.kPadAny)"), 1);
    }

    // -- key constants ------------------------------------------------------

    #[test]
    fn key_constants_evaluate_to_reference_values() {
        let _vm_lock = vm_lock();
        let (e, _state) = test_engine();
        for (name, expected) in [
            ("kBack", 0x08),
            ("kTab", 0x09),
            ("kReturn", 0x0D),
            ("kShift", 0x10),
            ("kControl", 0x11),
            ("kMenu", 0x12),
            ("kEscape", 0x1B),
            ("kSpace", 0x20),
            ("kLeft", 0x25),
            ("kUp", 0x26),
            ("kRight", 0x27),
            ("kDown", 0x28),
            ("kA", 0x41),
            ("kZ", 0x5A),
            ("k0", 0x30),
            ("k9", 0x39),
            ("kNumPad0", 0x60),
            ("kDivide", 0x6F),
            ("kF1", 0x70),
            ("kF24", 0x87),
            ("kLShift", 0xA0),
            ("kRMenu", 0xA5),
            ("kOEMClear", 0xFE),
        ] {
            assert_eq!(
                eval_i(&e, &format!("Key.{name}")),
                expected as i64,
                "Key.{name}"
            );
        }
        // the Rust lookup agrees with the registered properties
        assert_eq!(key_code("kReturn"), Some(0x0D));
        assert_eq!(key_code("kNoSuchKey"), None);
    }

    #[test]
    fn every_registered_key_constant_evaluates_to_its_table_value() {
        let _vm_lock = vm_lock();
        let (e, _state) = test_engine();
        for &(name, code) in KEY_CODE_TABLE {
            assert_eq!(
                eval_i(&e, &format!("Key.{name}")),
                i64::from(code),
                "Key.{name} must equal 0x{code:X}"
            );
        }
    }

    // -- bridge-fed edges: button press -> hold -> release, pos, key edges ---

    #[test]
    fn bridge_edges_pos_and_wheel_land_in_state() {
        let _vm_lock = vm_lock();
        let (_e, state) = test_engine();

        // frame 1: the bridge records a left-button press edge, the cursor
        // position, a key press, and a wheel notch.
        {
            let mut s = state.lock().unwrap();
            s.begin_frame();
            s.set_mouse_pos(640, 360);
            s.set_mouse_button(MB_LEFT, true);
            s.set_key_down(key_code("kReturn").unwrap());
            s.add_wheel(0, 120, 0);
            s.end_frame();
        }
        {
            let s = state.lock().unwrap();
            // position landed
            assert_eq!((s.mouse.x, s.mouse.y), (640, 360));
            // button down edge -> held, hold counter reset to 0, then
            // end_frame bumped it to 1
            assert!(s.is_mouse_button_down(MB_LEFT));
            assert!(!s.is_mouse_button_released(MB_LEFT));
            assert_eq!(s.mouse_button_repeat(MB_LEFT), 1);
            // key down edge lands; repeat bumped to 1 by end_frame
            assert!(s.is_key_down(key_code("kReturn").unwrap()));
            assert_eq!(s.key_repeat(key_code("kReturn").unwrap()), 1);
            // wheel accumulated
            assert_eq!(s.mouse.wheel, (0, 120, 0));
        }

        // frame 2: still held (no new down edge), wheel cleared; a release
        // edge arrives.
        {
            let mut s = state.lock().unwrap();
            s.begin_frame();
            s.set_mouse_button(MB_LEFT, false);
            s.set_key_up(key_code("kReturn").unwrap());
            s.end_frame();
        }
        {
            let s = state.lock().unwrap();
            // wheel was cleared by begin_frame
            assert_eq!(s.mouse.wheel, (0, 0, 0));
            // release edge visible this frame only
            assert!(s.is_mouse_button_released(MB_LEFT));
            assert!(!s.is_mouse_button_down(MB_LEFT));
            assert!(s.is_key_released(key_code("kReturn").unwrap()));
            assert!(!s.is_key_down(key_code("kReturn").unwrap()));
        }

        // frame 3: the release flag is gone (single-frame gatting).
        {
            let mut s = state.lock().unwrap();
            s.begin_frame();
            s.end_frame();
        }
        {
            let s = state.lock().unwrap();
            assert!(!s.is_mouse_button_released(MB_LEFT));
            assert!(!s.is_key_released(key_code("kReturn").unwrap()));
        }
    }

    // -- last_pressed / hold counters at the Rust level ---------------------

    #[test]
    fn last_pressed_records_press_order() {
        let _vm_lock = vm_lock();
        let (e, state) = test_engine();
        let (k_left, k_right) = (key_code("kLeft").unwrap(), key_code("kRight").unwrap());

        // frame 1: two fresh presses, in order.
        {
            let mut s = state.lock().unwrap();
            s.begin_frame();
            s.set_key_down(k_left);
            s.set_key_down(k_right);
            s.end_frame();
        }
        assert_eq!(
            state.lock().unwrap().keys.last_pressed,
            vec![k_left, k_right]
        );
        // the held count increments each frame while held.
        assert_eq!(state.lock().unwrap().keys.pressed.get(&k_left), Some(&1));

        // frame 2: pressing an already-held key is a no-op (no new edge).
        {
            let mut s = state.lock().unwrap();
            s.begin_frame();
            s.set_key_down(k_left);
            s.end_frame();
        }
        assert!(state.lock().unwrap().keys.last_pressed.is_empty());
        assert_eq!(state.lock().unwrap().keys.pressed.get(&k_left), Some(&2));
        assert_eq!(eval_i(&e, "Key.getRepeat(Key.kLeft)"), 2);

        // frame 3: release, then press again → a fresh edge, count resets.
        {
            let mut s = state.lock().unwrap();
            s.begin_frame();
            s.set_key_up(k_left);
            s.set_key_down(k_left);
            s.end_frame();
        }
        assert_eq!(state.lock().unwrap().keys.last_pressed, vec![k_left]);
        assert_eq!(state.lock().unwrap().keys.pressed.get(&k_left), Some(&1));
        assert_eq!(eval_i(&e, "Key.getRepeat(Key.kLeft)"), 1);
    }

    // -- reference TVP_SS_* shift-state mask ---------------------------------

    #[test]
    fn shift_flags_compose_modifiers_and_buttons() {
        let _vm_lock = vm_lock();
        let (_e, state) = test_engine();
        {
            let mut s = state.lock().unwrap();
            s.begin_frame();
            s.set_key_down(VK_SHIFT);
            s.set_key_down(VK_CONTROL);
            s.set_mouse_button(MB_LEFT, true);
            s.end_frame();
        }
        let s = state.lock().unwrap();
        let flags = s.shift_flags();
        assert_eq!(flags & TVP_SS_SHIFT, TVP_SS_SHIFT);
        assert_eq!(flags & TVP_SS_CTRL, TVP_SS_CTRL);
        assert_eq!(flags & TVP_SS_ALT, 0);
        assert_eq!(flags & TVP_SS_LEFT, TVP_SS_LEFT);
        assert_eq!(flags & TVP_SS_RIGHT, 0);
        assert!(shift_state::any_mouse_button_pressed(flags));
        assert!(!shift_state::any_mouse_button_pressed(
            TVP_SS_SHIFT | TVP_SS_ALT
        ));
        // the flag values match tvpinputdefs.h
        assert_eq!(TVP_SS_SHIFT, 0x01);
        assert_eq!(TVP_SS_ALT, 0x02);
        assert_eq!(TVP_SS_CTRL, 0x04);
        assert_eq!(TVP_SS_LEFT, 0x08);
        assert_eq!(TVP_SS_RIGHT, 0x10);
        assert_eq!(TVP_SS_MIDDLE, 0x20);
        assert_eq!(TVP_SS_DOUBLE, 0x40);
        assert_eq!(TVP_SS_REPEAT, 0x80);
    }
}
