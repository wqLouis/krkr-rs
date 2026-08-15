//! Built-in default values for the preference keys the engine reads.
//!
//! The C++ code has no central default table: every call site passes its own
//! default to `GetValue<T>(name, defVal)`. The table below is the union of
//! those call-site defaults, collected from:
//!
//! * `reference/cpp/core/environ/ui/PreferenceConfig.h` (`initAllConfig` —
//!   the global *and* individual preference forms share this table)
//! * `reference/cpp/core/environ/cocos2d/MainScene.cpp`
//! * `reference/cpp/core/base/impl/SysInitImpl.cpp`
//! * `reference/cpp/core/visual/...` (renderer / font / layer options)
//! * `reference/cpp/core/environ/ConfigManager/LocaleConfigManager.cpp`
//!   (`user_language`)
//!
//! [`crate::GlobalConfig`] and [`crate::IndividualConfig`] fall back to this
//! table when a key is missing; the `get_*_with_default` methods let callers
//! supply their own default exactly like the C++ call sites do.
//!
//! Two C++ behaviors are intentionally **not** in the table:
//!
//! * `RenderManager_ogl.cpp` reads *arbitrary* OpenGL extension names with
//!   `GetValue<int>(*name, 1)` — the extension list is dynamic, so the
//!   `1` default lives at the call site, not here.
//! * `GL_*` checkbox defaults follow the non-iOS (`PreferenceConfig.h`)
//!   values; the iOS build overrides some of them.

/// A typed built-in default for one preference key.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DefaultValue {
    String(&'static str),
    Integer(i64),
    Real(f64),
    Boolean(bool),
}

/// The built-in default table (union of the C++ call-site defaults).
static DEFAULTS: &[(&str, DefaultValue)] = &[
    // --- PreferenceConfig.h initAllConfig (global + individual forms) ---
    ("outputlog", DefaultValue::Boolean(true)),
    ("showfps", DefaultValue::Boolean(false)),
    // "60" as a string in the preference form; read as an int by MainScene.cpp
    ("fps_limit", DefaultValue::Integer(60)),
    ("renderer", DefaultValue::String("software")),
    ("default_font", DefaultValue::String("")),
    ("force_default_font", DefaultValue::Boolean(false)),
    ("memusage", DefaultValue::String("unlimited")),
    ("keep_screen_alive", DefaultValue::Boolean(true)),
    ("vcursor_scale", DefaultValue::Real(0.5)),
    ("menu_handler_opa", DefaultValue::Real(0.15)),
    ("remember_last_path", DefaultValue::Boolean(true)),
    ("hide_android_sys_btn", DefaultValue::Boolean(false)),
    ("software_draw_thread", DefaultValue::Integer(0)),
    ("software_compress_tex", DefaultValue::String("none")),
    ("ogl_accurate_render", DefaultValue::Boolean(false)),
    ("ogl_max_texsize", DefaultValue::Integer(0)),
    ("ogl_compress_tex", DefaultValue::String("none")),
    // --- OpenGL extension toggles (PreferenceConfig.h, non-iOS) ---
    (
        "GL_EXT_shader_framebuffer_fetch",
        DefaultValue::Boolean(false),
    ),
    (
        "GL_ARM_shader_framebuffer_fetch",
        DefaultValue::Boolean(true),
    ),
    (
        "GL_NV_shader_framebuffer_fetch",
        DefaultValue::Boolean(true),
    ),
    ("GL_EXT_copy_image", DefaultValue::Boolean(false)),
    ("GL_OES_copy_image", DefaultValue::Boolean(false)),
    ("GL_ARB_copy_image", DefaultValue::Boolean(false)),
    ("GL_NV_copy_image", DefaultValue::Boolean(false)),
    ("GL_EXT_clear_texture", DefaultValue::Boolean(true)),
    ("GL_ARB_clear_texture", DefaultValue::Boolean(true)),
    ("GL_QCOM_alpha_test", DefaultValue::Boolean(true)),
    // --- LocaleConfigManager.cpp ---
    ("user_language", DefaultValue::String("")),
];

/// Look up the built-in default for `key`, if any.
pub fn default_for(key: &str) -> Option<DefaultValue> {
    DEFAULTS.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// Built-in default as a string (`getDefault...`), if any.
pub fn default_string(key: &str) -> Option<String> {
    match default_for(key)? {
        DefaultValue::String(s) => Some(s.to_owned()),
        DefaultValue::Integer(i) => Some(i.to_string()),
        DefaultValue::Real(r) => Some(r.to_string()),
        DefaultValue::Boolean(b) => Some(if b { "true" } else { "false" }.to_owned()),
    }
}

/// Built-in default as an integer, if any.
pub fn default_integer(key: &str) -> Option<i64> {
    match default_for(key)? {
        DefaultValue::Integer(i) => Some(i),
        DefaultValue::Real(r) => Some(r.trunc() as i64),
        DefaultValue::Boolean(b) => Some(i64::from(b)),
        DefaultValue::String(s) => parse_i64(s),
    }
}

/// Built-in default as a real number, if any.
pub fn default_real(key: &str) -> Option<f64> {
    match default_for(key)? {
        DefaultValue::Integer(i) => Some(i as f64),
        DefaultValue::Real(r) => Some(r),
        DefaultValue::Boolean(b) => Some(if b { 1.0 } else { 0.0 }),
        DefaultValue::String(s) => s.trim().parse().ok(),
    }
}

/// Built-in default as a boolean, if any.
pub fn default_boolean(key: &str) -> Option<bool> {
    match default_for(key)? {
        DefaultValue::Boolean(b) => Some(b),
        DefaultValue::Integer(i) => Some(i != 0),
        DefaultValue::Real(r) => Some(r != 0.0),
        DefaultValue::String(s) => parse_bool(s),
    }
}

fn parse_i64(s: &str) -> Option<i64> {
    let t = s.trim();
    t.parse::<i64>()
        .ok()
        .or_else(|| t.parse::<f64>().ok().map(|f| f.trunc() as i64))
}

fn parse_bool(s: &str) -> Option<bool> {
    let t = s.trim();
    let low = t.to_ascii_lowercase();
    match low.as_str() {
        "true" | "1" => return Some(true),
        "false" | "0" => return Some(false),
        _ => {}
    }
    parse_i64(t).map(|i| i != 0)
}
