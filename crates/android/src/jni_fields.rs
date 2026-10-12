//! Reads the static fields `KrkrGameActivity` publishes before
//! `super.onCreate` returns.
//!
//! The launcher starts the engine Activity with an Intent extra and, before
//! `super.onCreate` loads this library, copies that value onto a static field.
//! `android_main` therefore always observes the field already set, with no
//! race and no extra IPC. This is the only JNI in the engine: everything else
//! goes through the platform-neutral code the desktop build uses.
//!
//! [`pendingGameDir`](FIELD_GAME_DIR) and [`pendingGameName`](FIELD_GAME_NAME)
//! are written by `KrkrGameActivity.onCreate`; [`pendingStateDir`](FIELD_STATE_DIR)
//! is its `filesDir.absolutePath`, where [`crate::state`] writes
//! `krkr_state.json`. [`pendingFrameRate`](FIELD_FRAME_RATE),
//! [`pendingScaling`](FIELD_SCALING) and
//! [`pendingPreloadArchives`](FIELD_PRELOAD_ARCHIVES) are the launcher's
//! persisted runtime settings; [`crate::config`] validates them into the
//! `RuntimeConfig` the emulator receives.

use android_activity::AndroidApp;

/// Static field on `dev.krkr.rs.KrkrGameActivity` holding the chosen folder.
pub(crate) const FIELD_GAME_DIR: &str = "pendingGameDir";
/// Static field on the same class holding the folder's display name.
pub(crate) const FIELD_GAME_NAME: &str = "pendingGameName";
/// Static field on the same class holding the app's `filesDir`.
pub(crate) const FIELD_STATE_DIR: &str = "pendingStateDir";
/// Static field on the same class holding the frame-rate limit, as a decimal
/// integer; unset means the launcher had no override.
pub(crate) const FIELD_FRAME_RATE: &str = "pendingFrameRate";
/// Static field on the same class holding the presentation scaling name
/// (`game`/`fit`/`fill`/`expand`); unset means the launcher had no override.
pub(crate) const FIELD_SCALING: &str = "pendingScaling";
/// Static field on the same class holding the archive-preload choice (`true`
/// or `false`); unset means the launcher had no override.
pub(crate) const FIELD_PRELOAD_ARCHIVES: &str = "pendingPreloadArchives";
/// JNI signature of every field above (all are `java.lang.String`).
pub(crate) const SIG_STRING: &str = "Ljava/lang/String;";

/// Reads a `String` static field off the Activity's own class.
///
/// The field is read from the Activity instance's class rather than through
/// `FindClass`, because `FindClass` on a native-attached thread resolves
/// against the system class loader and would not find the application class.
///
/// `Ok(None)` means the field is null or empty — the launcher published no
/// value — while `Err` means a JNI step failed and the value, if present, could
/// not be read. Callers must keep those two cases apart: an absent settings
/// value takes its default, a read failure is reported like a missing game
/// directory rather than silently defaulted. Every failure is logged and any
/// pending Java exception is cleared, so the `android_main` thread is left
/// clean for the loop's later JNI use.
#[allow(unsafe_code)]
pub(crate) fn activity_static_string(
    app: &AndroidApp,
    field: &str,
    signature: &str,
) -> Result<Option<String>, jni::errors::Error> {
    // SAFETY: `AndroidApp` owns the VM and Activity pointers for the lifetime
    // of `android_main`, which is the only caller.
    let vm = match unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) } {
        Ok(vm) => vm,
        Err(error) => {
            log::error!("cannot obtain the JavaVM: {error}");
            return Err(error);
        }
    };
    let mut env = match vm.attach_current_thread() {
        Ok(env) => env,
        Err(error) => {
            log::error!("cannot attach the native thread to the JVM: {error}");
            return Err(error);
        }
    };
    // SAFETY: the Activity pointer is a global reference owned by the app; the
    // bare `JObject` wrapper does not delete it.
    let activity = unsafe { jni::objects::JObject::from_raw(app.activity_as_ptr().cast()) };
    let class = match env.get_object_class(&activity) {
        Ok(class) => class,
        Err(error) => return report(&mut env, field, error),
    };
    let value = match env.get_static_field(&class, field, signature) {
        Ok(value) => value,
        Err(error) => return report(&mut env, field, error),
    };
    let object = match value.l() {
        Ok(object) if !object.is_null() => object,
        Ok(_) => return Ok(None),
        Err(error) => return report(&mut env, field, error),
    };
    let string = jni::objects::JString::from(object);
    match env.get_string(&string) {
        Ok(value) => {
            let value: String = value.into();
            Ok((!value.is_empty()).then_some(value))
        }
        Err(error) => report(&mut env, field, error),
    }
}

/// Logs a failed JNI step, clears the pending exception and yields the error.
fn report<T>(
    env: &mut jni::JNIEnv<'_>,
    field: &str,
    error: jni::errors::Error,
) -> Result<T, jni::errors::Error> {
    log::error!("cannot read KrkrGameActivity.{field}: {error}");
    // The jni crate leaves the Java exception pending; clear it so every later
    // JNI call on this thread is not poisoned.
    let _ = env.exception_clear();
    Err(error)
}
