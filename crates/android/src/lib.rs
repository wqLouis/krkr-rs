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
//!   presentation; the user's saved scaling setting overrides either. See
//!   `docs/android.md` §2.2 and §3.2.
//!
//! The state-file writer ([`state`]), the log-file sink ([`log_file`]) and the
//! touch→mouse synthesizer ([`gesture`]) are plain `std` and name no Android
//! type, so the host test target compiles and exercises them; the Android-only
//! modules are target-gated.

#![deny(unsafe_code)]
#![warn(missing_docs)]

#[cfg(any(target_os = "android", test))]
mod config;
#[cfg(any(target_os = "android", test))]
mod gesture;
#[cfg(target_os = "android")]
mod host;
#[cfg(target_os = "android")]
mod input;
#[cfg(target_os = "android")]
#[allow(unsafe_code)]
mod jni_fields;
#[cfg(any(target_os = "android", test))]
mod log_file;
#[cfg(any(target_os = "android", test))]
mod state;

#[cfg(target_os = "android")]
mod android_entry {
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::Duration;

    use android_activity::AndroidApp;
    use api::config::PresentationScaling;
    use app::BackendKind;
    use engine::projection::ScalingMode;

    use crate::log_file::{self, FileSink};
    use crate::{config, host, jni_fields, state};

    /// The Android entry point.
    ///
    /// The symbol is resolved by the `GameActivity` glue, which the manifest
    /// names through `android.app.lib_name`. It uses the Rust ABI because the
    /// glue declares `extern "Rust" { fn android_main(app: AndroidApp); }`.
    ///
    /// The launcher's folder is read first so the engine has something to mount
    /// before any other work; a missing folder is reported through the state
    /// file and the loop is never started. The launcher's log level is read
    /// next and validated before the logger is installed, because it selects
    /// the filter. The remaining saved runtime settings (frame-rate limit,
    /// presentation scaling and archive preload) are read over the same JNI
    /// channel and resolved by [`crate::config`]: the presentation default is
    /// [`PresentationScaling::Fit`] and the user's stored choice overrides it,
    /// and an out-of-range value is reported and the loop is never started.
    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    pub fn android_main(app: AndroidApp) {
        // The state directory is read first: the state file and the logger are
        // then available for every later failure. A read failure is not the
        // same as an unset field, but with no directory there is nowhere to
        // write the state file, so stderr is the only channel here.
        match jni_fields::activity_static_string(
            &app,
            jni_fields::FIELD_STATE_DIR,
            jni_fields::SIG_STRING,
        ) {
            Ok(Some(state_dir)) => state::set_state_dir(state_dir),
            Ok(None) => {}
            Err(error) => {
                eprintln!("krkr-rs: cannot read KrkrGameActivity.pendingStateDir: {error}");
            }
        }
        // The log level is the launcher's choice and the filter is fixed when
        // the logger is installed, so it is read and validated first. This is
        // the one settings read that happens before the logger exists: a read
        // failure or an unknown level is echoed on stderr and written to the
        // state file, which is what the launcher reads. A missing value means
        // the launcher published no override, so the built-in default applies.
        let log_level = match read_settings_field(&app, jni_fields::FIELD_LOG_LEVEL) {
            Ok(value) => match config::resolve_log_level(value.as_deref()) {
                Ok(level) => level,
                Err(error) => {
                    let message = format!("invalid runtime configuration: {error}");
                    eprintln!("krkr-rs: {message}");
                    state::write_state(state::State::Failed, Some(&message));
                    return;
                }
            },
            Err(message) => {
                eprintln!("krkr-rs: {message}");
                state::write_state(state::State::Failed, Some(&message));
                return;
            }
        };
        if let Err(error) = init_logging(log_level) {
            // The launcher reads a file, not logcat: without it the Logs screen
            // is empty and a later failure has no explanation. A log the app
            // cannot open is therefore a reported startup failure, never a
            // silent fall back to stderr only.
            let message = format!("cannot start the engine log: {error}");
            eprintln!("krkr-rs: {message}");
            state::write_state(state::State::Failed, Some(&message));
            return;
        }
        state::write_state(state::State::Starting, None);

        let game_dir = match jni_fields::activity_static_string(
            &app,
            jni_fields::FIELD_GAME_DIR,
            jni_fields::SIG_STRING,
        ) {
            Ok(Some(dir)) => PathBuf::from(dir),
            Ok(None) => {
                report_startup_failure(
                    "no game directory was passed by KrkrGameActivity (pendingGameDir is unset)",
                );
                return;
            }
            Err(error) => {
                report_startup_failure(&format!(
                    "cannot read KrkrGameActivity.pendingGameDir: {error}"
                ));
                return;
            }
        };
        let name = match jni_fields::activity_static_string(
            &app,
            jni_fields::FIELD_GAME_NAME,
            jni_fields::SIG_STRING,
        ) {
            Ok(Some(name)) => name,
            Ok(None) => String::new(),
            // The name only decorates a log line; a read failure there is not
            // fatal, but it is logged rather than silently blanked.
            Err(error) => {
                log::warn!("cannot read KrkrGameActivity.pendingGameName: {error}");
                String::new()
            }
        };
        log::info!("krkr: launching {name:?} from {}", game_dir.display());

        // The launcher persisted these choices in its own settings store and
        // published them over JNI. Resolving them here keeps configuration
        // loading the launcher's business: the emulator only receives the
        // finished value. An unparseable or out-of-range value is a typed
        // error, never a fall back to the default. A JNI read *failure* is
        // likewise reported rather than treated as "unset": only a
        // null/empty field means "no override", and only that takes the
        // default.
        let frame_rate = match read_settings_field(&app, jni_fields::FIELD_FRAME_RATE) {
            Ok(value) => value,
            Err(message) => {
                report_startup_failure(&message);
                return;
            }
        };
        let scaling = match read_settings_field(&app, jni_fields::FIELD_SCALING) {
            Ok(value) => value,
            Err(message) => {
                report_startup_failure(&message);
                return;
            }
        };
        let preload_archives = match read_settings_field(&app, jni_fields::FIELD_PRELOAD_ARCHIVES) {
            Ok(value) => value,
            Err(message) => {
                report_startup_failure(&message);
                return;
            }
        };
        let runtime = match config::resolve_runtime_config(
            frame_rate.as_deref(),
            scaling.as_deref(),
            preload_archives.as_deref(),
        ) {
            Ok(runtime) => runtime,
            Err(error) => {
                log::error!("invalid runtime configuration: {error}");
                let message = error.to_string();
                state::write_state(state::State::Failed, Some(&message));
                return;
            }
        };
        log::info!(
            "krkr: frame rate {} fps, scaling {}, preload archives {}, log level {}",
            runtime.frame_rate_limit.fps(),
            runtime.scaling.as_str(),
            runtime.preload_archives,
            log_level.as_filter()
        );
        let handler = host::AndroidHost::new(
            BackendKind::Vulkan,
            map_scaling(runtime.scaling),
            game_dir,
            runtime.preload_archives,
        );
        let outcome = host::run_loop(
            app,
            handler,
            Duration::from_secs_f64(runtime.frame_rate_limit.period_secs()),
        );

        // The terminal write reports a recorded failure, or a clean stop when
        // the loop recorded none; a later `stopped` must never erase a failure.
        let (terminal, message) = outcome.terminal();
        state::write_state(terminal, message);
    }

    /// Reads one optional settings field, turning a JNI read failure into the
    /// option it belongs to instead of treating it as "unset".
    fn read_settings_field(app: &AndroidApp, field: &str) -> Result<Option<String>, String> {
        jni_fields::activity_static_string(app, field, jni_fields::SIG_STRING)
            .map_err(|error| format!("cannot read KrkrGameActivity.{field}: {error}"))
    }

    /// Reports a startup failure through the state file and the log.
    fn report_startup_failure(message: &str) {
        log::error!("{message}");
        state::write_state(state::State::Failed, Some(message));
    }

    /// Maps a resolved presentation scaling to the engine's projection mode.
    fn map_scaling(scaling: PresentationScaling) -> ScalingMode {
        match scaling {
            PresentationScaling::Game => ScalingMode::Game,
            PresentationScaling::Fit => ScalingMode::Fit,
            PresentationScaling::FitSharp => ScalingMode::FitSharp,
            PresentationScaling::Fill => ScalingMode::Fill,
            PresentationScaling::Expand => ScalingMode::Expand,
        }
    }

    /// Installs the process logger.
    ///
    /// The single global logger fans every record out to two sinks:
    ///
    /// * `env_logger` on stderr, which `android-activity` redirects to logcat
    ///   (`RUST_LOG` selects the filter when it is set; otherwise the launcher's
    ///   log level does);
    /// * [`FileSink`] on `<filesDir>/krkr.log`, the file the launcher's Logs
    ///   screen reads. It is capped at [`log_file::LOG_MAX_BYTES`] and keeps
    ///   [`log_file::LOG_ROTATED_FILES`] rotations, so a long or failure-heavy
    ///   session cannot fill the device's storage.
    ///
    /// `RUST_LOG` keeps its precedence for anyone who can set it; a device
    /// cannot, so the launcher's level is what a device uses. `level` is the
    /// launcher's choice; a device needs `debug` to surface the VM's exception
    /// dumps.
    ///
    /// The file is opened before the logger is claimed. An unusable log file
    /// is returned as an error for [`android_main`] to report through the
    /// state file, rather than a fallback that leaves the Logs screen empty.
    /// A missing state directory is the same error: without it there is no
    /// place for the file the launcher reads.
    fn init_logging(level: config::LogLevel) -> std::io::Result<()> {
        let Some(dir) = state::state_dir() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "KrkrGameActivity.pendingStateDir is unset; the log has nowhere to go",
            ));
        };
        let file = FileSink::open(dir.join(log_file::LOG_FILE_NAME))?;

        let mut builder = env_logger::Builder::from_env(
            env_logger::Env::default().default_filter_or(level.as_filter()),
        );
        builder.format_timestamp_millis();
        let stderr = builder.build();
        let max_level = stderr.filter();
        let logger = FanoutLog {
            stderr,
            file: Mutex::new(Some(file)),
        };
        match log::set_boxed_logger(Box::new(logger)) {
            Ok(()) => {
                log::set_max_level(max_level);
                Ok(())
            }
            // The global logger is process-wide, so this can only mean an
            // earlier `android_main` in this same process already installed it:
            // `android-activity` calls `android_main` once per Activity
            // instance and the launcher and the engine share the process. The
            // existing logger already fans every record into the same
            // `krkr.log`, so a second launch keeps logging instead of failing.
            // There is no other process that could own this process's logger;
            // a genuine setup failure (an unopenable log file) was returned
            // above, before the claim.
            Err(_) => {
                log::debug!("the engine log is already installed for this process");
                Ok(())
            }
        }
    }

    /// The one global logger: the `env_logger` stderr sink plus the capped
    /// file. `log` records are filtered once, by the `env_logger` filter, so
    /// the file holds exactly what stderr holds.
    struct FanoutLog {
        stderr: env_logger::Logger,
        file: Mutex<Option<FileSink>>,
    }

    impl FanoutLog {
        /// Append `record` to the file, if a sink is open.
        fn write_file(&self, record: &log::Record<'_>) {
            // A panic while logging poisons the mutex but must not stop the
            // record from reaching the file.
            let mut guard = match self.file.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            if let Some(sink) = guard.as_mut() {
                sink.write_line(&log_file::format_record(record));
            }
        }
    }

    impl log::Log for FanoutLog {
        fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
            log::Log::enabled(&self.stderr, metadata)
        }

        fn log(&self, record: &log::Record<'_>) {
            if !self.stderr.matches(record) {
                return;
            }
            log::Log::log(&self.stderr, record);
            self.write_file(record);
        }

        fn flush(&self) {
            log::Log::flush(&self.stderr);
            let mut guard = match self.file.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            if let Some(sink) = guard.as_mut() {
                sink.flush();
            }
        }
    }
}

#[cfg(target_os = "android")]
pub use android_entry::android_main;
