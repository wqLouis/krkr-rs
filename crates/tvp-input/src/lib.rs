//! tvp-input — shared input state + the `Mouse` and `Key` TJS natives.
//!
//! This crate defines the **input state** the whole engine reads from and
//! writes to, and registers the two input native classes KiriKiri scripts
//! call: `Mouse` and `Key`.
//!
//! The state is **not** fed by OS input capture here — the app (Bevy
//! wiring, a later wave) writes into the shared state every frame; the
//! natives only read it when scripts call them. The app drives the state
//! with the frame protocol below.
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
//! `Mouse` (static class; port of the reference `Mouse` class, which lives
//! in `reference/cpp/core/base/MouseIntf.cpp` in the full krkrz tree — not
//! present in this repository's reference subset, so the surface below
//! follows the well-known krkrz API):
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
//! | `getClickCount(button)` | 0 (stub; click counting is not implemented) |
//!
//! Buttons are indexed by the TVP button constants from
//! `reference/cpp/core/visual/tvpinputdefs.h`
//! (`enum tTVPMouseButton { mbLeft, mbRight, mbMiddle, mbX1, mbX2 }`):
//! [`MB_LEFT`]=0, [`MB_RIGHT`]=1, [`MB_MIDDLE`]=2, [`MB_X1`]=3, [`MB_X2`]=4.
//!
//! `Key` (static class; port of the reference `Key` class, normally
//! `reference/cpp/core/base/KeyIntf.cpp`):
//!
//! | member | returns |
//! |---|---|
//! | `getPressed(code)` | bool — whether the key is currently held |
//! | `getReleased(code)` | bool — whether the key was released this frame |
//! | `getRepeat(code)` | frames the key has been held (increments while
//!   held, 0 when not held) |
//! | `kBack`, `kTab`, `kReturn`, `kShift`, `kControl`, `kMenu`,
//!   `kEscape`, `kSpace`, `kLeft`, `kUp`, `kRight`, `kDown`, `kA`..`kZ`,
//!   `k0`..`k9`, `kF1`..`kF24`, `kNumPad0`..`kNumPad9`, ... | get-only
//!   constant properties: the Windows virtual-key code (see
//!   [`KEY_CODE_TABLE`]) |
//!
//! Key codes are Windows virtual-key codes (`VK_*`), the same codes
//! `System.getKeyState` uses — see
//! `reference/cpp/core/environ/vkdefine.h`.
//!
//! # Deviations from the reference (documented)
//!
//! * `Mouse.getCursorPos` in the reference takes an object argument and
//!   fills its `x`/`y` properties. The C ABI in `tjs2-sys` cannot marshal
//!   TJS objects, so this port returns the position as a `"x,y"` string
//!   instead.
//! * The reference's `Key.getPressed` returns void when the key is up and
//!   `TVPKeyRepeatCount / 2` when it is down (a press "ticker"); this port
//!   returns a plain bool (0/1). `Key.getRepeat` returns the per-key
//!   held-frame count instead of the global `TVPKeyRepeatCount`.
//! * The reference raises `TJS_E_INVALIDPARAM` for key codes ≥ 256 and
//!   out-of-range mouse buttons. This port returns 0/false for unknown
//!   codes and buttons instead (tests require it, and it is more robust
//!   against scripts probing values).
//! * `Mouse.getClickCount` is a 0 stub.
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
    register_mouse(engine)?;
    register_key(engine)?;
    Ok(())
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
    /// Cursor visibility — `Mouse.isVisible()` / `Mouse.setVisible`.
    pub visible: bool,
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
            visible: true,
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
    /// `down` transition resets the button's hold counter; an `up`
    /// transition sets its released-this-frame flag. Returns false when
    /// `button` is out of range.
    pub fn set_mouse_button(&mut self, button: usize, down: bool) -> bool {
        if button >= MOUSE_BUTTONS {
            return false;
        }
        self.mouse.buttons[button] = down;
        if down {
            self.mouse.hold[button] = 0;
        } else {
            self.mouse.released[button] = true;
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

    /// Frames `button` has been held (`Mouse.getRepeat`); 0 for
    /// out-of-range indices.
    pub fn mouse_button_repeat(&self, button: usize) -> u32 {
        self.mouse.hold.get(button).copied().unwrap_or(0)
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
    use super::*;
    use tjs2_sys::TjsValue;

    /// Fresh engine with `Mouse`/`Key` registered and a fresh, installed
    /// input state (tests must run single-threaded — the state is
    /// process-global).
    fn test_engine() -> (Tjs2Engine, Arc<Mutex<InputState>>) {
        let e = Tjs2Engine::new().expect("create engine");
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

        // getClickCount is a 0 stub.
        assert_eq!(eval_i(&e, "Mouse.getClickCount(0)"), 0);
    }

    // -- Test 4: unknown key codes are false, not errors --------------------

    #[test]
    fn unknown_key_codes_return_false_not_error() {
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
        let (e, _state) = test_engine();
        assert_eq!(eval_i(&e, "Mouse.getPressed(5)"), 0);
        assert_eq!(eval_i(&e, "Mouse.getPressed(-1)"), 0);
        assert_eq!(eval_i(&e, "Mouse.getReleased(99)"), 0);
        assert_eq!(eval_i(&e, "Mouse.getRepeat(99)"), 0);
    }

    // -- setCursorPos writes back into the state ----------------------------

    #[test]
    fn mouse_set_cursor_pos_writes_state() {
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
        // the arg-count check matches the reference (TJS_E_BADPARAMCOUNT)
        assert!(e.eval("Mouse.setCursorPos(1)", "test").is_err());
    }

    // -- key constants ------------------------------------------------------

    #[test]
    fn key_constants_evaluate_to_reference_values() {
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
        let (e, _state) = test_engine();
        for &(name, code) in KEY_CODE_TABLE {
            assert_eq!(
                eval_i(&e, &format!("Key.{name}")),
                i64::from(code),
                "Key.{name} must equal 0x{code:X}"
            );
        }
    }

    // -- last_pressed / hold counters at the Rust level ---------------------

    #[test]
    fn last_pressed_records_press_order() {
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
}
