//! Host-fact seam for the `windowEx` plugin.
//!
//! A handful of `System`/`Window` members describe genuine host facts (the
//! display monitors, the cursor position, system metrics, environment
//! variables, the application/window icon, the double-click time). L3 owns
//! no platform code, so the host installs one closure per fact; until a fact
//! is installed its member raises a typed TJS error rather than
//! fabricating a value.
//!
//! The installers are inherent methods on [`CompatHost`] defined here (the
//! same crate may add inherent impls for its own types), so the plugin stays
//! self-contained: no other module changes and the public installers are
//! reachable from the app host and the integration tests.
//!
//! Fact shapes:
//!
//! * a monitor rectangle is
//!   `(left, top, right, bottom, work_left, work_top, work_right, work_bottom, primary)`;
//! * `getMonitorInfo` resolves its monitor from the `getDisplayMonitors`
//!   list (the primary, or the monitor for a window, point or rectangle);
//! * `readEnvValue` returns `None` for an unset variable;
//! * `expandEnvString` performs `%NAME%` expansion.

use crate::{CompatHost, CompatServices};

/// A monitor rectangle:
/// `(left, top, right, bottom, work_left, work_top, work_right, work_bottom, primary)`.
type MonitorRect = (i32, i32, i32, i32, i32, i32, i32, i32, bool);
/// `getDisplayMonitors` provider signature.
type DisplayMonitorsFn = dyn Fn() -> Result<Vec<MonitorRect>, String>;
/// `getCursorPos` provider signature.
type CursorPosFn = dyn Fn() -> Result<(i32, i32), String>;
/// `setCursorPos(x, y)` provider signature.
type SetCursorPosFn = dyn Fn(i32, i32) -> Result<(), String>;
/// `getSystemMetrics(name)` provider signature.
type SystemMetricsFn = dyn Fn(&str) -> Result<i64, String>;
/// `readEnvValue(name)` provider signature.
type ReadEnvFn = dyn Fn(&str) -> Result<Option<String>, String>;
/// `expandEnvString(value)` provider signature.
type ExpandEnvFn = dyn Fn(&str) -> Result<String, String>;
/// `setApplicationIcon(path)` provider signature.
type SetApplicationIconFn = dyn Fn(&str) -> Result<(), String>;
/// `setIconicPreview(enabled)` provider signature.
type SetIconicPreviewFn = dyn Fn(bool) -> Result<bool, String>;
/// `getDoubleClickTime` provider signature.
type DoubleClickTimeFn = dyn Fn() -> Result<i64, String>;
/// `getCPUType` provider signature: the `tjs_uint32` CPU-feature bitmask the
/// reference returns (`reference/cpp/core/environ/DetectCPU.cpp:170-175`).
type CpuTypeFn = dyn Fn() -> Result<i64, String>;
/// `breathe` provider signature.
type BreatheFn = dyn Fn() -> Result<(), String>;
/// `isBreathing` provider signature.
type IsBreathingFn = dyn Fn() -> Result<bool, String>;

/// Per-binding `windowEx` host facts. `None` means the host has not installed
/// the fact yet and the matching member raises a typed error.
#[derive(Default)]
pub(crate) struct WindowExHost {
    display_monitors: Option<Box<DisplayMonitorsFn>>,
    cursor_pos: Option<Box<CursorPosFn>>,
    set_cursor_pos: Option<Box<SetCursorPosFn>>,
    system_metrics: Option<Box<SystemMetricsFn>>,
    read_env: Option<Box<ReadEnvFn>>,
    expand_env: Option<Box<ExpandEnvFn>>,
    set_application_icon: Option<Box<SetApplicationIconFn>>,
    set_iconic_preview: Option<Box<SetIconicPreviewFn>>,
    double_click_time: Option<Box<DoubleClickTimeFn>>,
    cpu_type: Option<Box<CpuTypeFn>>,
    breathe: Option<Box<BreatheFn>>,
    is_breathing: Option<Box<IsBreathingFn>>,
}

impl CompatHost {
    /// Install the `System.getDisplayMonitors()` provider, the monitor list
    /// `System.getMonitorInfo` also selects from.
    pub fn set_window_ex_display_monitors(
        &self,
        f: impl Fn() -> Result<Vec<MonitorRect>, String> + 'static,
    ) {
        self.services().state::<WindowExHost>().display_monitors = Some(Box::new(f));
    }

    /// Install the `System.getCursorPos()` provider.
    pub fn set_window_ex_cursor_pos(&self, f: impl Fn() -> Result<(i32, i32), String> + 'static) {
        self.services().state::<WindowExHost>().cursor_pos = Some(Box::new(f));
    }

    /// Install the `System.setCursorPos(x, y)` provider.
    pub fn set_window_ex_set_cursor_pos(
        &self,
        f: impl Fn(i32, i32) -> Result<(), String> + 'static,
    ) {
        self.services().state::<WindowExHost>().set_cursor_pos = Some(Box::new(f));
    }

    /// Install the `System.getSystemMetrics(name)` provider.
    pub fn set_window_ex_system_metrics(&self, f: impl Fn(&str) -> Result<i64, String> + 'static) {
        self.services().state::<WindowExHost>().system_metrics = Some(Box::new(f));
    }

    /// Install the `System.readEnvValue(name)` provider. The closure returns
    /// `None` for an unset variable.
    pub fn set_window_ex_read_env(
        &self,
        f: impl Fn(&str) -> Result<Option<String>, String> + 'static,
    ) {
        self.services().state::<WindowExHost>().read_env = Some(Box::new(f));
    }

    /// Install the `System.expandEnvString(value)` provider.
    pub fn set_window_ex_expand_env(&self, f: impl Fn(&str) -> Result<String, String> + 'static) {
        self.services().state::<WindowExHost>().expand_env = Some(Box::new(f));
    }

    /// Install the `System.setApplicationIcon(path)` provider.
    pub fn set_window_ex_application_icon(&self, f: impl Fn(&str) -> Result<(), String> + 'static) {
        self.services().state::<WindowExHost>().set_application_icon = Some(Box::new(f));
    }

    /// Install the `System.setIconicPreview(enabled)` provider.
    pub fn set_window_ex_iconic_preview(&self, f: impl Fn(bool) -> Result<bool, String> + 'static) {
        self.services().state::<WindowExHost>().set_iconic_preview = Some(Box::new(f));
    }

    /// Install the `System.getDoubleClickTime()` provider.
    pub fn set_window_ex_double_click_time(&self, f: impl Fn() -> Result<i64, String> + 'static) {
        self.services().state::<WindowExHost>().double_click_time = Some(Box::new(f));
    }

    /// Install the `System.getCPUType()` provider (the CPU-feature bitmask).
    pub fn set_window_ex_cpu_type(&self, f: impl Fn() -> Result<i64, String> + 'static) {
        self.services().state::<WindowExHost>().cpu_type = Some(Box::new(f));
    }

    /// Install the `System.breathe()` provider.
    pub fn set_window_ex_breathe(&self, f: impl Fn() -> Result<(), String> + 'static) {
        self.services().state::<WindowExHost>().breathe = Some(Box::new(f));
    }

    /// Install the `System.isBreathing()` provider.
    pub fn set_window_ex_is_breathing(&self, f: impl Fn() -> Result<bool, String> + 'static) {
        self.services().state::<WindowExHost>().is_breathing = Some(Box::new(f));
    }
}

/// Call the installed `getDisplayMonitors` provider, if any.
pub(crate) fn display_monitors(
    services: &CompatServices,
) -> Option<Result<Vec<MonitorRect>, String>> {
    services
        .state::<WindowExHost>()
        .display_monitors
        .as_ref()
        .map(|f| f())
}

/// Call the installed `getCursorPos` provider, if any.
pub(crate) fn cursor_pos(services: &CompatServices) -> Option<Result<(i32, i32), String>> {
    services
        .state::<WindowExHost>()
        .cursor_pos
        .as_ref()
        .map(|f| f())
}

/// Call the installed `setCursorPos` provider, if any.
pub(crate) fn set_cursor_pos(
    services: &CompatServices,
    x: i32,
    y: i32,
) -> Option<Result<(), String>> {
    services
        .state::<WindowExHost>()
        .set_cursor_pos
        .as_ref()
        .map(|f| f(x, y))
}

/// Call the installed `getSystemMetrics` provider, if any.
pub(crate) fn system_metrics(services: &CompatServices, name: &str) -> Option<Result<i64, String>> {
    services
        .state::<WindowExHost>()
        .system_metrics
        .as_ref()
        .map(|f| f(name))
}

/// Call the installed `readEnvValue` provider, if any.
pub(crate) fn read_env(
    services: &CompatServices,
    name: &str,
) -> Option<Result<Option<String>, String>> {
    services
        .state::<WindowExHost>()
        .read_env
        .as_ref()
        .map(|f| f(name))
}

/// Call the installed `expandEnvString` provider, if any.
pub(crate) fn expand_env(services: &CompatServices, value: &str) -> Option<Result<String, String>> {
    services
        .state::<WindowExHost>()
        .expand_env
        .as_ref()
        .map(|f| f(value))
}

/// Call the installed `setApplicationIcon` provider, if any.
pub(crate) fn set_application_icon(
    services: &CompatServices,
    path: &str,
) -> Option<Result<(), String>> {
    services
        .state::<WindowExHost>()
        .set_application_icon
        .as_ref()
        .map(|f| f(path))
}

/// Call the installed `setIconicPreview` provider, if any.
pub(crate) fn set_iconic_preview(
    services: &CompatServices,
    enabled: bool,
) -> Option<Result<bool, String>> {
    services
        .state::<WindowExHost>()
        .set_iconic_preview
        .as_ref()
        .map(|f| f(enabled))
}

/// Call the installed `getDoubleClickTime` provider, if any.
pub(crate) fn double_click_time(services: &CompatServices) -> Option<Result<i64, String>> {
    services
        .state::<WindowExHost>()
        .double_click_time
        .as_ref()
        .map(|f| f())
}

/// Call the installed `getCPUType` provider, if any.
pub(crate) fn cpu_type(services: &CompatServices) -> Option<Result<i64, String>> {
    services
        .state::<WindowExHost>()
        .cpu_type
        .as_ref()
        .map(|f| f())
}

/// Call the installed `breathe` provider, if any.
pub(crate) fn breathe(services: &CompatServices) -> Option<Result<(), String>> {
    services
        .state::<WindowExHost>()
        .breathe
        .as_ref()
        .map(|f| f())
}

/// Call the installed `isBreathing` provider, if any.
pub(crate) fn is_breathing(services: &CompatServices) -> Option<Result<bool, String>> {
    services
        .state::<WindowExHost>()
        .is_breathing
        .as_ref()
        .map(|f| f())
}
