//! Real-game frontend: mount an explicit archive list, run the L2 startup
//! sequence through the L4 VM host, and drive the VM frame body together with
//! the L2 render host.
//!
//! Loading is explicit: the caller
//! supplies the `--archive` list and base directory; nothing scans a folder
//! for `*.xp3` or `startup.tjs`. [`GameHost`] is the composition-root
//! counterpart of the empty-scene [`Host`]: it adds the VM host, the mounted
//! game [`Storage`] and the ordered frame body.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use api::error::FsError;
use api::fs::FsPath;
use compat::{CompatCtx, CompatError, FrameBody};
use engine::Storage;
use engine::storage::{MountOptions, StartupPlan, StorageReadError};
use fs::{FileSystem, list};
use platform::{FramePacing, PlatformError, PlatformEvent};
use vm::{FrameCtx, VmHost};

use crate::audio::AudioOutput;
use crate::startup::{StartupSnapshot, StartupStats};
use crate::{Host, HostError, StepReport};

/// A frame whose wall-clock gap reaches this many milliseconds logs its phase
/// breakdown once, at `warn`.
///
/// The threshold matches the audio drain's playback cap
/// (`crate::audio::DEFAULT_MAX_CHUNK_MS`), so the frame that produces the
/// existing `audio: ... exceeds the ... playback cap` warning also produces
/// the attribution line that explains it.
const SLOW_FRAME_MS: f64 = 250.0;

/// Errors surfaced while opening or stepping a real game.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GameError {
    /// Mounting the explicit archive list failed.
    #[error("game: {0}")]
    Mount(#[from] engine::storage::MountError),
    /// A storage probe failed while resolving the startup sequence.
    #[error("game: {0}")]
    Storage(#[from] engine::storage::StorageReadError),
    /// The searched `startup.tjs` failed and no `system/Initialize.tjs`
    /// fallback exists.
    #[error("startup: {script}: {error}")]
    Startup {
        /// The startup script that failed.
        script: String,
        /// The script error.
        error: String,
    },
    /// No `startup.tjs` (and no `system/Initialize.tjs` fallback) was found.
    #[error("startup: no startup.tjs found")]
    NoStartup,
    /// A render action failed.
    #[error(transparent)]
    Host(#[from] HostError),
    /// The L1 audio device could not be opened or written (typed, never
    /// silent; headless hosts use the null backend instead).
    #[error(transparent)]
    Audio(#[from] PlatformError),
    /// The VM failed to start or run a frame.
    #[error(transparent)]
    Vm(#[from] vm::VmError),
    /// Auto-loading a `.tpm` plugin failed.
    #[error("plugin: {0}")]
    Plugin(#[from] CompatError),
}

/// Per-frame wall-clock costs, in milliseconds, recorded by the frame body.
///
/// The fields are overwritten by each poll's completion hook, so one instance
/// describes exactly one frame. [`FramePhases::body_ms`] is the sum of the six
/// poll phases; the render host's own costs are not part of it.
#[derive(Clone, Copy, Default)]
struct FramePhases {
    timer_ms: f64,
    sound_ms: f64,
    video_ms: f64,
    async_trigger_ms: f64,
    continuous_handler_ms: f64,
    project_and_render_ms: f64,
    body_ms: f64,
}

impl FramePhases {
    /// Replaces the recorded cost of one phase and keeps the body sum.
    fn record(field: &mut f64, body_ms: &mut f64, elapsed: Duration) {
        let ms = elapsed.as_secs_f64() * 1000.0;
        *body_ms += ms;
        *field = ms;
    }
}

/// The ordered frame-body polls the game host runs each frame (doc 08 §6).
#[derive(Default)]
struct Polls {
    /// Elapsed milliseconds since the previous frame, forwarded to the layer
    /// transition step.
    dt_ms: f64,
    /// Wall-clock cost of each poll in the current frame.
    phases: FramePhases,
}

impl FrameBody for Polls {
    fn timer_poll(&mut self, ctx: &mut CompatCtx) -> Result<(), CompatError> {
        compat::timer_poll(ctx).map(|_fired| ())
    }

    fn sound_poll(&mut self, ctx: &mut CompatCtx) -> Result<(), CompatError> {
        compat::sound_poll(ctx)
    }

    fn video_poll(&mut self, ctx: &mut CompatCtx) -> Result<(), CompatError> {
        compat::video_advance(ctx, self.dt_ms)
    }

    fn async_trigger_poll(&mut self, ctx: &mut CompatCtx) -> Result<(), CompatError> {
        compat::async_trigger_poll(ctx)
    }

    fn continuous_handler_poll(&mut self, ctx: &mut CompatCtx) -> Result<(), CompatError> {
        compat::continuous_handler_poll(ctx).map(|_remaining| ())
    }

    #[allow(unsafe_code)]
    fn project_and_render(&mut self, ctx: &mut CompatCtx) -> Result<(), CompatError> {
        // The movie frame body advances its overlays and copies the current
        // frame into the layer before the transition step composites it
        // (reference `layerExMovie::OnContinuousCallback`).
        compat::movie_poll(ctx, self.dt_ms)?;
        // The frame body owns the per-frame transition step; the remaining
        // hand-offs are host drains that the render host consumes after this
        // poll returns.
        compat::advance_transitions(ctx, self.dt_ms).map_err(CompatError::Engine)?;
        // SAFETY: `ctx` is bound to a live engine for the callback.
        let _ = compat::take_bring_to_front_requests(unsafe { ctx.services() });
        let _ = compat::take_update_requests(unsafe { ctx.services() });
        let _ = compat::take_capture_releases(ctx);
        Ok(())
    }

    fn timer_completed(&mut self, elapsed: Duration) {
        FramePhases::record(&mut self.phases.timer_ms, &mut self.phases.body_ms, elapsed);
    }

    fn sound_completed(&mut self, elapsed: Duration) {
        FramePhases::record(&mut self.phases.sound_ms, &mut self.phases.body_ms, elapsed);
    }

    fn video_completed(&mut self, elapsed: Duration) {
        FramePhases::record(&mut self.phases.video_ms, &mut self.phases.body_ms, elapsed);
    }

    fn async_trigger_completed(&mut self, elapsed: Duration) {
        FramePhases::record(
            &mut self.phases.async_trigger_ms,
            &mut self.phases.body_ms,
            elapsed,
        );
    }

    fn continuous_handler_completed(&mut self, elapsed: Duration) {
        FramePhases::record(
            &mut self.phases.continuous_handler_ms,
            &mut self.phases.body_ms,
            elapsed,
        );
    }

    fn project_and_render_completed(&mut self, elapsed: Duration) {
        FramePhases::record(
            &mut self.phases.project_and_render_ms,
            &mut self.phases.body_ms,
            elapsed,
        );
    }
}

/// The previous frame's measured attribution, kept so a slow gap names the
/// frame it actually spans.
///
/// The gap handed to a step is the interval since the previous step started,
/// so the phase and host costs that explain it belong to the previous frame,
/// not the one running now. One instance holds exactly that previous frame
/// until the next step replaces it.
#[derive(Default)]
struct FrameAttribution {
    /// The previous frame's poll-phase costs.
    phases: FramePhases,
    /// The previous frame's host step report.
    report: StepReport,
    /// The previous frame's GPU call costs, when the backend reports them.
    timings: Option<render::FrameTimings>,
    /// The previous frame's audio drain cost.
    audio_ms: f64,
    /// The previous frame's whole wall-clock cost.
    total_ms: f64,
    /// Whether a frame has been attributed yet.
    valid: bool,
}

impl FrameAttribution {
    /// Records the frame that just completed.
    fn record(
        &mut self,
        phases: FramePhases,
        report: StepReport,
        timings: Option<render::FrameTimings>,
        audio_ms: f64,
        total_ms: f64,
    ) {
        self.phases = phases;
        self.report = report;
        self.timings = timings;
        self.audio_ms = audio_ms;
        self.total_ms = total_ms;
        self.valid = true;
    }

    /// Formats the single slow-frame line for `gap_ms`, or `None` when the
    /// gap is below the threshold or no previous frame exists yet.
    ///
    /// The named phases and host/GPU costs are the previous frame's, because
    /// the gap spans it. `platform_wait` and `input` are this step's
    /// measurement; `unaccounted` is `gap - previous total - platform wait`,
    /// so a non-zero value is loop overhead or a stall outside both the
    /// frame and the event poll.
    fn slow_frame_line(&self, gap_ms: f64, pacing: FramePacing) -> Option<String> {
        if !self.valid || gap_ms < SLOW_FRAME_MS {
            return None;
        }
        let phases = &self.phases;
        let report = &self.report;
        let timings = self.timings.unwrap_or_default();
        let unaccounted = gap_ms - self.total_ms - pacing.poll_ms;
        Some(format!(
            "frame {} slow: gap={gap_ms:.1} ms since the previous frame; \
             the gap spans the previous frame, so these are its phases: \
             timer={:.1} sound={:.1} video={:.1} async={:.1} continuous={:.1} \
             project_and_render={:.1} body={:.1}; its host: audio={:.1} \
             host_sync={:.1} host_project={:.1} host_render={:.1} total={:.1}; \
             its gpu: acquire={:.1} present={:.1} recreate={:.1} fence_wait={:.1}; \
             this wait: platform_wait={:.1} input={:.1} unaccounted={unaccounted:.1} ms",
            report.frame_index,
            phases.timer_ms,
            phases.sound_ms,
            phases.video_ms,
            phases.async_trigger_ms,
            phases.continuous_handler_ms,
            phases.project_and_render_ms,
            phases.body_ms,
            self.audio_ms,
            report.sync_ms,
            report.project_ms,
            report.render_ms,
            self.total_ms,
            timings.acquire_ms,
            timings.present_ms,
            timings.recreate_ms,
            timings.fence_wait_ms,
            pacing.poll_ms,
            pacing.input_ms,
        ))
    }
}

/// A running game: the L4 VM host bound to the render host's scene and the
/// mounted game storage.
///
/// Field order is load-bearing. `audio` is declared first so the L1 device is
/// stopped (its state drop stops the stream) **before** the VM dies; the VM
/// then holds raw pointers to the [`Storage`] and to the
/// [`Scene`](engine::Scene) inside `host`, so `vm` is declared before its
/// targets, and the [`Host`] is boxed so its address (and thus the scene
/// pointer) is stable across the move into this struct.
pub struct GameHost {
    /// The L1 audio device drained to the L3 mixer once per frame; dropped
    /// first, so the stream stops before the engine tears down.
    audio: AudioOutput,
    /// The L4 VM host, bound to `storage` and `host.scene`.
    vm: VmHost,
    /// The mounted game archives, read by the `Storages` natives.
    storage: Box<Storage>,
    /// The L2 render host, boxed for a stable scene address.
    host: Box<Host>,
    /// The ordered frame body.
    polls: Polls,
    /// Monotonic frame clock fed to the VM frame body.
    now_ms: u64,
    /// Wall-clock instant the open sequence started.
    open_start: Instant,
    /// The previous frame's measured attribution, reported when the next gap
    /// is slow.
    last_frame: FrameAttribution,
    /// Whether the first frame's timing has been logged.
    first_frame_logged: bool,
}

impl GameHost {
    /// Mounts `archives` in order (later archives win), binds the VM to
    /// `host`'s scene and the storage, installs the L1 codec seams, and runs
    /// the L2 startup sequence.
    ///
    /// `base_dir` is the app path (loose files, `patch.tjs`,
    /// `AfterStartup.tjs`). Every mounted archive keeps its file open and is
    /// read positionally per entry; use [`Self::open_with_preload`] to hold
    /// the archives in memory instead.
    ///
    /// # Errors
    ///
    /// Returns [`GameError`] when the audio device cannot be opened (typed,
    /// headless hosts use the null backend and cannot fail here), when
    /// an archive cannot be mounted, the VM cannot be created, or the startup
    /// sequence fails.
    pub fn open(host: Host, archives: Vec<PathBuf>, base_dir: PathBuf) -> Result<Self, GameError> {
        Self::open_with_preload(host, archives, base_dir, false)
    }

    /// Like [`Self::open`], but reads every mounted archive into memory in
    /// full at mount when `preload_archives` is set.
    ///
    /// The resident-memory cost is the sum of the mounted archives' sizes and
    /// there is no cap; the mount order and precedence are unchanged. See
    /// [`engine::storage::MountOptions`].
    ///
    /// # Errors
    ///
    /// As [`Self::open`], plus a typed error when a preloaded archive cannot
    /// be read into memory in full.
    #[allow(unsafe_code)]
    pub fn open_with_preload(
        host: Host,
        archives: Vec<PathBuf>,
        base_dir: PathBuf,
        preload_archives: bool,
    ) -> Result<Self, GameError> {
        let open_start = Instant::now();
        let stats = StartupStats::new();
        // L1 audio first: a windowed host opens the real device and fails
        // fast if none exists; a headless host gets the null backend.
        let audio = AudioOutput::open(host.is_headless())?;
        let mut host = Box::new(host);
        let fs = FileSystem::host(base_dir.clone());
        // A directory-only launch (no explicit `--archive`) mounts every
        // `*.xp3` found under the base directory
        // (`engine::storage::discover_archives`), so the CLI and the Android
        // launcher can take the game folder instead of each archive. A
        // scanned archive that cannot be read is skipped with a warning; an
        // explicitly requested one is a typed error.
        let mount_started = Instant::now();
        let storage = Box::new(if archives.is_empty() {
            let discovered = engine::storage::discover_archives(&fs, &base_dir)?;
            Storage::mount_discovered_archives_with(
                fs,
                &base_dir,
                discovered,
                MountOptions { preload_archives },
            )?
        } else {
            Storage::mount_archives_with(
                fs,
                &base_dir,
                archives,
                MountOptions { preload_archives },
            )?
        });
        let mount_time = mount_started.elapsed();
        let scene_ptr = host.scene_mut() as *mut engine::Scene;
        let storage_ptr = storage.as_ref() as *const Storage as *mut Storage;
        // SAFETY: `storage` and `host` are declared after `vm` in the returned
        // struct and therefore outlive it; the boxed `host` keeps the scene
        // address stable, and nothing else aliases either value while the
        // binding is installed. Attaching before the compat surface is
        // installed lets a registration-time service read (the `xp3filter`
        // `xp3filter.tjs` read) see the mounted archives.
        let vm = VmHost::with_default_services(|services| unsafe {
            services.attach_scene(scene_ptr);
            services.attach_storage(storage_ptr);
        })?;

        // Install the seams the compat natives compose: the L1 codecs and the
        // emulated Windows host facts. The codecs clone their backend, so the
        // `media` local may drop here.
        let media = media::Media::new();
        crate::install_audio_decoder_metered(vm.compat(), &media, &stats);
        crate::install_image_codec_metered(vm.compat(), &media, &storage, &stats);
        // `fontdb`'s own system-font scan excludes Android, so the host's font
        // directories are supplied here, before a startup script can resolve a
        // face.
        vm.compat()
            .set_system_font_dirs(platform::system_font_dirs());
        // SAFETY: `storage` is declared after `vm` in the returned struct, so
        // it outlives the compat host that holds the installed source.
        unsafe { crate::install_video_source_metered(vm.compat(), &media, &storage, &stats) }
            .map_err(vm::VmError::from)?;
        crate::install_window_ex_facts(vm.compat(), host.surface_size());

        // The reference auto-loads `.tpm` plugins from the exe/system/plugin
        // folders before the startup scripts run
        // (`reference/cpp/core/plugin/PluginImpl.cpp:148-177`, called from
        // `reference/cpp/core/environ/Application.cpp:342`). The composition
        // root enumerates with the L1 file system and compat links the ordered
        // names.
        let tpm_plugins = discover_tpm_plugins(&FileSystem::host(base_dir.clone()), &base_dir)?;
        if !tpm_plugins.is_empty() {
            let mut ctx = vm.compat().ctx()?;
            vm.compat().registry().autoload(&tpm_plugins, &mut ctx)?;
        }

        let plan = storage.startup_plan()?;
        let startup_started = Instant::now();
        // Complete the deferred, service-dependent half of plugin
        // registration before the startup scripts run; a read failure stops
        // the open instead of running unfiltered.
        vm.compat().activate_deferred_plugins()?;
        vm.compat().begin_native_timing();
        let startup_result = run_startup(&vm, &storage, &plan, &stats);
        vm.compat().end_native_timing();
        startup_result?;
        let startup_time = startup_started.elapsed();
        log_startup(&storage, &stats, mount_time, startup_time);
        Ok(Self {
            audio,
            vm,
            storage,
            host,
            polls: Polls::default(),
            now_ms: 0,
            open_start,
            last_frame: FrameAttribution::default(),
            first_frame_logged: false,
        })
    }

    /// The L2 render host.
    pub fn host(&self) -> &Host {
        &self.host
    }

    /// The L2 render host, mutably (the platform loop resizes it).
    pub fn host_mut(&mut self) -> &mut Host {
        &mut self.host
    }

    /// The mounted game storage (the VM reads it through the `Storages`
    /// natives).
    pub fn storage(&self) -> &Storage {
        &self.storage
    }

    /// The TVP layer system the VM mutates.
    pub fn scene(&self) -> &engine::Scene {
        self.host.scene()
    }

    /// Runs one frame: the VM frame body (events, polls, transitions), then
    /// renders and presents.
    ///
    /// The L1 audio drain runs between the two: after `tick` a dt-derived
    /// chunk of the mixer is rendered through `compat::sound_render` (which
    /// advances the mixer by the frames it produces) and written to the
    /// device (policy documented in the `audio` module).
    ///
    /// # Errors
    ///
    /// Returns [`GameError`] when the VM frame body fails, when the audio
    /// pump cannot reach the bound compat context or the device rejects a
    /// write, or when the render action fails.
    ///
    /// Returns the L1 [`StepReport`] so a headless run can print the draw
    /// counters (they exist on the null backend only, see [`StepReport::stats`]);
    /// the windowed loop ignores them.
    pub fn step(&mut self, dt_ms: f32, events: &[PlatformEvent]) -> Result<StepReport, GameError> {
        self.step_with_pacing(dt_ms, events, FramePacing::default())
    }

    /// Runs one frame exactly like [`Self::step`], but attributes a slow gap
    /// with the platform loop's own `pacing` measurement.
    ///
    /// The line a slow gap produces names the **previous** frame: the gap is
    /// the interval since that frame started, so its phases and host/GPU
    /// costs are what explain the gap, while the newly-finished frame's own
    /// costs are reported by the next slow line. `platform_wait` and `input`
    /// are this step's wait, and `unaccounted` closes the budget.
    ///
    /// # Errors
    ///
    /// As [`Self::step`].
    pub fn step_with_pacing(
        &mut self,
        dt_ms: f32,
        events: &[PlatformEvent],
        pacing: FramePacing,
    ) -> Result<StepReport, GameError> {
        let dt = f64::from(dt_ms.max(0.0));
        // The gap spans the previous frame, so attribute it before this step
        // overwrites the stored frame.
        if let Some(line) = self.last_frame.slow_frame_line(dt, pacing) {
            log::warn!("{line}");
        }
        self.polls.dt_ms = dt;
        self.polls.phases = FramePhases::default();
        self.now_ms = self.now_ms.saturating_add(dt as u64);
        let frame = FrameCtx {
            now_ms: self.now_ms,
            dt_ms: dt as u32,
            body: &mut self.polls,
            events,
        };
        let step_started = Instant::now();
        self.vm.tick(frame)?;
        let audio_started = Instant::now();
        self.audio.pump(&self.vm, dt)?;
        let audio_ms = audio_started.elapsed().as_secs_f64() * 1000.0;
        let report = self.host.step(dt_ms)?;
        let total_ms = step_started.elapsed().as_secs_f64() * 1000.0;

        self.last_frame.record(
            self.polls.phases,
            report,
            self.host.last_frame_timings(),
            audio_ms,
            total_ms,
        );

        if !self.first_frame_logged {
            self.first_frame_logged = true;
            log::info!(
                "startup: first frame {:.1} ms after open",
                self.open_start.elapsed().as_secs_f64() * 1000.0
            );
        }
        Ok(report)
    }
}

/// Logs the start-up phase aggregates once, at `info`.
///
/// `mount_time` covers the archive mount and the XP3 index parse (the index
/// is parsed inside the mount call), `entries` is the summed index entry
/// count and `bytes` the summed archive size. `stats` carries the decode and
/// script counters the installed seams recorded while the startup scripts
/// ran.
fn log_startup(
    storage: &Storage,
    stats: &StartupStats,
    mount_time: Duration,
    startup_time: Duration,
) {
    let mut entries: usize = 0;
    let mut bytes: u64 = 0;
    storage.for_each_archive(|path, archive| {
        entries = entries.saturating_add(archive.len());
        match fs::stat(storage.file_system(), &FsPath::host(path)) {
            Ok(stat) => bytes = bytes.saturating_add(stat.size),
            Err(error) => {
                log::warn!("startup: cannot size archive {}: {error}", path.display());
            }
        }
    });
    let snapshot: StartupSnapshot = stats.snapshot();
    log::info!(
        "startup: archives mounted count={} entries={entries} bytes={bytes} in {:.1} ms",
        storage.archive_count(),
        mount_time.as_secs_f64() * 1000.0,
    );
    log::info!(
        "startup: images decoded count={} in {:.1} ms",
        snapshot.images_decoded,
        snapshot.image_decode.as_secs_f64() * 1000.0,
    );
    log::info!(
        "startup: decoders opened audio={} video={}",
        snapshot.audio_decoders_opened,
        snapshot.video_decoders_opened,
    );
    log::info!(
        "startup: entry scripts count={} in {:.1} ms (the whole startup sequence took {:.1} ms)",
        snapshot.startup_scripts,
        snapshot.startup_script_time.as_secs_f64() * 1000.0,
        startup_time.as_secs_f64() * 1000.0,
    );
}

/// The `.tpm` plugin files the reference auto-loads, in ASCII filename order.
///
/// The reference `TVPSearchPluginsAt` keeps regular files whose name ends in
/// `.tpm` (case-insensitive) and `tvpLoadPlugins` scans the exe directory,
/// `<exe>/system` and `<exe>/plugin`, sorts the combined list by file name and
/// loads each (`reference/cpp/core/plugin/PluginImpl.cpp:76-88`, `:148-177`,
/// called from `reference/cpp/core/environ/Application.cpp:342`). The scan is
/// the composition root's job: compat owns no file-system access. A missing or
/// non-directory entry contributes nothing; any other listing error
/// propagates.
fn discover_tpm_plugins(fs: &FileSystem, base_dir: &Path) -> Result<Vec<String>, GameError> {
    let mut found = Vec::new();
    for dir in [
        base_dir.to_path_buf(),
        base_dir.join("system"),
        base_dir.join("plugin"),
    ] {
        let entries = match list(fs, &FsPath::host(dir)) {
            Ok(entries) => entries,
            Err(FsError::NotFound { .. }) | Err(FsError::NotADirectory { .. }) => continue,
            Err(error) => return Err(GameError::Storage(StorageReadError::Fs(error))),
        };
        for entry in entries {
            if !entry.is_dir && has_tpm_extension(&entry.name) {
                found.push(entry.name);
            }
        }
    }
    // ASCII filename order, the reference's sort key (`PluginImpl.cpp:168`).
    found.sort();
    Ok(found)
}

/// Whether `name` ends in `.tpm`, case-insensitively.
///
/// The reference matches the suffix without regard to case
/// (`reference/cpp/core/plugin/PluginImpl.cpp:76-88`).
fn has_tpm_extension(name: &str) -> bool {
    name.len() >= 4 && name.as_bytes()[name.len() - 4..].eq_ignore_ascii_case(b".tpm")
}

/// Runs one startup script through `exec_storage` and records its cost.
fn run_startup_script(
    vm: &VmHost,
    storage: &Storage,
    name: &str,
    stats: &StartupStats,
) -> Result<(), String> {
    let started = Instant::now();
    let result = compat::exec_storage(vm.engine(), storage, name);
    stats.record_startup_script(started.elapsed());
    result
}

/// Runs the resolved startup sequence (reference `TVPExecuteStartupScript`).
///
/// `patch.tjs` and `AfterStartup.tjs` are app-path only and non-fatal (their
/// errors are reported but do not stop startup); `startup.tjs` is searched and
/// falls back to `system/Initialize.tjs` when it is absent or fails.
fn run_startup(
    vm: &VmHost,
    storage: &Storage,
    plan: &StartupPlan,
    stats: &StartupStats,
) -> Result<(), GameError> {
    if let Some(name) = &plan.patch
        && let Err(error) = run_startup_script(vm, storage, name, stats)
    {
        log::warn!("startup: {name}: {error}");
    }

    match &plan.startup {
        Some(startup) => {
            if let Err(error) = run_startup_script(vm, storage, startup, stats) {
                match &plan.fallback {
                    Some(fallback) => {
                        log::warn!("startup: {startup} failed ({error}); running {fallback}");
                        run_startup_script(vm, storage, fallback, stats).map_err(|error| {
                            GameError::Startup {
                                script: fallback.clone(),
                                error,
                            }
                        })?;
                    }
                    None => {
                        return Err(GameError::Startup {
                            script: startup.clone(),
                            error,
                        });
                    }
                }
            }
        }
        None => match &plan.fallback {
            Some(fallback) => {
                run_startup_script(vm, storage, fallback, stats).map_err(|error| {
                    GameError::Startup {
                        script: fallback.clone(),
                        error,
                    }
                })?;
            }
            None => return Err(GameError::NoStartup),
        },
    }

    if let Some(name) = &plan.after
        && let Err(error) = run_startup_script(vm, storage, name, stats)
    {
        log::warn!("startup: {name}: {error}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes the tests that bind the process-global VM.
    fn vm_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    /// A unique temporary directory, removed by the caller.
    fn unique_temp_dir(tag: &str) -> PathBuf {
        let mut base = std::env::temp_dir();
        base.push(format!(
            "krkr-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        base
    }

    /// Builds a valid attribution for `frame` whose body spent `body_ms` in
    /// the `project_and_render` phase.
    fn attribution_for(frame: u64, body_ms: f64, host_render_ms: f64) -> FrameAttribution {
        let mut attribution = FrameAttribution::default();
        attribution.record(
            FramePhases {
                project_and_render_ms: body_ms,
                body_ms,
                ..FramePhases::default()
            },
            StepReport {
                frame_index: frame,
                render_ms: host_render_ms,
                ..StepReport::default()
            },
            Some(render::FrameTimings {
                acquire_ms: 1.5,
                present_ms: 2.5,
                recreate_ms: 0.0,
                fence_wait_ms: 0.5,
            }),
            3.0,
            body_ms + host_render_ms + 3.0,
        );
        attribution
    }

    /// A fast frame emits nothing, even from a frame that was itself slow.
    #[test]
    fn fast_gap_emits_nothing() {
        let attribution = attribution_for(7, 480.0, 10.0);
        assert!(
            attribution
                .slow_frame_line(16.0, FramePacing::default())
                .is_none()
        );
    }

    /// The first frame has no predecessor, so a large gap for it emits
    /// nothing rather than attributing a frame that never ran.
    #[test]
    fn first_frame_emits_nothing() {
        let attribution = FrameAttribution::default();
        assert!(
            attribution
                .slow_frame_line(900.0, FramePacing::default())
                .is_none()
        );
    }

    /// The slow line names the **previous** frame and accounts the gap as
    /// `previous total + platform wait + unaccounted`.
    #[test]
    fn slow_line_names_the_previous_frame_and_balances_the_gap() {
        let attribution = attribution_for(7, 480.0, 10.0);
        let timings = attribution.timings.expect("timings recorded");
        let line = attribution
            .slow_frame_line(
                520.0,
                FramePacing {
                    poll_ms: 15.0,
                    input_ms: 4.0,
                },
            )
            .expect("slow gap emits a line");

        assert!(line.contains("frame 7 slow"), "line was: {line}");
        assert!(
            line.contains("project_and_render=480.0"),
            "line was: {line}"
        );
        assert!(line.contains("body=480.0"), "line was: {line}");
        assert!(line.contains("host_render=10.0"), "line was: {line}");
        assert!(line.contains("acquire=1.5"), "line was: {line}");
        assert!(line.contains("present=2.5"), "line was: {line}");
        assert!(line.contains("fence_wait=0.5"), "line was: {line}");
        assert!(line.contains("platform_wait=15.0"), "line was: {line}");
        assert!(line.contains("input=4.0"), "line was: {line}");
        // total was 480 + 10 + 3 = 493, so 520 - 493 - 15 = 12 unaccounted.
        assert!(line.contains("unaccounted=12.0"), "line was: {line}");
        assert!(line.contains("total=493.0"), "line was: {line}");
        assert_eq!(timings.present_ms, 2.5);
    }

    /// After the next frame is recorded, the following slow gap names that
    /// new frame, not the old one: the attribution tracks the last frame.
    #[test]
    fn slow_line_follows_the_most_recent_frame() {
        let mut attribution = attribution_for(7, 480.0, 10.0);
        let first = attribution
            .slow_frame_line(520.0, FramePacing::default())
            .expect("first slow line");
        assert!(first.contains("frame 7 slow"), "line was: {first}");

        attribution.record(
            FramePhases {
                timer_ms: 250.0,
                body_ms: 250.0,
                ..FramePhases::default()
            },
            StepReport {
                frame_index: 8,
                ..StepReport::default()
            },
            None,
            0.0,
            300.0,
        );
        let second = attribution
            .slow_frame_line(320.0, FramePacing::default())
            .expect("second slow line");
        assert!(second.contains("frame 8 slow"), "line was: {second}");
        assert!(second.contains("timer=250.0"), "line was: {second}");
        assert!(!second.contains("frame 7"), "line was: {second}");
    }

    /// The reference auto-load scan: regular `*.tpm` files only, from the exe
    /// directory, `<exe>/system` and `<exe>/plugin`, sorted by file name
    /// (`reference/cpp/core/plugin/PluginImpl.cpp:76-88`, `:163-168`).
    #[test]
    fn tpm_scan_keeps_regular_tpm_files_from_three_dirs_in_name_order() {
        let base = unique_temp_dir("tpm-scan");
        std::fs::create_dir_all(base.join("system")).expect("system dir");
        std::fs::create_dir_all(base.join("plugin")).expect("plugin dir");
        // A directory whose name ends in `.tpm` is not a plugin file.
        std::fs::create_dir_all(base.join("subdir.tpm")).expect("a .tpm directory");
        for (dir, name) in [
            (&base, "b.tpm"),
            (&base, "a.tpm"),
            (&base, "notes.txt"),
            (&base, "c.TPM"),
            (&base.join("system"), "d.tpm"),
            (&base.join("plugin"), "a.tpm"),
        ] {
            std::fs::write(dir.join(name), b"x").expect("write fixture");
        }

        let fs = FileSystem::host(base.clone());
        let found = discover_tpm_plugins(&fs, &base).expect("scan");
        assert_eq!(
            found,
            vec![
                "a.tpm".to_string(),
                "a.tpm".to_string(),
                "b.tpm".to_string(),
                "c.TPM".to_string(),
                "d.tpm".to_string(),
            ],
            "only regular .tpm files from all three dirs, in ASCII name order"
        );

        // A missing directory contributes nothing (the reference tolerates it).
        let missing = base.join("does-not-exist");
        let empty = discover_tpm_plugins(&FileSystem::host(missing.clone()), &missing)
            .expect("missing dirs are tolerated");
        assert!(
            empty.is_empty(),
            "missing dirs contribute nothing: {empty:?}"
        );

        std::fs::remove_dir_all(&base).expect("cleanup");
    }

    /// Production attaches the storage before the compat surface is installed
    /// (`VmHost::with_default_services`), so the `xp3filter` plugin installs
    /// its L2 seam during registration — before the deferred hook in
    /// `activate_deferred_plugins` ever runs. Without the ordering fix the
    /// seam is still `None` here.
    #[test]
    #[allow(unsafe_code)] // the service attach takes the storage pointer
    fn vm_attaches_services_before_install_so_xp3_filter_registers() {
        let _lock = vm_lock();
        let base = unique_temp_dir("xp3-registration");
        std::fs::create_dir_all(&base).expect("game dir");
        std::fs::write(base.join("xp3filter.tjs"), "// no-op filter\n").expect("filter script");
        let mut storage = Box::new(
            Storage::mount_archives(
                FileSystem::host(base.clone()),
                base.clone(),
                std::iter::empty::<&str>(),
            )
            .expect("mount storage"),
        );
        let vm = VmHost::with_default_services(|services| unsafe {
            services.attach_storage(storage.as_mut() as *mut Storage);
        })
        .expect("create vm host");
        assert!(
            storage.xp3_extraction_filter.is_some(),
            "the storage attached before install must be visible at plugin \
             registration, which installs the xp3 filter seam"
        );
        drop(vm);
        drop(storage);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The production entry point leaves the seam installed after
    /// `GameHost::open`, proving the composition root reaches the plugin's
    /// registration with the storage attached.
    #[test]
    fn game_host_open_installs_the_xp3_filter_seam() {
        let _lock = vm_lock();
        let base = unique_temp_dir("xp3-gamehost");
        std::fs::create_dir_all(&base).expect("game dir");
        std::fs::write(base.join("xp3filter.tjs"), "// no-op filter\n").expect("filter script");
        std::fs::write(base.join("startup.tjs"), "// no-op startup\n").expect("startup script");
        let host = crate::Host::new(crate::HostConfig::headless()).expect("headless host");
        let game = GameHost::open(host, Vec::new(), base.clone()).expect("open game");
        assert!(
            game.storage().xp3_extraction_filter.is_some(),
            "GameHost::open must leave the xp3 filter seam installed"
        );
        drop(game);
        let _ = std::fs::remove_dir_all(&base);
    }
}
