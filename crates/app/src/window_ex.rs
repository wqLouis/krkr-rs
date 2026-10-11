//! Emulated Windows host facts for the `windowEx` `System`/`Window` members.
//!
//! The installed values describe a single-primary-monitor Windows desktop:
//! the render surface is the whole primary monitor, the cursor starts at the
//! origin, and the system metrics and CPU type are the common Win32
//! constants.

use compat::CompatHost;

/// A monitor rectangle:
/// `(left, top, right, bottom, work_left, work_top, work_right, work_bottom, primary)`.
type MonitorRect = (i32, i32, i32, i32, i32, i32, i32, i32, bool);

/// Installs every `windowEx` host-fact provider on `host`.
///
/// `surface_size` is the render surface the primary monitor covers. An
/// unknown system-metric name yields `0` and an unset environment variable
/// yields no value.
pub fn install_window_ex_facts(host: &CompatHost, surface_size: (u32, u32)) {
    let primary = primary_monitor(surface_size);

    host.set_window_ex_display_monitors(move || Ok(vec![primary]));
    host.set_window_ex_cursor_pos(|| Ok((0, 0)));
    host.set_window_ex_set_cursor_pos(|_x, _y| Ok(()));
    host.set_window_ex_system_metrics(move |name| Ok(system_metric(name, surface_size)));
    host.set_window_ex_read_env(|name| Ok(std::env::var(name).ok()));
    host.set_window_ex_expand_env(|value| Ok(expand_env_value(value)));
    host.set_window_ex_application_icon(|_path| Ok(()));
    host.set_window_ex_iconic_preview(Ok);
    host.set_window_ex_double_click_time(|| Ok(500));
    host.set_window_ex_cpu_type(|| Ok(0));
    host.set_window_ex_breathe(|| Ok(()));
    host.set_window_ex_is_breathing(|| Ok(false));
}

/// Returns the one primary monitor covering `surface_size`.
fn primary_monitor(surface_size: (u32, u32)) -> MonitorRect {
    let width = i32::try_from(surface_size.0).unwrap_or(i32::MAX);
    let height = i32::try_from(surface_size.1).unwrap_or(i32::MAX);
    (0, 0, width, height, 0, 0, width, height, true)
}

/// Returns the plausible Win32 value for the uppercased metric `name`.
fn system_metric(name: &str, surface_size: (u32, u32)) -> i64 {
    let width = i64::from(surface_size.0);
    let height = i64::from(surface_size.1);
    match name {
        "SM_CXSCREEN" => width,
        "SM_CYSCREEN" => height,
        "SM_CMONITORS" => 1,
        "SM_CXDOUBLECLK" | "SM_CYDOUBLECLK" => 4,
        "SM_CXDRAG" | "SM_CYDRAG" => 4,
        "SM_CXBORDER" | "SM_CYBORDER" => 1,
        _ => 0,
    }
}

/// Expands every `%NAME%` against the process environment, leaving a literal
/// whose `NAME` is unset unchanged.
fn expand_env_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(replacement) => out.push_str(&replacement),
                    Err(_) => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::{expand_env_value, primary_monitor, system_metric};

    #[test]
    fn primary_monitor_covers_the_surface() {
        assert_eq!(
            primary_monitor((1280, 720)),
            (0, 0, 1280, 720, 0, 0, 1280, 720, true)
        );
    }

    #[test]
    fn system_metrics_use_the_common_win32_values() {
        let surface = (800, 600);
        assert_eq!(system_metric("SM_CXSCREEN", surface), 800);
        assert_eq!(system_metric("SM_CYSCREEN", surface), 600);
        assert_eq!(system_metric("SM_CMONITORS", surface), 1);
        assert_eq!(system_metric("SM_CXDOUBLECLK", surface), 4);
        assert_eq!(system_metric("SM_CYDOUBLECLK", surface), 4);
        assert_eq!(system_metric("SM_CXDRAG", surface), 4);
        assert_eq!(system_metric("SM_CYDRAG", surface), 4);
        assert_eq!(system_metric("SM_CXBORDER", surface), 1);
        assert_eq!(system_metric("SM_CYBORDER", surface), 1);
        assert_eq!(system_metric("SM_CYCAPTION", surface), 0);
    }

    #[test]
    fn expand_env_leaves_unknown_literals_unchanged() {
        assert_eq!(expand_env_value("%KRKR_UNSET_VAR%"), "%KRKR_UNSET_VAR%");
        assert_eq!(expand_env_value("no percents"), "no percents");
        assert_eq!(expand_env_value("%"), "%");
    }

    #[test]
    fn expand_env_expands_a_known_variable() {
        let Ok(path) = std::env::var("PATH") else {
            return;
        };
        assert_eq!(expand_env_value("%PATH%"), path);
    }
}
