//! krkr-rs Android composition root.
//!
//! `crates/app` is the desktop composition root and must not branch on the
//! operating system, and `platform` is L1: it may not call up into the
//! emulator. The OS entry point is therefore its own crate per host. This
//! crate owns `android_main`, the `GameActivity` event loop and the JNI reads
//! that hand the launcher's choice to the shared host; it contains no engine
//! logic. The desktop binary (`krkr`) and this library drive the same
//! `app::Host` and `app::GameHost` through the same `platform::EventHandler`.
//!
//! Two Android-specific rules live here rather than in the shared host:
//!
//! * **The loop.** `platform::run` is a desktop (winit) driver; the Android
//!   driver is `android-activity`, whose lifecycle and input arrive on the
//!   `android_main` thread. [`host::run_loop`] normalizes those callbacks into
//!   the same `PlatformEvent`s the winit backend emits.
//! * **The presentation default.** Phones always present the scene scaled
//!   uniformly to fit the screen, with the remainder letterboxed
//!   (`ScalingMode::Fit`), instead of showing the raw logical size. The
//!   desktop default stays `Game`, the reference's one-to-one Win32
//!   presentation; see `docs/android.md` §2.2.
//!
//! The state-file writer ([`state`]) is plain `std` and has no Android
//! dependency, so the host test target compiles and exercises it; the
//! Android-only modules are target-gated.

#![deny(unsafe_code)]
#![warn(missing_docs)]

#[cfg(target_os = "android")]
mod host;
#[cfg(target_os = "android")]
mod input;
#[cfg(target_os = "android")]
#[allow(unsafe_code)]
mod jni_fields;
#[cfg(any(target_os = "android", test))]
mod state;

#[cfg(target_os = "android")]
mod android_entry {
    use std::path::PathBuf;
    use std::time::Duration;

    use android_activity::AndroidApp;
    use api::config::{PresentationScaling, RuntimeConfig};
    use app::BackendKind;
    use engine::projection::ScalingMode;

    use crate::{host, jni_fields, state};

    /// The Android entry point.
    ///
    /// The symbol is resolved by the `GameActivity` glue, which the manifest
    /// names through `android.app.lib_name`. It uses the Rust ABI because the
    /// glue declares `extern "Rust" { fn android_main(app: AndroidApp); }`.
    ///
    /// The launcher's folder is read first so the engine has something to mount
    /// before any other work; a missing folder is reported through the state
    /// file and the loop is never started. The presentation default is chosen
    /// here: [`PresentationScaling::Fit`] is mapped to the engine's
    /// [`ScalingMode::Fit`] and carried into the host.
    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    pub fn android_main(app: AndroidApp) {
        // The state directory is read first: the state file and the logger are
        // then available for every later failure.
        if let Some(state_dir) = jni_fields::activity_static_string(
            &app,
            jni_fields::FIELD_STATE_DIR,
            jni_fields::SIG_STRING,
        ) {
            state::set_state_dir(state_dir);
        }
        init_logging();
        state::write_state(state::State::Starting, None);

        let game_dir = match jni_fields::activity_static_string(
            &app,
            jni_fields::FIELD_GAME_DIR,
            jni_fields::SIG_STRING,
        ) {
            Some(dir) => PathBuf::from(dir),
            None => {
                let message =
                    "no game directory was passed by KrkrGameActivity (pendingGameDir is unset)";
                log::error!("{message}");
                state::write_state(state::State::Failed, Some(message));
                return;
            }
        };
        let name = jni_fields::activity_static_string(
            &app,
            jni_fields::FIELD_GAME_NAME,
            jni_fields::SIG_STRING,
        )
        .unwrap_or_default();
        log::info!("krkr: launching {name:?} from {}", game_dir.display());

        // A phone always fits the scene rather than presenting the raw logical
        // size, so the default is set on this side of the composition root.
        let runtime = RuntimeConfig {
            scaling: PresentationScaling::Fit,
            ..RuntimeConfig::default()
        };
        let handler =
            host::AndroidHost::new(BackendKind::Vulkan, map_scaling(runtime.scaling), game_dir);
        host::run_loop(
            app,
            handler,
            Duration::from_secs_f64(runtime.frame_rate_limit.period_secs()),
        );

        state::write_state(state::State::Stopped, None);
    }

    /// Maps a resolved presentation scaling to the engine's projection mode.
    fn map_scaling(scaling: PresentationScaling) -> ScalingMode {
        match scaling {
            PresentationScaling::Game => ScalingMode::Game,
            PresentationScaling::Fit => ScalingMode::Fit,
            PresentationScaling::Fill => ScalingMode::Fill,
            PresentationScaling::Expand => ScalingMode::Expand,
        }
    }

    /// Installs the process logger.
    ///
    /// `android-activity` redirects the process's standard error to logcat, so
    /// a stderr logger is visible on the device. `RUST_LOG` selects the filter
    /// (default `info`). Initializing twice is a no-op.
    fn init_logging() {
        let mut builder =
            env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));
        builder.format_timestamp_millis();
        let _ = builder.try_init();
    }
}

#[cfg(target_os = "android")]
pub use android_entry::android_main;
