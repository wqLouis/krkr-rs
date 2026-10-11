//! `window_ex` plugin — full port of the KiriKiri `windowEx.dll`.
//!
//! Reference: `reference/cpp/plugins/windowEx.cpp`. The plugin has two
//! halves: it attaches members to existing classes and it provides the
//! `WindowEx` implementation that the reference hooks onto `Window`
//! (`NCB_ATTACH_CLASS_WITH_HOOK(WindowEx, Window)`).
//!
//! # Class layout
//!
//! Our TJS ABI has no subclassing and `register_instance_members` attaches
//! objthis-only members (no per-object native payload) to an existing class,
//! so — exactly like `main` did for `registerExEvent` — the `WindowEx`
//! members live on `Window` and `MenuItemEx`'s on `MenuItem`. The small
//! per-object flag sets live in [`window::WindowExState`] /
//! [`menu_item::MenuItemExState`] keyed by the `objthis` pointer; there is no
//! `WindowEx` global class, matching the reference.
//!
//! # Registered surface
//!
//! * `System` static methods: `getDisplayMonitors`, `getMonitorInfo`,
//!   `getCursorPos`, `setCursorPos`, `getSystemMetrics`, `readEnvValue`,
//!   `expandEnvString`, `setApplicationIcon`, `setIconicPreview`,
//!   `getDoubleClickTime`, `breathe`, `isBreathing`, `getAboutString`,
//!   `getCPUType`.
//! * `Scripts` static methods: `eval` (error-log-aware override),
//!   `setEvalErrorLog`.
//! * `MenuItem` instance members: `popupEx`, `rightJustify`, `bmpItem`,
//!   `bmpChecked`, `bmpUnchecked`; static constants `biSystem` … `biPopupMinimize`.
//! * `Console` (`Debug.console`) instance members: `restoreMaximize`,
//!   `maximize`, `getRect`, `setPos`, `getPlacement`, `setPlacement`,
//!   `bringAfter`; `Pad.registerExEvent`.
//! * `Window` instance members: `minimize`, `maximize`, `showRestore`,
//!   `focusMenuByKey`, `resetWindowIcon`, `setWindowIcon`, `getWindowRect`,
//!   `getClientRect`, `getNormalRect`, `disableResize`,
//!   `setOverlayBitmap`, `exSystemMenu`,
//!   `enableNCMouseEvent`, `ncHitTest`, `setMessageHook`, `bringTo`,
//!   `sendToBack`, `registerExEvent`, `getNotificationNum`,
//!   `getNotificationName`, and the static constants `nchtError` …
//!   `nchtBorder`.
//! * `Window._Notifications`: the two-way `WM_*` name/value dictionary.
//!
//! # Host seam
//!
//! The display monitors, cursor position, environment variables and the
//! remaining system metrics are genuine host facts; they go through the
//! closure seam defined in [`host`] (installers are inherent methods on
//! `CompatHost`) and raise a typed TJS error until the host installs one.
//! Three members keep a reference literal instead (the vendored reference
//! body is not a host query): `getDoubleClickTime` reads `-1`
//! (`windowEx.cpp:1422`), `getCPUType` reads the `tjs_uint32` CPU-feature
//! bitmask as an integer, defaulting to 0 (`DetectCPU.cpp:170-175`), and
//! `getSystemMetrics` first performs the reference's `System.metrics`
//! dictionary lookup (`windowEx.cpp:1466-1481`) before consulting the host.

mod console;
mod host;
mod menu_item;
mod notifications;
mod pad;
mod scripts;
mod system;
mod util;
mod window;

use crate::{CompatCtx, CompatError};

/// Register the plugin's native surface on the VM.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    system::register(ctx)?;
    scripts::register(ctx)?;
    menu_item::register(ctx)?;
    console::register(ctx)?;
    pad::register(ctx)?;
    window::register(ctx)?;
    Ok(())
}
