//! Real-game frontend: mount an explicit archive list, run the L2 startup
//! sequence through the L4 VM host, and drive the VM frame body together with
//! the L2 engine frame.
//!
//! Loading starts from the base directory plus either an explicit `--archive`
//! list or, when that list is empty, every `*.xp3` found under the base
//! directory. The project path is chosen from that mount input; no folder is
//! scanned for `startup.tjs`. [`GameHost`] is the composition-root
//! counterpart of the empty-scene [`Host`]: it adds the VM host, the mounted
//! game [`Storage`] and the ordered frame body, which it runs inside the
//! engine's frame hook.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use api::error::FsError;
use api::fs::FsPath;
use api::input::InputEvent;
use compat::{CompatCtx, CompatError, FrameBody};
use engine::imagedecode::DecodeStats;
use engine::input::InputState;
use engine::storage::{MountOptions, StartupPlan, StorageReadError};
use engine::timer::TimerState;
use engine::{FrameTimeSource, GameFrame, PixelTranslation, Storage};
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
    /// The image decode pool could not be started.
    #[error(transparent)]
    DecodePool(#[from] engine::imagedecode::DecodePoolError),
    /// The storage read pool could not be started.
    #[error(transparent)]
    StoragePool(#[from] engine::storage::StoragePoolError),
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
/// describes exactly one frame. [`FramePhases::body_ms`] is the sum of the five
/// poll phases; the engine's own costs are not part of it.
#[derive(Clone, Copy, Default)]
struct FramePhases {
    sound_ms: f64,
    video_ms: f64,
    async_trigger_ms: f64,
    continuous_handler_ms: f64,
    frame_tail_ms: f64,
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

/// Per-frame wall-clock costs the VM tick records outside the ordered polls.
#[derive(Clone, Copy, Default)]
struct TickPhases {
    /// Event adapter's per-frame setup.
    input_begin_ms: f64,
    /// Adapting and dispatching the frame's platform events to script.
    input_events_ms: f64,
    /// Event adapter's per-frame commit.
    input_end_ms: f64,
    /// The whole `VmHost::tick` call, including the compat context lookup.
    tick_ms: f64,
}

impl TickPhases {
    /// The input segments' combined cost.
    fn input_ms(&self) -> f64 {
        self.input_begin_ms + self.input_events_ms + self.input_end_ms
    }
}

/// The ordered frame-body polls the game host runs each frame (doc 08 §6).
#[derive(Default)]
struct Polls {
    /// The most playback one frame may advance, in milliseconds: the audio
    /// pump's playback cap, forwarded to the movie steps.
    max_step_ms: f64,
    /// Wall-clock cost of each poll in the current frame.
    phases: FramePhases,
    /// Wall-clock cost of the VM tick's non-poll segments.
    tick: TickPhases,
}

impl FrameBody for Polls {
    fn sound_poll(&mut self, ctx: &mut CompatCtx) -> Result<(), CompatError> {
        compat::sound_poll(ctx)
    }

    fn video_poll(&mut self, ctx: &mut CompatCtx) -> Result<(), CompatError> {
        compat::video_advance(ctx, self.max_step_ms)
    }

    fn async_trigger_poll(&mut self, ctx: &mut CompatCtx) -> Result<(), CompatError> {
        compat::async_trigger_poll(ctx)
    }

    fn continuous_handler_poll(&mut self, ctx: &mut CompatCtx) -> Result<(), CompatError> {
        compat::continuous_handler_poll(ctx).map(|_remaining| ())
    }

    #[allow(unsafe_code)]
    fn frame_tail(&mut self, ctx: &mut CompatCtx) -> Result<(), CompatError> {
        // The movie frame body advances its overlays and copies the current
        // frame into the layer before the transition step composites it
        // (reference `layerExMovie::OnContinuousCallback`).
        compat::movie_poll(ctx, self.max_step_ms)?;
        // The frame body owns the per-frame transition step; the remaining
        // hand-offs are host drains the engine frame consumes after this poll
        // returns.
        compat::advance_transitions(ctx).map_err(CompatError::Engine)?;
        // The script-cached layers composite after the transition step, so a
        // cache that contains a transition takes the frame just blended, and
        // before the engine frame uploads and projects.
        compat::refresh_cached_layers(ctx).map_err(CompatError::Engine)?;
        // SAFETY: `ctx` is bound to a live engine for the callback.
        let services = unsafe { ctx.services() };
        let _ = compat::take_bring_to_front_requests(services);
        let _ = compat::take_update_requests(services);
        let _ = compat::take_capture_releases(ctx);
        Ok(())
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

    fn frame_tail_completed(&mut self, elapsed: Duration) {
        FramePhases::record(
            &mut self.phases.frame_tail_ms,
            &mut self.phases.body_ms,
            elapsed,
        );
    }

    fn input_begin_completed(&mut self, elapsed: Duration) {
        self.tick.input_begin_ms = elapsed.as_secs_f64() * 1000.0;
    }

    fn input_events_completed(&mut self, elapsed: Duration) {
        self.tick.input_events_ms = elapsed.as_secs_f64() * 1000.0;
    }

    fn input_end_completed(&mut self, elapsed: Duration) {
        self.tick.input_end_ms = elapsed.as_secs_f64() * 1000.0;
    }
}

/// The per-frame costs [`FrameAttribution::record`] stores alongside the
/// phases: audio drain, engine frame and total wall clock.
#[derive(Clone, Copy, Default)]
struct FrameCosts {
    /// Audio drain cost.
    audio_ms: f64,
    /// Engine frame cost, excluding the frame hook.
    host_ms: f64,
    /// Whole wall-clock cost.
    total_ms: f64,
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
    /// The previous frame's VM-tick costs outside the polls.
    tick: TickPhases,
    /// The previous frame's host step report.
    report: StepReport,
    /// The previous frame's GPU call costs, when the backend reports them.
    timings: Option<render::FrameTimings>,
    /// The previous frame's audio drain cost.
    audio_ms: f64,
    /// The previous frame's engine frame cost, excluding the frame hook.
    host_ms: f64,
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
        tick: TickPhases,
        report: StepReport,
        timings: Option<render::FrameTimings>,
        costs: FrameCosts,
    ) {
        self.phases = phases;
        self.tick = tick;
        self.report = report;
        self.timings = timings;
        self.audio_ms = costs.audio_ms;
        self.host_ms = costs.host_ms;
        self.total_ms = costs.total_ms;
        self.valid = true;
    }

    /// Formats the single slow-frame line for `gap_ms`, or `None` when the
    /// gap is below the threshold or no previous frame exists yet.
    ///
    /// The named phases and host/GPU costs are the previous frame's, because
    /// the gap spans it, and `presented` says whether that frame reached the
    /// surface: an acquisition timeout appears as `presented=false` with the
    /// wait in `acquire`. `platform_wait` and `input` are this step's
    /// measurement; `unaccounted` is `gap - previous total - platform wait`,
    /// so a non-zero value is loop overhead or a stall outside both the
    /// frame and the event poll. `tick_residual` is the VM tick not covered
    /// by the input segments and the five polls; `host_residual` is the
    /// engine frame not covered by sync/project/render.
    fn slow_frame_line(&self, gap_ms: f64, pacing: FramePacing) -> Option<String> {
        if !self.valid || gap_ms < SLOW_FRAME_MS {
            return None;
        }
        let phases = &self.phases;
        let tick = &self.tick;
        let report = &self.report;
        let timings = self.timings.unwrap_or_default();
        let unaccounted = gap_ms - self.total_ms - pacing.poll_ms;
        let tick_residual = tick.tick_ms - tick.input_ms() - phases.body_ms;
        let host_residual = self.host_ms - report.sync_ms - report.project_ms - report.render_ms;
        Some(format!(
            "frame {} slow: presented={}, gap={gap_ms:.1} ms since the previous frame; \
             the gap spans the previous frame, so these are its phases: \
             sound={:.1} video={:.1} async={:.1} continuous={:.1} \
             frame_tail={:.1} body={:.1}; its vm: tick={:.1} \
             input_begin={:.1} input_events={:.1} input_end={:.1} \
             tick_residual={tick_residual:.1}; its host: host_total={:.1} \
             audio={:.1} host_sync={:.1} sync_upload={:.1} sync_prune={:.1} \
             host_project={:.1} host_render={:.1} submit={:.1} \
             host_residual={host_residual:.1} total={:.1}; its gpu: acquire={:.1} \
             present={:.1} recreate={:.1} fence_wait={:.1}; this wait: \
             platform_wait={:.1} input={:.1} unaccounted={unaccounted:.1} ms",
            report.frame_index,
            report.presented,
            phases.sound_ms,
            phases.video_ms,
            phases.async_trigger_ms,
            phases.continuous_handler_ms,
            phases.frame_tail_ms,
            phases.body_ms,
            tick.tick_ms,
            tick.input_begin_ms,
            tick.input_events_ms,
            tick.input_end_ms,
            self.host_ms,
            self.audio_ms,
            report.sync_ms,
            report.upload_ms,
            report.prune_ms,
            report.project_ms,
            report.render_ms,
            timings.submit_ms,
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

/// A running game: the L4 VM host bound to the engine's scene and timer, and
/// the mounted game storage.
///
/// Field order is load-bearing. `audio` is declared first so the L1 device is
/// stopped (its state drop stops the stream) **before** the VM dies; the VM
/// then holds raw pointers to the [`Storage`] and to the
/// [`Scene`](engine::Scene), [`TimerState`] and [`InputState`] behind it, so
/// `vm` is declared before its targets, and the [`Host`] is boxed so its
/// address (and thus the engine's) is stable across the move into this
/// struct.
pub struct GameHost {
    /// The L1 audio device drained to the L3 mixer once per frame; dropped
    /// first, so the stream stops before the engine tears down.
    audio: AudioOutput,
    /// The L4 VM host, bound to `storage` and the engine services.
    vm: VmHost,
    /// The mounted game archives, read by the `Storages` natives.
    storage: Box<Storage>,
    /// The L2 engine host, boxed for a stable scene/timer/input address.
    host: Box<Host>,
    /// The ordered frame body.
    polls: Polls,
    /// Wall-clock instant the open sequence started.
    open_start: Instant,
    /// The previous frame's measured attribution, reported when the next gap
    /// is slow.
    last_frame: FrameAttribution,
    /// Whether the first frame's timing has been logged.
    first_frame_logged: bool,
}

/// The engine's frame hook for a running game: the VM tick, the audio drain it
/// feeds, and the ordered polls.
///
/// [`Engine::step`](engine::Engine::step) calls it after firing the due timers
/// and before projecting and drawing, so the scene the VM left behind is the
/// one the frame presents.
struct VmHook<'a> {
    /// The VM host whose frame body runs.
    vm: &'a mut VmHost,
    /// The audio device drained from the mixer after the VM tick.
    audio: &'a mut AudioOutput,
    /// Platform events delivered to script this frame.
    events: &'a [PlatformEvent],
    /// The presentation the frame started with, used to map input positions
    /// from device pixels into VM pixels.
    translation: Option<PixelTranslation>,
    /// The ordered frame body and its per-frame cost record.
    polls: &'a mut Polls,
    /// Wall-clock cost of the whole hook, in milliseconds.
    hook_ms: f64,
    /// Wall-clock cost of the audio drain, in milliseconds.
    audio_ms: f64,
}

impl GameFrame for VmHook<'_> {
    fn before_render(&mut self, now_ms: u64, dt_ms: f64) -> Result<(), String> {
        let hook_started = Instant::now();
        // A movie's picture advances by the same bound the audio pump puts on
        // one frame's playback, so a long frame leaves the picture and the
        // sound equally far behind.
        self.polls.max_step_ms = self.audio.max_chunk_ms();
        // Pointer and touch positions arrive in device pixels while the scene's
        // hit-testing is in VM pixels, so those events are mapped back before
        // the VM sees them. Without a mapping, or at an identity mapping, the
        // events pass through untouched.
        let translation = self
            .translation
            .filter(|translation| !translation.is_identity());
        let source = self.events;
        let translated = translation.map(|translation| {
            source
                .iter()
                .map(|event| translate_event(Some(translation), event))
                .collect::<Vec<PlatformEvent>>()
        });
        let events: &[PlatformEvent] = translated.as_deref().unwrap_or(source);
        let frame = FrameCtx {
            now_ms,
            dt_ms: dt_ms as u32,
            body: &mut *self.polls,
            events,
        };
        let vm_started = Instant::now();
        self.vm.tick(frame).map_err(|error| error.to_string())?;
        self.polls.tick.tick_ms = vm_started.elapsed().as_secs_f64() * 1000.0;
        let audio_started = Instant::now();
        self.audio
            .pump(self.vm, dt_ms)
            .map_err(|error| error.to_string())?;
        self.audio_ms = audio_started.elapsed().as_secs_f64() * 1000.0;
        self.hook_ms = hook_started.elapsed().as_secs_f64() * 1000.0;
        Ok(())
    }

    fn after_project(&mut self, drawn_layers: &[u32]) -> Result<(), String> {
        let ctx = self.vm.compat().ctx().map_err(|error| error.to_string())?;
        compat::advance_self_updating_transitions(&ctx, drawn_layers)?;
        compat::refresh_cached_layers(&ctx)?;
        Ok(())
    }
}

/// Maps the position-carrying events of one frame from device pixels into VM
/// pixels.
///
/// Only the pointer position and a touch contact carry a coordinate; button,
/// wheel, key, text, focus and resize events mean the same in both spaces and
/// are returned unchanged, as is every event without a mapping.
fn translate_event(translation: Option<PixelTranslation>, event: &PlatformEvent) -> PlatformEvent {
    let Some(translation) = translation else {
        return event.clone();
    };
    match event {
        PlatformEvent::Input(InputEvent::PointerMoved { x, y }) => {
            let (x, y) = translation.to_vm_point((*x, *y));
            PlatformEvent::Input(InputEvent::PointerMoved { x, y })
        }
        PlatformEvent::Input(InputEvent::Touch { id, phase, x, y }) => {
            let (x, y) = translation.to_vm_point((*x, *y));
            PlatformEvent::Input(InputEvent::Touch {
                id: *id,
                phase: *phase,
                x,
                y,
            })
        }
        other => other.clone(),
    }
}

impl GameHost {
    /// Mounts `archives` in order (later archives win), binds the VM to the
    /// engine's scene and timer, to the L2 input state and to the storage,
    /// installs the L1 codec seams, and runs the L2 startup sequence.
    ///
    /// `base_dir` is the app path (loose files, `patch.tjs`,
    /// `AfterStartup.tjs`). Every mounted archive keeps its file open and is
    /// read positionally per entry; use [`Self::open_with_preload`] to hold
    /// the archives in memory instead.
    ///
    /// The project path is chosen from the mount input: for a directory launch
    /// the conventional `data.xp3` when it mounted, else the archive holding
    /// the boot scripts, else the base directory itself; for an explicit list
    /// the archive holding the boot scripts, else the first named archive. A
    /// relative `Storages.addAutoPath("system/")` therefore scopes to that
    /// archive rather than to the folder.
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

    /// Like [`Self::open`], but reads every mounted archive into memory in the
    /// background when `preload_archives` is set.
    ///
    /// The resident-memory cost is the sum of the mounted archives' sizes and
    /// there is no cap; the mount order and precedence are unchanged, and the
    /// mount does not wait for the archive's bytes. See
    /// [`engine::storage::MountOptions`].
    ///
    /// # Errors
    ///
    /// As [`Self::open`], plus a typed error when a preloaded archive is
    /// larger than this platform can address in one buffer.
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
        let mut storage = Box::new(if archives.is_empty() {
            let discovered = engine::storage::discover_archives(&fs, &base_dir)?;
            let mut storage = Storage::mount_discovered_archives_with(
                fs,
                &base_dir,
                &discovered,
                MountOptions { preload_archives },
            )?;
            // A directory launch names no archive, so the project path is
            // reconstructed from what the folder holds (`directory_project`).
            // Only an archive that mounted can be the project: the project
            // archive is opened on demand for a relative lookup, so one that
            // failed to mount cannot serve a file.
            let project = directory_project(
                &base_dir,
                &discovered,
                |path| boot_script_entries(&storage, path),
                |path| {
                    let mounted = resolved_archive_path(&storage, path);
                    storage
                        .archive_paths()
                        .any(|archive| archive == mounted.as_path())
                },
            );
            storage.set_project(&project);
            storage
        } else {
            let mut storage = Storage::mount_archives_with(
                fs,
                &base_dir,
                &archives,
                MountOptions { preload_archives },
            )?;
            // The list is the launcher's own choice of archives; the project
            // is the data archive — the one that holds the boot scripts — else
            // the first named.
            let project = explicit_project(&base_dir, &archives, |path| {
                boot_script_entries(&storage, path)
            });
            storage.set_project(&project);
            storage
        });
        let mount_time = mount_started.elapsed();
        // The storage read pool: the worker pool the entry reads run on. The
        // engine drains it every frame and the storage schedules the reads
        // on it, so the raw read and the archive decompression happen off the
        // frame thread.
        let storage_pool = host
            .engine_mut()
            .install_storage_reader(
                std::sync::Arc::new(engine::storage::FileSystemReader),
                engine::storage::StoragePoolConfig::default(),
            )
            .map_err(GameError::StoragePool)?;
        storage.read_pool = Some(storage_pool);
        let scene_ptr = host.scene_mut() as *mut engine::Scene;
        let storage_ptr = storage.as_ref() as *const Storage as *mut Storage;
        let timer_ptr = host.engine_mut().timer_mut() as *mut TimerState;
        let input_ptr = host.engine_mut().input_mut() as *mut InputState;
        // The frame clock: the tick count the script reads is the clock the
        // frame advances, so the game and its timers share one clock.
        let clock_ptr = host.engine_mut().clock_mut() as *mut engine::FrameClock;
        // The image decode pool: the bottom decoder it drives, installed
        // before the binding so the natives can submit decodes. The pool
        // must not be replaced while the binding is installed: the natives
        // hold the address the host attaches here.
        let media = media::Media::new();
        crate::install_image_decoder(host.engine_mut(), &media).map_err(GameError::DecodePool)?;
        let pool_ptr = host
            .engine_mut()
            .decode_pool_mut()
            .map_or(std::ptr::null_mut(), |pool| pool as *mut engine::DecodePool);
        // SAFETY: `storage` and `host` are declared after `vm` in the returned
        // struct and therefore outlive it; the boxed `host` keeps the engine's
        // scene, timer, input and clock addresses stable, and nothing else
        // aliases any of them while the binding is installed. Attaching before
        // the compat surface is installed lets a registration-time service
        // read (the `xp3filter` `xp3filter.tjs` read) see the mounted
        // archives.
        let vm = VmHost::with_default_services(|services| unsafe {
            services.attach_scene(scene_ptr);
            services.attach_storage(storage_ptr);
            services.attach_timer(timer_ptr);
            services.attach_input(input_ptr);
            services.attach_clock(clock_ptr);
            services.attach_decode_pool(pool_ptr);
        })?;

        // Install the seams the compat natives compose: the L1 codecs and the
        // emulated Windows host facts. The codecs clone their backend, so
        // the `media` local may drop here.
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
        // The reference clipboard is inert
        // (`reference/cpp/core/utils/impl/ClipboardImpl.cpp:5-19`):
        // `hasFormat` is false, `asText` reads void, and a write is a no-op.
        // Installing the emulated backend keeps every `Clipboard` member from
        // raising the missing-backend error in production.
        vm.compat()
            .set_clipboard_backend((|_format| false, || None, |_text| {}));

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
        log_startup(
            &storage,
            &stats,
            host.engine().decode_stats(),
            mount_time,
            startup_time,
        );
        Ok(Self {
            audio,
            vm,
            storage,
            host,
            polls: Polls::default(),
            open_start,
            last_frame: FrameAttribution::default(),
            first_frame_logged: false,
        })
    }

    /// The L2 engine host.
    pub fn host(&self) -> &Host {
        &self.host
    }

    /// The L2 engine host, mutably (the platform loop resizes it).
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

    /// The L4 VM host bound to the scene and storage.
    pub fn vm(&self) -> &VmHost {
        &self.vm
    }

    /// Runs one frame: the engine frame with the VM frame body (events, polls,
    /// transitions) as its upward hook, then the projection and the draw.
    ///
    /// The engine fires the due timers before the hook and projects and draws
    /// after it, so the scene the VM left behind is the frame that presents.
    /// The L1 audio drain runs inside the hook, right after `tick`: a
    /// dt-derived chunk of the mixer is rendered through
    /// `compat::sound_render` (which advances the mixer by the frames it
    /// produces) and written to the device (policy documented in the `audio`
    /// module).
    ///
    /// # Errors
    ///
    /// Returns [`GameError`] when the VM frame body fails, when the audio
    /// pump cannot reach the bound compat context or the device rejects a
    /// write, or when a render action fails.
    ///
    /// Returns the engine [`StepReport`] so a headless run can print the draw
    /// counters (they exist on the null backend only, see [`StepReport::stats`]);
    /// the windowed loop ignores them.
    pub fn step(
        &mut self,
        clock: &dyn FrameTimeSource,
        events: &[PlatformEvent],
    ) -> Result<StepReport, GameError> {
        self.step_with_pacing(clock, events, FramePacing::default())
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
        clock: &dyn FrameTimeSource,
        events: &[PlatformEvent],
        pacing: FramePacing,
    ) -> Result<StepReport, GameError> {
        let dt = clock.elapsed_ms().max(0.0);
        // The gap spans the previous frame, so attribute it before this step
        // overwrites the stored frame.
        if let Some(line) = self.last_frame.slow_frame_line(dt, pacing) {
            log::warn!("{line}");
        }
        self.polls.phases = FramePhases::default();
        self.polls.tick = TickPhases::default();
        // Input is mapped with the presentation captured as the frame starts;
        // the engine refits after the hook, so a window size the VM changes
        // during the hook applies to this frame's drawing and to the next
        // frame's input.
        let translation = self.host.engine().translation();

        let step_started = Instant::now();
        let (report, hook_ms, audio_ms) = {
            let mut hook = VmHook {
                vm: &mut self.vm,
                audio: &mut self.audio,
                events,
                translation,
                polls: &mut self.polls,
                hook_ms: 0.0,
                audio_ms: 0.0,
            };
            let report = self.host.step_with_frame(clock, &mut hook)?;
            (report, hook.hook_ms, hook.audio_ms)
        };
        let total_ms = step_started.elapsed().as_secs_f64() * 1000.0;
        // The engine frame's own cost: the whole step minus the upward hook it
        // ran (the VM tick and the audio drain are attributed separately).
        let host_ms = total_ms - hook_ms;

        self.last_frame.record(
            self.polls.phases,
            self.polls.tick,
            report,
            self.host.last_frame_timings(),
            FrameCosts {
                audio_ms,
                host_ms,
                total_ms,
            },
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
    deferred: Option<DecodeStats>,
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
    let mut snapshot: StartupSnapshot = stats.snapshot();
    if let Some(deferred) = deferred {
        snapshot.images_decoded = snapshot.images_decoded.saturating_add(deferred.decodes);
        snapshot.image_decode = snapshot.image_decode.saturating_add(deferred.elapsed);
    }
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

/// The path an archive is mounted under.
///
/// A relative path is resolved against the app directory, the same way the
/// mount resolves it; an absolute path is used as given.
fn resolved_archive_path(storage: &Storage, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        storage.game_dir().join(path)
    }
}

/// The number of entries in the archive mounted at `path` when it holds
/// the boot scripts (`startup.tjs` or `system/Initialize.tjs`), `None` when
/// it holds neither.
///
/// The archive entry lookup is case-insensitive, so either spelling finds the
/// same entry. A relative `path` is resolved against the app directory, the
/// same way the mount resolves it; a path that is not one of the mounted
/// archives holds no boot scripts.
fn boot_script_entries(storage: &Storage, path: &Path) -> Option<usize> {
    let path = resolved_archive_path(storage, path);
    let mut entries = None;
    storage.for_each_archive(|mounted, archive| {
        if mounted == path.as_path()
            && (archive.entry("startup.tjs").is_some()
                || archive.entry("system/Initialize.tjs").is_some())
        {
            entries = Some(archive.len());
        }
    });
    entries
}

/// The project path for a directory-only launch.
///
/// The project decides whether a relative auto path resolves inside an archive
/// (`Storages.addAutoPath("system/")` searches the project archive's
/// `system/`) or on disk, so a launch that mounts archives needs one of them as
/// the project. A directory launch names no archive, so the path is
/// reconstructed: the conventional `data.xp3` when it mounted, else the
/// archive that holds the boot scripts, else the folder itself.
///
/// Several archives can hold the boot scripts: a game ships the archive it
/// is played from and a patch archive, and the patch carries a copy of
/// `startup.tjs`. The project is the archive the game is played from, so the
/// data archive is chosen over the patch: the patch holds the changed files
/// and the game registers it itself as an auto path
/// (`Storages.addAutoPath("patch.xp3>")`), so its files are reached through
/// the auto path and not through the project. The data archive is the bigger
/// of the two, so the boot-script archive with the most entries is the
/// project.
///
/// Only a mounted archive can be the project. The project archive is opened on
/// demand for a relative lookup, so an archive that failed to mount cannot
/// serve a file and must not be named; `is_mounted` reports which of the
/// discovered archives are mounted.
fn directory_project(
    base_dir: &Path,
    discovered: &[PathBuf],
    boot_script_entries: impl Fn(&Path) -> Option<usize>,
    is_mounted: impl Fn(&Path) -> bool,
) -> PathBuf {
    if let Some(data) = discovered.iter().find(|path| {
        path.file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("data.xp3"))
            && is_mounted(path)
    }) {
        return data.clone();
    }
    if let Some((archive, _)) = data_archive(discovered, &boot_script_entries) {
        return archive.clone();
    }
    base_dir.to_path_buf()
}

/// The boot-script archive with the most entries, `None` when no candidate
/// holds the boot scripts.
///
/// The first candidate wins a tie, so the discovery order decides.
fn data_archive<'a>(
    candidates: &'a [PathBuf],
    boot_script_entries: &impl Fn(&Path) -> Option<usize>,
) -> Option<(&'a PathBuf, usize)> {
    let mut best: Option<(&PathBuf, usize)> = None;
    for path in candidates {
        let Some(entries) = boot_script_entries(path) else {
            continue;
        };
        if best.is_none_or(|(_, best_entries)| entries > best_entries) {
            best = Some((path, entries));
        }
    }
    best
}

/// The project path for an explicit `--archive` list: the archive holding the
/// boot scripts, else the first named archive, else the directory.
///
/// Several named archives can hold the boot scripts; the data archive is the
/// project, the same way a directory launch chooses it.
///
/// The mount order and precedence are unchanged: every named archive is still
/// mounted in order and a later mount still overrides an earlier one outside
/// the project archive.
fn explicit_project(
    base_dir: &Path,
    archives: &[PathBuf],
    boot_script_entries: impl Fn(&Path) -> Option<usize>,
) -> PathBuf {
    data_archive(archives, &boot_script_entries)
        .map(|(archive, _)| archive.clone())
        .or_else(|| archives.first().cloned())
        .unwrap_or_else(|| base_dir.to_path_buf())
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
    use engine::InjectedFrameTime;

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
    /// the `frame_tail` phase.
    fn attribution_for(frame: u64, body_ms: f64, host_render_ms: f64) -> FrameAttribution {
        let mut attribution = FrameAttribution::default();
        attribution.record(
            FramePhases {
                frame_tail_ms: body_ms,
                body_ms,
                ..FramePhases::default()
            },
            TickPhases {
                input_begin_ms: 1.0,
                input_events_ms: 2.0,
                input_end_ms: 0.5,
                tick_ms: body_ms + 3.5 + 4.0,
            },
            StepReport {
                frame_index: frame,
                sync_ms: 0.5,
                upload_ms: 0.4,
                prune_ms: 0.1,
                project_ms: 0.2,
                render_ms: host_render_ms,
                ..StepReport::default()
            },
            Some(render::FrameTimings {
                acquire_ms: 1.5,
                present_ms: 2.5,
                submit_ms: 0.7,
                recreate_ms: 0.0,
                fence_wait_ms: 0.5,
            }),
            FrameCosts {
                audio_ms: 3.0,
                host_ms: host_render_ms + 1.0,
                total_ms: body_ms + 3.5 + 4.0 + host_render_ms + 1.0 + 3.0,
            },
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
            line.contains("presented=false"),
            "a slow gap names whether the frame it spans reached the surface: {line}"
        );
        assert!(line.contains("frame_tail=480.0"), "line was: {line}");
        assert!(line.contains("body=480.0"), "line was: {line}");
        assert!(line.contains("tick=487.5"), "line was: {line}");
        assert!(line.contains("input_begin=1.0"), "line was: {line}");
        assert!(line.contains("input_events=2.0"), "line was: {line}");
        assert!(line.contains("input_end=0.5"), "line was: {line}");
        assert!(line.contains("tick_residual=4.0"), "line was: {line}");
        assert!(line.contains("host_total=11.0"), "line was: {line}");
        assert!(line.contains("host_render=10.0"), "line was: {line}");
        assert!(line.contains("host_sync=0.5"), "line was: {line}");
        assert!(line.contains("sync_upload=0.4"), "line was: {line}");
        assert!(line.contains("sync_prune=0.1"), "line was: {line}");
        assert!(line.contains("host_project=0.2"), "line was: {line}");
        assert!(line.contains("submit=0.7"), "line was: {line}");
        assert!(line.contains("host_residual=0.3"), "line was: {line}");
        assert!(line.contains("acquire=1.5"), "line was: {line}");
        assert!(line.contains("present=2.5"), "line was: {line}");
        assert!(line.contains("fence_wait=0.5"), "line was: {line}");
        assert!(line.contains("platform_wait=15.0"), "line was: {line}");
        assert!(line.contains("input=4.0"), "line was: {line}");
        // total was 480 + 7.5 + 10 + 1 + 3 = 501.5, so 520 - 501.5 - 15 = 3.5.
        assert!(line.contains("unaccounted=3.5"), "line was: {line}");
        assert!(line.contains("total=501.5"), "line was: {line}");
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
                frame_tail_ms: 250.0,
                body_ms: 250.0,
                ..FramePhases::default()
            },
            TickPhases {
                tick_ms: 250.0,
                ..TickPhases::default()
            },
            StepReport {
                frame_index: 8,
                ..StepReport::default()
            },
            None,
            FrameCosts {
                audio_ms: 0.0,
                host_ms: 0.0,
                total_ms: 300.0,
            },
        );
        let second = attribution
            .slow_frame_line(320.0, FramePacing::default())
            .expect("second slow line");
        assert!(second.contains("frame 8 slow"), "line was: {second}");
        assert!(second.contains("frame_tail=250.0"), "line was: {second}");
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

    /// One `step` is one engine frame: the timer registry advances with the
    /// frame clock, the game's hook (the VM frame body) delivers what fired,
    /// and the scene is drawn afterwards. The script counter therefore advances
    /// only because `Engine::step` ran the app's hook.
    ///
    /// The reference fires a timer whose deadline the frame tick passed
    /// (`reference/cpp/core/utils/impl/TimerImpl.cpp:110-143`) and runs the
    /// per-frame update hook before `drawScene`
    /// (`reference/cpp/core/environ/RenderUtils.h:13-24`).
    #[test]
    fn step_runs_the_game_hook_inside_the_engine_frame() {
        let _lock = vm_lock();
        let base = unique_temp_dir("frame-hook");
        std::fs::create_dir_all(&base).expect("game dir");
        std::fs::write(
            base.join("startup.tjs"),
            "var fired = 0;\n\
             var ticker = new Timer(function() { fired++; }, '');\n\
             ticker.interval = 1;\n\
             ticker.enabled = 1;\n",
        )
        .expect("startup script");
        let host = crate::Host::new(crate::HostConfig::headless()).expect("headless host");
        let mut game = GameHost::open(host, Vec::new(), base.clone()).expect("open game");

        fn fired(game: &GameHost) -> i64 {
            match game.vm().eval("fired", "t").expect("eval `fired`") {
                tjs::TjsValue::Integer(value) => value,
                other => panic!("expected Integer for `fired`, got {other:?}"),
            }
        }

        assert_eq!(fired(&game), 0, "no frame has run yet");
        let clock = InjectedFrameTime::new(16.0);
        for _ in 0..3 {
            game.step(&clock, &[]).expect("step");
        }
        assert!(
            fired(&game) >= 1,
            "the engine frame must run the hook that delivers the fired timer"
        );
        assert_eq!(game.host().frame_index(), 3);

        drop(game);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The composition root maps pointer and touch positions from device
    /// pixels into VM pixels before the VM sees them, so hit-testing stays in
    /// the scene's own space. Every other event carries no position and passes
    /// through.
    ///
    /// The reference receives input in the window's own pixels
    /// (`reference/cpp/core/visual/LayerManager.cpp:345-356` hit-tests in
    /// primary-layer coordinates). Separating the framebuffer from the VM size
    /// and mapping back into it is the D40 presentation extension, not
    /// reference behaviour.
    #[test]
    fn pointer_and_touch_events_are_mapped_into_vm_pixels() {
        // A 320x240 VM surface in a 640x480 framebuffer: scale 2, no letterbox.
        let translation = PixelTranslation::fit((320, 240), (640, 480)).expect("fittable sizes");

        let moved = PlatformEvent::Input(InputEvent::PointerMoved { x: 200.0, y: 100.0 });
        assert_eq!(
            translate_event(Some(translation), &moved),
            PlatformEvent::Input(InputEvent::PointerMoved { x: 100.0, y: 50.0 })
        );

        let touch = PlatformEvent::Input(InputEvent::Touch {
            id: 7,
            phase: api::host::TouchPhase::Moved,
            x: 200.0,
            y: 100.0,
        });
        assert_eq!(
            translate_event(Some(translation), &touch),
            PlatformEvent::Input(InputEvent::Touch {
                id: 7,
                phase: api::host::TouchPhase::Moved,
                x: 100.0,
                y: 50.0,
            })
        );

        let key = PlatformEvent::Input(InputEvent::Key {
            code: api::input::PhysicalKey::Enter,
            pressed: true,
        });
        assert_eq!(translate_event(Some(translation), &key), key);
        assert_eq!(translate_event(None, &key), key);
    }

    /// A directory launch names no archive, so the project path is
    /// reconstructed. The conventional boot archive `data.xp3` wins even
    /// though discovery sorts `bgimage.xp3` before it.
    // The reference only asks whether the project path is a storage and
    // appends the archive delimiter when it is
    // (`reference/cpp/core/base/impl/SysInitImpl.cpp:160-165`).
    #[test]
    fn directory_project_prefers_data_xp3() {
        let base = Path::new("/game");
        let discovered = [
            PathBuf::from("/game/bgimage.xp3"),
            PathBuf::from("/game/data.xp3"),
        ];
        assert_eq!(
            directory_project(base, &discovered, |_| None, |_| true),
            PathBuf::from("/game/data.xp3")
        );
    }

    /// The conventional name is matched without regard to case.
    #[test]
    fn directory_project_matches_the_data_name_case_insensitively() {
        let base = Path::new("/game");
        let discovered = [PathBuf::from("/game/DATA.XP3")];
        assert_eq!(
            directory_project(base, &discovered, |_| None, |_| true),
            PathBuf::from("/game/DATA.XP3")
        );
    }

    /// A `data.xp3` that did not mount is not the project: the archive that
    /// holds the boot scripts and did mount takes its place.
    // The reference appends the archive delimiter to the project path only
    // when that path is a storage
    // (`reference/cpp/core/base/impl/SysInitImpl.cpp:160-165`), so a path
    // that names no readable archive cannot be one.
    #[test]
    fn directory_project_skips_a_data_archive_that_did_not_mount() {
        let base = Path::new("/game");
        let discovered = [
            PathBuf::from("/game/data.xp3"),
            PathBuf::from("/game/bbb.xp3"),
        ];
        assert_eq!(
            directory_project(
                base,
                &discovered,
                |path| path.ends_with("bbb.xp3").then_some(1),
                |path| !path.ends_with("data.xp3"),
            ),
            PathBuf::from("/game/bbb.xp3")
        );
    }

    /// Without `data.xp3`, the archive that holds the boot scripts is the
    /// project.
    #[test]
    fn directory_project_falls_back_to_the_boot_script_archive() {
        let base = Path::new("/game");
        let discovered = [
            PathBuf::from("/game/aaa.xp3"),
            PathBuf::from("/game/bbb.xp3"),
        ];
        assert_eq!(
            directory_project(
                base,
                &discovered,
                |path| path.ends_with("bbb.xp3").then_some(1),
                |_| true,
            ),
            PathBuf::from("/game/bbb.xp3")
        );
    }

    /// A game ships the archive it is played from and a patch archive, and
    /// both carry a copy of `startup.tjs`. The project is the archive the game
    /// is played from, so the data archive wins over the patch: the patch
    /// holds the changed files and the game registers it itself as an auto
    /// path, so its files are reached through the auto path and not through
    /// the project.
    ///
    /// The reference takes the project from the launcher, which asks the user
    /// to pick an archive (`reference/cpp/core/environ/win32/Platform.cpp:89-104`);
    /// a directory launch has no user, so the port picks the data archive.
    #[test]
    fn directory_project_prefers_the_data_archive_over_a_patch_archive() {
        let base = Path::new("/game");
        let discovered = [
            PathBuf::from("/game/patch.xp3"),
            PathBuf::from("/game/game.xp3"),
        ];
        assert_eq!(
            directory_project(
                base,
                &discovered,
                |path| {
                    if path.ends_with("patch.xp3") {
                        Some(275)
                    } else {
                        Some(2671)
                    }
                },
                |_| true,
            ),
            PathBuf::from("/game/game.xp3")
        );
    }

    /// With no boot archive, the directory is the folder project.
    #[test]
    fn directory_project_falls_back_to_the_folder() {
        let base = Path::new("/game");
        assert_eq!(
            directory_project(base, &[], |_| None, |_| true),
            base.to_path_buf()
        );
    }

    /// A `data.xp3` that did not mount and holds no boot archive leaves the
    /// folder as the project.
    #[test]
    fn directory_project_falls_back_to_the_folder_when_data_did_not_mount() {
        let base = Path::new("/game");
        let discovered = [PathBuf::from("/game/data.xp3")];
        assert_eq!(
            directory_project(base, &discovered, |_| None, |_| false),
            base.to_path_buf()
        );
    }

    /// A directory launch whose conventional `data.xp3` cannot be mounted
    /// keeps the folder as the project, so the loose files next to it stay
    /// reachable.
    ///
    /// The project archive is opened on demand for a relative lookup, so an
    /// archive that failed to mount fails the lookup instead of serving the
    /// file. The reference asks the same question of the project path — is it
    /// a storage? — and appends the archive delimiter when it is
    /// (`reference/cpp/core/base/impl/SysInitImpl.cpp:160-165`).
    #[test]
    fn directory_launch_skips_an_unmountable_data_archive() {
        let _lock = vm_lock();
        let base = unique_temp_dir("unmountable-data");
        std::fs::create_dir_all(&base).expect("game dir");
        std::fs::write(base.join("data.xp3"), b"not an xp3 archive").expect("corrupt archive");
        std::fs::write(base.join("startup.tjs"), "// no-op startup\n").expect("startup script");
        let host = crate::Host::new(crate::HostConfig::headless()).expect("headless host");
        let game = GameHost::open(host, Vec::new(), base.clone())
            .expect("the folder project reaches the loose startup script");
        assert_eq!(
            game.storage().archive_count(),
            0,
            "the archive that failed to mount is not mounted"
        );
        drop(game);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The boot-script predicate reads the mounted archives: an archive with
    /// `startup.tjs` holds the boot script, an archive with only
    /// `system/Initialize.tjs` holds the fallback, and an archive with neither
    /// holds neither. A relative archive path resolves against the app
    /// directory, the way the mount resolves it. The entry count of a
    /// boot-script archive is reported, so the data archive can be told from
    /// a patch archive.
    ///
    /// The reference checks an archive for the boot script by the normalized
    /// entry name (`reference/cpp/core/base/impl/StorageImpl.cpp:562-590`),
    /// and runs `system/Initialize.tjs` when `startup.tjs` is absent
    /// (`reference/cpp/core/base/ScriptMgnIntf.cpp:958-965`).
    #[test]
    fn boot_script_entries_reads_the_mounted_archives() {
        let base = unique_temp_dir("boot-scripts");
        std::fs::create_dir_all(&base).expect("game dir");
        assert!(base.is_absolute(), "the app directory is absolute");
        let startup = base.join("startup.xp3");
        let initialize = base.join("initialize.xp3");
        let plain = base.join("plain.xp3");
        std::fs::write(
            &startup,
            testkit::build_xp3(&[("startup.tjs", b"// startup\n", false)]),
        )
        .expect("write the startup archive");
        std::fs::write(
            &initialize,
            testkit::build_xp3(&[("system/Initialize.tjs", b"// initialize\n", false)]),
        )
        .expect("write the initialize archive");
        std::fs::write(
            &plain,
            testkit::build_xp3(&[("readme.txt", b"nothing to boot\n", false)]),
        )
        .expect("write the plain archive");

        let storage = Storage::mount_archives(
            FileSystem::host(base.clone()),
            base.clone(),
            [&startup, &initialize, &plain],
        )
        .expect("mount the archives");

        assert_eq!(
            boot_script_entries(&storage, &startup),
            Some(1),
            "an archive with startup.tjs holds the boot script"
        );
        assert_eq!(
            boot_script_entries(&storage, &initialize),
            Some(1),
            "an archive with only system/Initialize.tjs holds the fallback"
        );
        assert_eq!(
            boot_script_entries(&storage, &plain),
            None,
            "an archive with neither holds no boot script"
        );
        assert_eq!(
            boot_script_entries(&storage, Path::new("startup.xp3")),
            Some(1),
            "a relative archive path resolves against the app directory"
        );
        assert_eq!(
            boot_script_entries(&storage, Path::new("plain.xp3")),
            None,
            "a relative archive path resolves against the app directory"
        );
        assert_eq!(
            boot_script_entries(&storage, Path::new("missing.xp3")),
            None,
            "an archive that is not mounted holds no boot script"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A directory launch with only `boot.xp3` uses it as the project: the
    /// relative name resolves inside the project archive, ahead of the loose
    /// file with the same name.
    ///
    /// The reference makes the project path the current directory and appends
    /// the archive delimiter when that path is a storage
    /// (`reference/cpp/core/base/impl/SysInitImpl.cpp:160-165`), so the
    /// current directory is the archive and its files shadow the app path.
    #[test]
    fn directory_launch_uses_the_boot_archive_as_the_project() {
        let _lock = vm_lock();
        let base = unique_temp_dir("boot-archive-project");
        std::fs::create_dir_all(&base).expect("game dir");
        std::fs::write(
            base.join("boot.xp3"),
            testkit::build_xp3(&[
                ("startup.tjs", b"var booted = 1;\n", false),
                ("marker.txt", b"archive\n", false),
            ]),
        )
        .expect("write the boot archive");
        // The same name on disk: the project archive is searched first.
        std::fs::write(base.join("marker.txt"), b"disk\n").expect("loose marker");

        let host = crate::Host::new(crate::HostConfig::headless()).expect("headless host");
        let game = GameHost::open(host, Vec::new(), base.clone()).expect("open game");
        let location = game.storage().find("marker.txt").expect("find the marker");
        assert!(
            matches!(location, Some(engine::storage::Location::Archive(_, _))),
            "the project archive serves the relative name, got {location:?}"
        );
        match game.vm().eval("booted", "t").expect("eval `booted`") {
            tjs::TjsValue::Integer(1) => {}
            other => panic!("expected Integer 1 for `booted`, got {other:?}"),
        }
        drop(game);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A single explicit archive is the project.
    #[test]
    fn explicit_project_uses_the_named_archive() {
        let base = Path::new("/game");
        let archives = [PathBuf::from("/game/extra.xp3")];
        assert_eq!(
            explicit_project(base, &archives, |_| None),
            PathBuf::from("/game/extra.xp3")
        );
    }

    /// An explicit list prefers the archive that holds the boot scripts, not
    /// the first one named.
    #[test]
    fn explicit_project_prefers_the_boot_script_archive() {
        let base = Path::new("/game");
        let archives = [
            PathBuf::from("/game/data.xp3"),
            PathBuf::from("/game/patch.xp3"),
        ];
        assert_eq!(
            explicit_project(base, &archives, |path| {
                path.ends_with("patch.xp3").then_some(1)
            }),
            PathBuf::from("/game/patch.xp3")
        );
    }

    /// An explicit list with a patch archive and a data archive, both
    /// holding the boot scripts, takes the data archive: the same choice a
    /// directory launch makes.
    #[test]
    fn explicit_project_prefers_the_data_archive_over_a_patch_archive() {
        let base = Path::new("/game");
        let archives = [
            PathBuf::from("/game/patch.xp3"),
            PathBuf::from("/game/game.xp3"),
        ];
        assert_eq!(
            explicit_project(base, &archives, |path| {
                if path.ends_with("patch.xp3") {
                    Some(275)
                } else {
                    Some(2671)
                }
            }),
            PathBuf::from("/game/game.xp3")
        );
    }

    /// When no named archive holds the boot scripts, the first named archive
    /// is the project.
    #[test]
    fn explicit_project_falls_back_to_the_first_archive() {
        let base = Path::new("/game");
        let archives = [
            PathBuf::from("/game/data.xp3"),
            PathBuf::from("/game/patch.xp3"),
        ];
        assert_eq!(
            explicit_project(base, &archives, |_| None),
            PathBuf::from("/game/data.xp3")
        );
    }
}
