//! `steam`, `steam_api` and `DrawDeviceForSteam` plugin ports.
//!
//! Behavioural reference:
//! `reference/cpp/plugins/steam/steamPlugin.cpp` (class `Steam`),
//! `reference/cpp/plugins/steam/steam_api.cpp` (class `steam_api`) and
//! `reference/cpp/plugins/DrawDeviceForSteam/DrawDeviceForSteam.cpp` (class
//! `DrawDeviceForSteam`).
//!
//! # The mock `Steam` class
//!
//! The reference `Steam` class links no `libsteam_api`: every member returns a
//! hard-coded value or does nothing. This port reproduces those values
//! one-for-one (`getLanguage` returns `"English"`, `getCloudQuota` returns
//! 1 GiB, `isBroadcasting` returns `0`, ...). All members carry the reference
//! `TJS_STATICMEMBER` flag, so they are attached to the class object with
//! [`CompatCtx::register_static_members`] on top of the instance class that
//! carries them. The reference registers no `Constructor()`, so `new Steam()`
//! raises `TJS_E_NOTIMPL` (`reference/cpp/core/plugin/ncbind.hpp:1948,1951`).
//!
//! # `steam_api` and `DrawDeviceForSteam`
//!
//! `steam_api` is the reference's trivial empty class: it registers only a
//! constructor, so this port registers an empty instance class with no
//! script-visible members (neither the reference's plain `vserion` field nor
//! any other state is exposed to TJS).
//!
//! `DrawDeviceForSteam` is a transparent wrapper around the main window's draw
//! device whose `interface` property returns that device as an integer
//! (`DrawDeviceForSteam.cpp:21-24,412-415`). This port has no host
//! draw-device object, so the getter reports the stable native payload
//! address as a non-zero synthetic pointer.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};
use std::ptr;

use tjs::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, NativeMethodDef,
    NativePropertyDef, NativeStaticMembers, VAL_STRING, Value,
};

use crate::native_util::{report_error, set_int, set_void};
use crate::natives::system::resolve;
use crate::{CompatCtx, CompatError};

// ---------------------------------------------------------------------------
// Reference mock values
// ---------------------------------------------------------------------------

/// `Steam.getLanguage()`.
const LANGUAGE: &str = "English";
/// `Steam.getAchievement()` (the reference `AchievementInfo::name`).
const ACHIEVEMENT_NAME: &str = "Achievement 1";
/// `Steam.getCloudQuota()` (`1024 * 1024 * 1024`, the reference `total`).
const CLOUD_QUOTA_TOTAL: i64 = 1024 * 1024 * 1024;
/// `Steam.getCloudFileInfo()` (the reference `CloudFileInfo::filename`).
const CLOUD_FILE_NAME: &str = "file1.txt";
/// `Steam.getDLCData()` (the reference mock DLC name).
const DLC_NAME: &str = "DLC1";
/// `Steam.getDLCCount()` (the reference mock count).
const DLC_COUNT: i64 = 2;

// ---------------------------------------------------------------------------
// Result marshalling
// ---------------------------------------------------------------------------

/// Per-engine scratch bytes for a returned string result. The ABI trampoline
/// copies the string immediately after the callback returns, so the buffer
/// only has to outlive the call.
#[derive(Default)]
struct SteamState {
    string: Vec<u8>,
}

/// Write `text` into `out`, keeping the bytes alive in the per-engine scratch
/// buffer.
fn set_string(ctx: &CompatCtx, out: *mut Value, text: &str) {
    if out.is_null() {
        return;
    }
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let mut state = services.state::<SteamState>();
    state.string.clear();
    state.string.extend_from_slice(text.as_bytes());
    state.string.push(0);
    let pointer = state.string.as_ptr() as *const c_char;
    drop(state);
    // SAFETY: the trampoline passes a valid out slot for the call and copies
    // the string before the callback returns.
    unsafe {
        (*out).ty = VAL_STRING;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = pointer;
        (*out).array = ptr::null();
        (*out).array_count = 0;
        (*out).retained = 0;
    }
}

/// Run a static `Steam` member that returns a fixed string (the reference
/// ignores the member's arguments).
fn steam_string(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    member: &str,
    text: &str,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            &format!("Steam.{member}: compat context is not bound"),
        );
    };
    set_string(&resolved.ctx(), out, text);
    0
}

// ---------------------------------------------------------------------------
// `Steam` instance class (empty payload + empty constructor)
// ---------------------------------------------------------------------------

/// Allocate the empty `Steam` payload (`new Steam()` allocates an empty
/// service object; every member is static).
extern "C" fn steam_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(0u8)) as *mut c_void
}

/// Free a payload created by [`steam_create`].
extern "C" fn steam_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if !instance.is_null() {
        // SAFETY: `instance` came from `steam_create` and is freed once.
        drop(unsafe { Box::from_raw(instance as *mut u8) });
    }
}

/// `new Steam()` — the reference `NCB_REGISTER_CLASS(Steam)` declares no
/// `Constructor()` (`reference/cpp/plugins/steam/steamPlugin.cpp:218-253`),
/// so ncbind registers its dummy constructor, which raises `TJS_E_NOTIMPL`
/// (`reference/cpp/core/plugin/ncbind.hpp:1948,1951-1956`).
extern "C" fn steam_ctor(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    _out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    report_error(out_error, "Steam: the class has no constructor")
}

// ---------------------------------------------------------------------------
// `Steam` static methods
// ---------------------------------------------------------------------------

/// `Steam.getAchievement(n)` — returns the mock achievement display name.
extern "C" fn steam_get_achievement(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    steam_string(engine, out, out_error, "getAchievement", ACHIEVEMENT_NAME)
}

/// `Steam.setAchievement(n)` — the mock always succeeds.
extern "C" fn steam_set_achievement(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_int(out, 1);
    0
}

/// `Steam.clearAchievement(n)` — the mock always succeeds.
extern "C" fn steam_clear_achievement(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_int(out, 1);
    0
}

/// `Steam.getLanguage()`.
extern "C" fn steam_get_language(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    steam_string(engine, out, out_error, "getLanguage", LANGUAGE)
}

/// `Steam.requestInitialize()` — the mock assumes success.
extern "C" fn steam_request_initialize(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_int(out, 1);
    0
}

/// `Steam.getCloudQuota()` — returns the mock total quota (1 GiB).
extern "C" fn steam_get_cloud_quota(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_int(out, CLOUD_QUOTA_TOTAL);
    0
}

/// `Steam.getCloudFileInfo(n)` — returns the mock file name.
extern "C" fn steam_get_cloud_file_info(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    steam_string(engine, out, out_error, "getCloudFileInfo", CLOUD_FILE_NAME)
}

/// `Steam.deleteCloudFile(name)` — the mock always succeeds.
extern "C" fn steam_delete_cloud_file(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_int(out, 1);
    0
}

/// `Steam.copyCloudFile(src, dest)` — the mock always succeeds.
extern "C" fn steam_copy_cloud_file(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_int(out, 1);
    0
}

/// `Steam.triggerScreenshot()` — the mock does nothing.
extern "C" fn steam_trigger_screenshot(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_void(out);
    0
}

/// `Steam.hookScreenshots(callback)` — the mock does nothing.
extern "C" fn steam_hook_screenshots(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_void(out);
    0
}

/// `Steam.writeScreenshot(layer, location)` — the mock does nothing.
extern "C" fn steam_write_screenshot(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_void(out);
    0
}

/// `Steam.isBroadcasting()` — the mock is never broadcasting.
extern "C" fn steam_is_broadcasting(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_int(out, 0);
    0
}

/// `Steam.hookBroadcasting(callback)` — the mock does nothing.
extern "C" fn steam_hook_broadcasting(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_void(out);
    0
}

/// `Steam.isIsSubscribedApp(appID)` — the mock is subscribed.
extern "C" fn steam_is_is_subscribed_app(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_int(out, 1);
    0
}

/// `Steam.ssDlcInstalled(appID)` — the mock DLC is installed.
extern "C" fn steam_ss_dlc_installed(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_int(out, 1);
    0
}

/// `Steam.getDLCCount()` — the mock has two DLC.
extern "C" fn steam_get_dlc_count(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_int(out, DLC_COUNT);
    0
}

/// `Steam.getDLCData(no)` — returns the mock DLC name.
extern "C" fn steam_get_dlc_data(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    steam_string(engine, out, out_error, "getDLCData", DLC_NAME)
}

// ---------------------------------------------------------------------------
// `Steam` static properties
// ---------------------------------------------------------------------------

/// `Steam.initialized` — the reference getter returns `1`.
extern "C" fn steam_initialized_get(
    _engine: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_int(out, 1);
    0
}

/// `Steam.achievementsCount` — the reference getter returns `0`.
extern "C" fn steam_achievements_count_get(
    _engine: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_int(out, 0);
    0
}

/// `Steam.cloudEnabled` — the reference getter returns `true`.
extern "C" fn steam_cloud_enabled_get(
    _engine: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_int(out, 1);
    0
}

/// `Steam.cloudFileCount` — the reference getter returns `0`.
extern "C" fn steam_cloud_file_count_get(
    _engine: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_int(out, 0);
    0
}

/// `Steam.dlcCount` — the reference getter returns `0`.
extern "C" fn steam_dlc_count_get(
    _engine: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_int(out, 0);
    0
}

// ---------------------------------------------------------------------------
// `steam_api` instance class
// ---------------------------------------------------------------------------

/// Allocate the empty `steam_api` payload. The reference class has no state
/// (it registers only a constructor), so `new steam_api()` allocates an empty
/// instance.
extern "C" fn steam_api_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(0u8)) as *mut c_void
}

/// Free a payload created by [`steam_api_create`].
extern "C" fn steam_api_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if !instance.is_null() {
        // SAFETY: `instance` came from `steam_api_create` and is freed once.
        drop(unsafe { Box::from_raw(instance as *mut u8) });
    }
}

/// `new steam_api()` — the reference constructor is empty.
extern "C" fn steam_api_ctor(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// `DrawDeviceForSteam` instance class
// ---------------------------------------------------------------------------

/// `DrawDeviceForSteam`'s instance payload. The reference proxy stores the
/// wrapped draw device; this host has no draw-device object, so the payload
/// is an empty marker whose address [`draw_device_interface_get`] reports
/// as a synthetic pointer.
struct DrawDeviceForSteamInst;

/// Allocate a `DrawDeviceForSteam` proxy.
extern "C" fn draw_device_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(DrawDeviceForSteamInst)) as *mut c_void
}

/// Free a payload created by [`draw_device_create`].
extern "C" fn draw_device_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if !instance.is_null() {
        // SAFETY: `instance` came from `draw_device_create` and is freed once.
        drop(unsafe { Box::from_raw(instance as *mut DrawDeviceForSteamInst) });
    }
}

/// `new DrawDeviceForSteam()` — the reference factory allocates an empty
/// proxy.
extern "C" fn draw_device_ctor(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void(out);
    0
}

/// `DrawDeviceForSteam.interface` — the reference returns
/// `reinterpret_cast<tjs_int64>(TVPMainWindow->GetDrawDevice())`
/// (`DrawDeviceForSteam.cpp:21-24`, registered at
/// `DrawDeviceForSteam.cpp:412-415`). This host has no draw-device object,
/// so the getter reports the stable native payload address as a non-zero
/// synthetic pointer.
extern "C" fn draw_device_interface_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int(out, instance as i64);
    0
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the `Steam`, `steam_api` and `DrawDeviceForSteam` native classes.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    // `Steam` — an empty instance class carrying the reference's static mock
    // surface on the class object.
    ctx.register_instance(&NativeInstanceBuilder {
        name: "Steam",
        create: steam_create,
        destroy: steam_destroy,
        invalidate: None,
        methods: vec![NativeInstanceMethodDef {
            name: "Steam",
            f: steam_ctor,
        }],
        properties: vec![],
    })?;
    ctx.register_static_members(&NativeStaticMembers {
        class_name: "Steam",
        methods: vec![
            NativeMethodDef {
                name: "getAchievement",
                f: steam_get_achievement,
            },
            NativeMethodDef {
                name: "setAchievement",
                f: steam_set_achievement,
            },
            NativeMethodDef {
                name: "clearAchievement",
                f: steam_clear_achievement,
            },
            NativeMethodDef {
                name: "getLanguage",
                f: steam_get_language,
            },
            NativeMethodDef {
                name: "requestInitialize",
                f: steam_request_initialize,
            },
            NativeMethodDef {
                name: "getCloudQuota",
                f: steam_get_cloud_quota,
            },
            NativeMethodDef {
                name: "getCloudFileInfo",
                f: steam_get_cloud_file_info,
            },
            NativeMethodDef {
                name: "deleteCloudFile",
                f: steam_delete_cloud_file,
            },
            NativeMethodDef {
                name: "copyCloudFile",
                f: steam_copy_cloud_file,
            },
            NativeMethodDef {
                name: "triggerScreenshot",
                f: steam_trigger_screenshot,
            },
            NativeMethodDef {
                name: "hookScreenshots",
                f: steam_hook_screenshots,
            },
            NativeMethodDef {
                name: "writeScreenshot",
                f: steam_write_screenshot,
            },
            NativeMethodDef {
                name: "isBroadcasting",
                f: steam_is_broadcasting,
            },
            NativeMethodDef {
                name: "hookBroadcasting",
                f: steam_hook_broadcasting,
            },
            NativeMethodDef {
                name: "isIsSubscribedApp",
                f: steam_is_is_subscribed_app,
            },
            NativeMethodDef {
                name: "ssDlcInstalled",
                f: steam_ss_dlc_installed,
            },
            NativeMethodDef {
                name: "getDLCCount",
                f: steam_get_dlc_count,
            },
            NativeMethodDef {
                name: "getDLCData",
                f: steam_get_dlc_data,
            },
        ],
        properties: vec![
            NativePropertyDef {
                name: "initialized",
                get: Some(steam_initialized_get),
                set: None,
            },
            NativePropertyDef {
                name: "achievementsCount",
                get: Some(steam_achievements_count_get),
                set: None,
            },
            NativePropertyDef {
                name: "cloudEnabled",
                get: Some(steam_cloud_enabled_get),
                set: None,
            },
            NativePropertyDef {
                name: "cloudFileCount",
                get: Some(steam_cloud_file_count_get),
                set: None,
            },
            NativePropertyDef {
                name: "dlcCount",
                get: Some(steam_dlc_count_get),
                set: None,
            },
        ],
    })?;

    // `steam_api` — the reference empty class (constructor only).
    ctx.register_instance(&NativeInstanceBuilder {
        name: "steam_api",
        create: steam_api_create,
        destroy: steam_api_destroy,
        invalidate: None,
        methods: vec![NativeInstanceMethodDef {
            name: "steam_api",
            f: steam_api_ctor,
        }],
        properties: vec![],
    })?;

    // `DrawDeviceForSteam` — the transparent draw-device proxy.
    ctx.register_instance(&NativeInstanceBuilder {
        name: "DrawDeviceForSteam",
        create: draw_device_create,
        destroy: draw_device_destroy,
        invalidate: None,
        methods: vec![NativeInstanceMethodDef {
            name: "DrawDeviceForSteam",
            f: draw_device_ctor,
        }],
        properties: vec![NativeInstancePropertyDef {
            name: "interface",
            get: Some(draw_device_interface_get),
            set: None,
        }],
    })?;
    Ok(())
}
