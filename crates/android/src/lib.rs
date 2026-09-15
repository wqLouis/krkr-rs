//! Android entry point for the krkr-rs engine.
//!
//! This crate is a `cdylib` loaded by the launcher's `KrkrGameActivity`
//! (declared through `android.app.lib_name` in the manifest). It does exactly
//! what the desktop binary's `run` subcommand does — take a game directory and
//! run it — with the Android UI living entirely in Kotlin around it
//! (`docs/android.md`). The engine itself knows nothing about Android: the
//! command-line runner and this entry point build the *same* Bevy app through
//! `krkr_render::runner`.
//!
//! ## How the game directory arrives
//!
//! The user picks a folder in the Material 3 launcher, which starts this
//! Activity with the resolved path; `KrkrGameActivity` publishes it as a static
//! field **before** calling `super.onCreate`, because that call is what loads
//! this library and enters [`main`]. Reading it here through JNI is therefore
//! race-free by construction — the value is written before the library can run.
//!
//! That is deliberately the *only* JNI in the engine. Everything else — input,
//! rendering, audio, storage — goes through the same platform-neutral code the
//! desktop build uses.
//!
//! ## Failure is observable
//!
//! The same mechanism passes `KrkrGameActivity.pendingStateDir` (the app's
//! `filesDir`). Native code uses it for two files the launcher can read:
//! `krkr_state.json`, the startup-state file written by `engine::state`, and
//! `krkr.log`, the capped log file written by `engine::logging` (see those
//! modules for the exact shapes). A startup failure writes `failed` with the
//! real error and terminates the process; a crash leaves `starting`/`running`
//! behind because a process killed in native code cannot report anything
//! itself. Either way the launcher regains control instead of showing an
//! unexplained black surface.
#![cfg(target_os = "android")]

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use bevy::prelude::*;
use krkr_render::sync::SharedScene;
use tvp_visual::scene::Scene;

/// Static field on `dev.krkr.rs.KrkrGameActivity` holding the chosen folder.
const FIELD_GAME_DIR: &str = "pendingGameDir";
/// JNI signature of [`FIELD_GAME_DIR`].
const SIG_GAME_DIR: &str = "Ljava/lang/String;";
/// Static field on the same class holding the folder's display name (logging).
const FIELD_GAME_NAME: &str = "pendingGameName";
/// Static field on the same class holding the app's `filesDir`, where the
/// state file (`krkr_state.json`) and log file (`krkr.log`) live.
const FIELD_STATE_DIR: &str = "pendingStateDir";

/// The Android entry point.
///
/// `#[bevy_main]` expands to an `android_main` that stores the `AndroidApp` in
/// `bevy::android::ANDROID_APP` and then calls this function, so the app
/// handle is available here without any plumbing of our own.
#[bevy_main]
fn main() {
    // The state directory must be known before logging starts: the log file
    // lives in it. It is read through the same JNI helper as the game dir and
    // is likewise written before `super.onCreate`.
    if let Some(state_dir) = activity_static_string(FIELD_STATE_DIR, SIG_GAME_DIR) {
        engine::state::set_state_dir(state_dir);
    }
    engine::init_logging(false);
    // Do not clear a previous file: a crash leaves `starting`/`running`
    // behind so the launcher can tell it apart from a clean `stopped` exit.
    engine::state::write_state(engine::state::State::Starting, None);

    let Some(game_dir) = activity_static_string(FIELD_GAME_DIR, SIG_GAME_DIR) else {
        // The launcher never starts this Activity without a folder, so this is
        // a programming error rather than a user-facing condition. Record it
        // for the launcher and terminate instead of sitting on a black
        // surface with a live process.
        let message = format!(
            "no game directory was passed by KrkrGameActivity \
             ({FIELD_GAME_DIR} is unset) — cannot start the engine"
        );
        log::error!("krkr-rs: {message}");
        engine::state::write_state(engine::state::State::Failed, Some(&message));
        std::process::exit(1);
    };

    let name = activity_static_string(FIELD_GAME_NAME, SIG_GAME_DIR).unwrap_or_default();
    let game_dir = PathBuf::from(game_dir);
    log::info!("krkr-rs: launching {name:?} from {}", game_dir.display());

    let shared = SharedScene(Arc::new(RwLock::new(Scene::default())));
    krkr_render::runner::game_app(shared, game_dir, None).run();

    // The app returned normally (window closed or `System.exit`): a clean
    // exit, so the next launch does not look like a crash.
    engine::state::write_state(engine::state::State::Stopped, None);
}

/// Read a `String` static field off the Activity class through JNI.
///
/// The field is read from the *Activity's own class* (`GetObjectClass` on the
/// Activity) rather than via `FindClass`, because `FindClass` on a
/// native-attached thread resolves against the system class loader and would
/// not find application classes.
///
/// Returns `None` when the field is unset or any JNI step fails — every failure
/// is logged, so a silent black screen never happens without a reason in the
/// log. `Set`/`GetStaticField` on a class whose field is declared with Kotlin's
/// `@JvmField` is a plain field access with no getter involved.
fn activity_static_string(field: &str, signature: &str) -> Option<String> {
    let android_app = bevy::android::ANDROID_APP.get()?;

    // SAFETY: `ANDROID_APP` is set by the generated `android_main` and lives for
    // the whole process; `vm_as_ptr`/`activity_as_ptr` are the JavaVM and
    // Activity the process was started with, valid for that same lifetime.
    let vm = match unsafe { jni::JavaVM::from_raw(android_app.vm_as_ptr().cast()) } {
        Ok(vm) => vm,
        Err(e) => {
            log::error!("krkr-rs: cannot obtain the JavaVM: {e}");
            return None;
        }
    };

    // `android_main` runs on the Activity's native thread; attach it so JNI
    // calls are legal even if android-activity has not already done so.
    let mut env = match vm.attach_current_thread() {
        Ok(env) => env,
        Err(e) => {
            log::error!("krkr-rs: cannot attach to the JVM: {e}");
            return None;
        }
    };

    let activity = unsafe { jni::objects::JObject::from_raw(android_app.activity_as_ptr().cast()) };
    let class = match env.get_object_class(&activity) {
        Ok(class) => class,
        Err(e) => {
            log::error!("krkr-rs: cannot get the Activity class: {e}");
            return None;
        }
    };

    let value = match env.get_static_field(&class, field, signature) {
        Ok(value) => value,
        Err(e) => {
            log::error!("krkr-rs: cannot read {field}: {e}");
            return None;
        }
    };
    let object = match value.l() {
        Ok(object) if !object.is_null() => object,
        Ok(_) => return None,
        Err(e) => {
            log::error!("krkr-rs: {field} is not an object: {e}");
            return None;
        }
    };

    let string = jni::objects::JString::from(object);
    match env.get_string(&string) {
        Ok(value) => {
            let value: String = value.into();
            (!value.is_empty()).then_some(value)
        }
        Err(e) => {
            log::error!("krkr-rs: {field} is not a valid string: {e}");
            None
        }
    }
}
