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

use compat::{CompatCtx, CompatError, FrameBody};
use engine::Storage;
use engine::storage::StartupPlan;
use fs::FileSystem;
use platform::{PlatformError, PlatformEvent};
use vm::{FrameCtx, VmHost};

use crate::audio::AudioOutput;
use crate::{Host, HostError, StepReport};

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

/// The ordered frame-body polls the game host runs each frame (doc 08 §6).
#[derive(Default)]
struct Polls {
    /// Elapsed milliseconds since the previous frame, forwarded to the layer
    /// transition step.
    dt_ms: f64,
}

impl FrameBody for Polls {
    fn timer_poll(&mut self, ctx: &mut CompatCtx) -> Result<(), CompatError> {
        compat::timer_poll(ctx).map(|_fired| ())
    }

    fn sound_poll(&mut self, ctx: &mut CompatCtx) -> Result<(), CompatError> {
        compat::sound_poll(ctx)
    }

    fn video_poll(&mut self, ctx: &mut CompatCtx) -> Result<(), CompatError> {
        compat::video_poll(ctx)
    }

    fn async_trigger_poll(&mut self, ctx: &mut CompatCtx) -> Result<(), CompatError> {
        compat::async_trigger_poll(ctx)
    }

    fn continuous_handler_poll(&mut self, ctx: &mut CompatCtx) -> Result<(), CompatError> {
        compat::continuous_handler_poll(ctx).map(|_remaining| ())
    }

    #[allow(unsafe_code)]
    fn project_and_render(&mut self, ctx: &mut CompatCtx) -> Result<(), CompatError> {
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
}

impl GameHost {
    /// Mounts `archives` in order (later archives win), binds the VM to
    /// `host`'s scene and the storage, installs the L1 codec seams, and runs
    /// the L2 startup sequence.
    ///
    /// `base_dir` is the app path (loose files, `patch.tjs`,
    /// `AfterStartup.tjs`).
    ///
    /// # Errors
    ///
    /// Returns [`GameError`] when the audio device cannot be opened (typed,
    /// headless hosts use the null backend and cannot fail here), when
    /// an archive cannot be mounted, the VM cannot be created, or the startup
    /// sequence fails.
    #[allow(unsafe_code)]
    pub fn open(host: Host, archives: Vec<PathBuf>, base_dir: PathBuf) -> Result<Self, GameError> {
        // L1 audio first: a windowed host opens the real device and fails
        // fast if none exists; a headless host gets the null backend.
        let audio = AudioOutput::open(host.is_headless())?;
        let mut host = Box::new(host);
        let storage = Box::new(Storage::mount_archives(
            FileSystem::host(base_dir.clone()),
            &base_dir,
            archives,
        )?);
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

        // Install the L1 codec seams the compat natives compose. Both clone
        // their backend, so the locals may drop here.
        let media = media::Media::new();
        crate::install_audio_decoder(vm.compat(), &media);
        crate::install_image_codec(vm.compat(), &media, &storage);

        let plan = storage.startup_plan()?;
        run_startup(&vm, &storage, &plan)?;
        Ok(Self {
            audio,
            vm,
            storage,
            host,
            polls: Polls::default(),
            now_ms: 0,
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
    /// The L1 audio drain runs between the two: after `tick` (whose
    /// `sound_poll` advanced the mixer to the L1 clock) a dt-derived chunk of
    /// the mixer is rendered through `compat::sound_render` and written to
    /// the device (policy documented in the `audio` module).
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
        self.now_ms = self.now_ms.saturating_add(dt as u64);
        let frame = FrameCtx {
            now_ms: self.now_ms,
            dt_ms: dt as u32,
            body: &mut self.polls,
            events,
        };
        self.vm.tick(frame)?;
        self.audio.pump(&self.vm, dt)?;
        Ok(self.host.step(dt_ms)?)
    }
}

/// Runs the resolved startup sequence (reference `TVPExecuteStartupScript`).
///
/// `patch.tjs` and `AfterStartup.tjs` are app-path only and non-fatal (their
/// errors are reported but do not stop startup); `startup.tjs` is searched and
/// falls back to `system/Initialize.tjs` when it is absent or fails.
fn run_startup(vm: &VmHost, storage: &Storage, plan: &StartupPlan) -> Result<(), GameError> {
    if let Some(name) = &plan.patch
        && let Err(error) = compat::exec_storage(vm.engine(), storage, name)
    {
        log::warn!("startup: {name}: {error}");
    }

    match &plan.startup {
        Some(startup) => {
            if let Err(error) = compat::exec_storage(vm.engine(), storage, startup) {
                match &plan.fallback {
                    Some(fallback) => {
                        log::warn!("startup: {startup} failed ({error}); running {fallback}");
                        compat::exec_storage(vm.engine(), storage, fallback).map_err(|error| {
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
                compat::exec_storage(vm.engine(), storage, fallback).map_err(|error| {
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
        && let Err(error) = compat::exec_storage(vm.engine(), storage, name)
    {
        log::warn!("startup: {name}: {error}");
    }
    Ok(())
}
