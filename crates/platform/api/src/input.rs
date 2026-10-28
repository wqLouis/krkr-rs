//! Platform-agnostic input DTOs.
//!
//! These values are the generic input contract between the platform layer
//! (L1) and the middle layer (L2). L1 backends translate native events
//! (winit, Android) into [`InputEvent`]; L2 folds them into explicit state
//! (`engine::input::InputState`); L3 maps them onto the TJS key codes and hit
//! testing. L1 itself assigns no engine meaning.
//!
//! The types live in `api` (not `platform`) because they are pure values, not
//! platform code: the same `InputEvent` describes desktop and Android input
//! and both the L1 backends and the higher layers name it directly.

use crate::host::TouchPhase;

/// High bit marking a [`PhysicalKey::Native`] identity.
///
/// [`PhysicalKey::index`] sets it on a raw native code so it can never collide
/// with a named key's id.
pub(crate) const NATIVE_KEY_FLAG: u32 = 0x8000_0000;

macro_rules! define_physical_keys {
    ($( $id:literal => $variant:ident ),* $(,)?) => {
        /// A platform-neutral physical key identity.
        ///
        /// L1 backends translate their native key codes into this type; L2
        /// keys its input state on [`PhysicalKey::index`]; L3 maps it onto the
        /// TJS-visible virtual-key codes. The type names no platform API and
        /// no engine key code, so every backend produces the same values.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum PhysicalKey {
            $(
                #[doc = concat!("The `", stringify!($variant), "` key.")]
                $variant,
            )*
            /// A key the backend could not identify, carrying its raw native
            /// code.
            ///
            /// [`PhysicalKey::index`] sets `NATIVE_KEY_FLAG`, so a native
            /// code can never collide with a named key.
            Native(u32),
        }

        impl PhysicalKey {
            /// Returns the stable numeric identity that keys engine input
            /// state.
            ///
            /// Named keys return their declared id; [`PhysicalKey::Native`]
            /// returns the raw code with `NATIVE_KEY_FLAG` set. The named
            /// ids are stable across builds.
            pub fn index(self) -> u32 {
                match self {
                    $(
                        PhysicalKey::$variant => $id,
                    )*
                    PhysicalKey::Native(raw) => raw | NATIVE_KEY_FLAG,
                }
            }
        }
    };
}

define_physical_keys! {
    0x0001 => KeyA,
    0x0002 => KeyB,
    0x0003 => KeyC,
    0x0004 => KeyD,
    0x0005 => KeyE,
    0x0006 => KeyF,
    0x0007 => KeyG,
    0x0008 => KeyH,
    0x0009 => KeyI,
    0x000A => KeyJ,
    0x000B => KeyK,
    0x000C => KeyL,
    0x000D => KeyM,
    0x000E => KeyN,
    0x000F => KeyO,
    0x0010 => KeyP,
    0x0011 => KeyQ,
    0x0012 => KeyR,
    0x0013 => KeyS,
    0x0014 => KeyT,
    0x0015 => KeyU,
    0x0016 => KeyV,
    0x0017 => KeyW,
    0x0018 => KeyX,
    0x0019 => KeyY,
    0x001A => KeyZ,
    0x0020 => Digit0,
    0x0021 => Digit1,
    0x0022 => Digit2,
    0x0023 => Digit3,
    0x0024 => Digit4,
    0x0025 => Digit5,
    0x0026 => Digit6,
    0x0027 => Digit7,
    0x0028 => Digit8,
    0x0029 => Digit9,
    0x0030 => F1,
    0x0031 => F2,
    0x0032 => F3,
    0x0033 => F4,
    0x0034 => F5,
    0x0035 => F6,
    0x0036 => F7,
    0x0037 => F8,
    0x0038 => F9,
    0x0039 => F10,
    0x003A => F11,
    0x003B => F12,
    0x003C => F13,
    0x003D => F14,
    0x003E => F15,
    0x003F => F16,
    0x0040 => F17,
    0x0041 => F18,
    0x0042 => F19,
    0x0043 => F20,
    0x0044 => F21,
    0x0045 => F22,
    0x0046 => F23,
    0x0047 => F24,
    0x0050 => Backspace,
    0x0051 => Tab,
    0x0052 => Clear,
    0x0053 => Enter,
    0x0054 => Escape,
    0x0055 => Space,
    0x0056 => Delete,
    0x0057 => Insert,
    0x0058 => Home,
    0x0059 => End,
    0x005A => PageUp,
    0x005B => PageDown,
    0x005C => ArrowUp,
    0x005D => ArrowDown,
    0x005E => ArrowLeft,
    0x005F => ArrowRight,
    0x0060 => Select,
    0x0061 => Print,
    0x0062 => Execute,
    0x0063 => PrintScreen,
    0x0064 => Help,
    0x0070 => ShiftLeft,
    0x0071 => ShiftRight,
    0x0072 => ControlLeft,
    0x0073 => ControlRight,
    0x0074 => AltLeft,
    0x0075 => AltRight,
    0x0080 => CapsLock,
    0x0081 => NumLock,
    0x0082 => ScrollLock,
    0x0083 => Pause,
    0x0084 => SuperLeft,
    0x0085 => SuperRight,
    0x0086 => ContextMenu,
    0x0087 => Sleep,
    0x0090 => Numpad0,
    0x0091 => Numpad1,
    0x0092 => Numpad2,
    0x0093 => Numpad3,
    0x0094 => Numpad4,
    0x0095 => Numpad5,
    0x0096 => Numpad6,
    0x0097 => Numpad7,
    0x0098 => Numpad8,
    0x0099 => Numpad9,
    0x009A => NumpadMultiply,
    0x009B => NumpadAdd,
    0x009C => NumpadSeparator,
    0x009D => NumpadSubtract,
    0x009E => NumpadDecimal,
    0x009F => NumpadDivide,
    0x00A0 => NumpadEnter,
    0x00B0 => Backquote,
    0x00B1 => Backslash,
    0x00B2 => BracketLeft,
    0x00B3 => BracketRight,
    0x00B4 => Comma,
    0x00B5 => Period,
    0x00B6 => Semicolon,
    0x00B7 => Quote,
    0x00B8 => Minus,
    0x00B9 => Equal,
    0x00BA => Slash,
    0x00BB => IntlBackslash,
    0x00BC => IntlRo,
    0x00BD => IntlYen,
    0x00C0 => KanaMode,
    0x00C1 => Hangul,
    0x00C2 => Junja,
    0x00C3 => Final,
    0x00C4 => Hanja,
    0x00C5 => Convert,
    0x00C6 => NonConvert,
    0x00C7 => Accept,
    0x00C8 => ModeChange,
    0x00C9 => ProcessKey,
    0x00D0 => BrowserBack,
    0x00D1 => BrowserForward,
    0x00D2 => BrowserRefresh,
    0x00D3 => BrowserStop,
    0x00D4 => BrowserSearch,
    0x00D5 => BrowserFavorites,
    0x00D6 => BrowserHome,
    0x00D7 => VolumeMute,
    0x00D8 => VolumeDown,
    0x00D9 => VolumeUp,
    0x00DA => MediaTrackNext,
    0x00DB => MediaTrackPrevious,
    0x00DC => MediaStop,
    0x00DD => MediaPlayPause,
    0x00DE => LaunchMail,
    0x00DF => MediaSelect,
    0x00E0 => LaunchApp1,
    0x00E1 => LaunchApp2,
}

/// A mouse button, independent of the platform event type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    /// The primary (usually left) button.
    Left,
    /// The secondary (usually right) button.
    Right,
    /// The middle button.
    Middle,
    /// The first extra (back) button.
    Back,
    /// The second extra (forward) button.
    Forward,
    /// A button the platform reported as a raw code.
    Other(u16),
}

/// A single raw input event.
///
/// All coordinates are in **physical window pixels** — the same space as
/// [`crate::host::InputState`] and the platform window size — so a higher
/// layer can hit-test without knowing the display scale factor. The `code`
/// carried by [`InputEvent::Key`] is a [`PhysicalKey`], which L3 maps onto the
/// TJS-visible key codes.
#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    /// The pointer moved to the given position.
    PointerMoved {
        /// Horizontal physical coordinate.
        x: f32,
        /// Vertical physical coordinate.
        y: f32,
    },
    /// The pointer left the window.
    PointerLeft,
    /// A mouse button changed state.
    MouseButton {
        /// Which button changed.
        button: MouseButton,
        /// `true` for press, `false` for release.
        pressed: bool,
    },
    /// A vertical wheel scroll.
    Wheel {
        /// Physical pixels (or lines) scrolled; positive is down.
        delta: f32,
    },
    /// A physical key changed state.
    Key {
        /// The platform-neutral physical key.
        code: PhysicalKey,
        /// `true` for press, `false` for release.
        pressed: bool,
    },
    /// Committed text input (IME).
    Text(String),
    /// A touch contact changed phase.
    Touch {
        /// Stable identifier of the contact.
        id: u64,
        /// Current phase of the contact.
        phase: TouchPhase,
        /// Horizontal physical coordinate.
        x: f32,
        /// Vertical physical coordinate.
        y: f32,
    },
}
