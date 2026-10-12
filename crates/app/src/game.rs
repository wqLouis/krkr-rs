//! Real-game frontend: mount an explicit archive list, run the L2 startup
//! sequence through the L4 VM host, and drive the VM frame body together with
//! the L2 render host.
//!
//! Loading is explicit: the caller
//! supplies the `--archive` list and base directory; nothing scans a folder
//! for `*.xp3` or `startup.tjs`. [`GameHost`] is the composition-root
//! counterpart of the empty-scene [`Host`]: it adds the VM host, the mounted
//! game [`Storage`] and the ordered frame body.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use api::fs::FsPath;
use compat::{CompatCtx, CompatError, FrameBody};
use engine::Storage;
use engine::storage::{MountOptions, StartupPlan};
use fs::FileSystem;
use platform::{PlatformError, PlatformEvent};
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
}

/// Per-frame wall-clock costs, in milliseconds, recorded by the frame body.
///
/// The fields are overwritten by each poll's completion hook, so one instance
/// describes exactly one frame. [`FramePhases::body_ms`] is the sum of the six
/// poll phases; the render host's own costs are not part of it.
#[derive(Default)]
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
        let mut vm = VmHost::with_default()?;
        // SAFETY: `storage` and `host` are declared after `vm` in the returned
        // struct and therefore outlive it; the boxed `host` keeps the scene
        // address stable, and nothing else aliases either value while the
        // binding is installed.
        unsafe {
            vm.compat_mut()
                .services_mut()
                .attach_scene(host.scene_mut() as *mut engine::Scene);
            vm.compat_mut()
                .services_mut()
                .attach_storage(storage.as_ref() as *const Storage as *mut Storage);
        }

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

        let plan = storage.startup_plan()?;
        let startup_started = Instant::now();
        run_startup(&vm, &storage, &plan, &stats)?;
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
        let dt = f64::from(dt_ms.max(0.0));
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

        // The gap is the platform's wall-clock frame period, the same value
        // the audio drain caps against; a gap this long also produces the
        // audio warning, so the attribution line explains it.
        if dt >= SLOW_FRAME_MS {
            self.log_slow_frame(dt, audio_ms, &report, total_ms);
        }
        if !self.first_frame_logged {
            self.first_frame_logged = true;
            log::info!(
                "startup: first frame {:.1} ms after open",
                self.open_start.elapsed().as_secs_f64() * 1000.0
            );
        }
        Ok(report)
    }

    /// Emits the one warning line a slow frame gets, naming every measured
    /// phase on it.
    fn log_slow_frame(&self, gap_ms: f64, audio_ms: f64, report: &StepReport, total_ms: f64) {
        let phases = &self.polls.phases;
        log::warn!(
            "frame {} slow: gap={gap_ms:.1} ms since the previous frame; \
             timer={:.1} sound={:.1} video={:.1} async={:.1} continuous={:.1} \
             project_and_render={:.1} body={:.1}; audio={audio_ms:.1} \
             host_sync={:.1} host_project={:.1} host_render={:.1} total={total_ms:.1} ms",
            report.frame_index,
            phases.timer_ms,
            phases.sound_ms,
            phases.video_ms,
            phases.async_trigger_ms,
            phases.continuous_handler_ms,
            phases.project_and_render_ms,
            phases.body_ms,
            report.sync_ms,
            report.project_ms,
            report.render_ms,
        );
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
