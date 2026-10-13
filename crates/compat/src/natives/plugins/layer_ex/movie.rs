//! `LayerExMovie`: movie playback into a layer.
//!
//! Reference: `reference/cpp/plugins/layerExMovie.cpp`. The C++ class is a
//! `layerExBase_GL`-derived native instance attached to `Layer` with
//! `NCB_ATTACH_CLASS_WITH_HOOK`; it opens a movie, drives it from the
//! continuous-event hook and copies the decoded front buffer into the layer.
//!
//! The port uses the L2 [`engine::video::VideoOverlay`] playback machine for
//! the clock/loop/event state and the L2 [`engine::layer_ops`] primitives for
//! the per-frame copy. L3 does not link a decoder (the same rule as the
//! `VideoOverlay` native): decoded frames come from a host-installed
//! [`VideoSource`], set with [`set_movie_source`]. The host drives
//! [`movie_poll`] once per frame (the analogue of
//! `TVPAddContinuousEventHook`).
//!
//! Surface (reference `NCB_ATTACH_CLASS_WITH_HOOK(layerExMovie, Layer)`):
//! `Layer.openMovie` / `startMovie` / `stopMovie` / `isPlayingMovie`, plus
//! the `onStartMovie` / `onStopMovie` / `onUpdateMovie` callbacks. The
//! callbacks are registered as no-op native methods so a script can override
//! them on the layer instance (the reference reads the script members).
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};

use api::blend::BlendMode;
use engine::video::{VideoEvent, VideoOverlay, blit_frame_to_layer};
use tjs::{DetachedValue, NativeObjThisMethodDef, Tjs2Engine, Value};

use super::{VideoSource, bound, layer_from_objthis, scene_mut};
use crate::native_util::{args, report_error, set_bool, set_void};
use crate::natives::video::{MovieAudio, decoded_audio};
use crate::storage::strip_file_media;
use crate::{CompatCtx, CompatError, CompatServices};

// ---------------------------------------------------------------------------
// Per-layer playback state
// ---------------------------------------------------------------------------

/// One open movie bound to a layer.
struct MovieSlot {
    /// Destination layer id.
    layer: u32,
    /// The L2 playback machine (position, loop, status, current frame).
    overlay: VideoOverlay,
    /// The path the source was opened with.
    path: String,
    /// The reference `alpha` flag (half-width alpha video).
    alpha: bool,
    /// Movie dimensions after the reference's alpha halving.
    movie_width: i32,
    movie_height: i32,
    /// Whether the continuous hook is active (reference `playing`).
    playing: bool,
    /// The retained `Layer` script object, used to dispatch the callbacks.
    layer_obj: Option<DetachedValue>,
    /// The decoded movie audio and the shared-mixer voice it plays on. The
    /// reference plays a movie layer's audio through the player's own sound
    /// device at the device defaults; `layerExMovie` registers no volume/pan
    /// members (`reference/cpp/plugins/layerExMovie.cpp:337-341`), so the
    /// unity gain and centre pan stay untouched.
    movie_audio: Option<MovieAudio>,
}

impl MovieSlot {
    fn new(layer: u32) -> Self {
        Self {
            layer,
            overlay: VideoOverlay::new(),
            path: String::new(),
            alpha: false,
            movie_width: 0,
            movie_height: 0,
            playing: false,
            layer_obj: None,
            movie_audio: None,
        }
    }

    /// Sync the mixer voice with the overlay's current playback state at the
    /// device defaults (unity gain, centre pan).
    fn sync_audio(&self, services: &CompatServices) {
        if let Some(audio) = &self.movie_audio {
            audio.sync(services, &self.overlay, 1.0, 0.0);
        }
    }

    /// Drop the mixer voice, if any.
    fn release_audio(&mut self, services: &CompatServices) {
        if let Some(audio) = self.movie_audio.take() {
            audio.detach(services);
        }
    }
}

/// The movie family's dispatch state.
#[derive(Default)]
struct LayerExMovieState {
    /// Host-installed decoded source (see [`set_movie_source`]).
    source: Option<Box<dyn VideoSource>>,
    /// Layer id → playback slot.
    slots: HashMap<u32, MovieSlot>,
}

/// Install the decoded-movie source `LayerExMovie.openMovie` reads from.
///
/// L3 owns no decoder: the host installs the same [`VideoSource`] it uses for
/// `VideoOverlay` (or any other implementation) so `openMovie` can read the
/// stream metadata and the frame body can present frames.
pub fn set_movie_source(ctx: &CompatCtx, source: Box<dyn VideoSource>) {
    // SAFETY: the caller holds a live, bound context.
    let services = unsafe { ctx.services() };
    services.state::<LayerExMovieState>().source = Some(source);
}

fn value_string(v: &Value) -> String {
    crate::native_util::value_as_string(v)
}

// ---------------------------------------------------------------------------
// Core operations
// ---------------------------------------------------------------------------

/// Reference `layerExMovie::openMovie`: open the stream metadata, size the
/// layer, set its type, install the decoded audio track on the shared mixer
/// and enter the stopped state.
fn open_movie(ctx: &CompatCtx, layer_id: u32, filename: &str, alpha: bool) -> Result<(), String> {
    // SAFETY: the caller holds a live, bound context.
    let services = unsafe { ctx.services() };
    // The reference opens the movie through `TVPCreateStream(filename, ...)`
    // (`reference/cpp/plugins/layerExMovie.cpp:160`), which normalizes the
    // media name before the storage lookup; the L2 namespace takes the bare
    // name. The script spelling stays in the diagnostics.
    let lookup = strip_file_media(filename);
    let (spec, audio) = {
        let mut state = services.state::<LayerExMovieState>();
        let source = state
            .source
            .as_mut()
            .ok_or_else(|| "Layer.openMovie: no movie source installed".to_string())?;
        let spec = source
            .spec(lookup)
            .ok_or_else(|| format!("Layer.openMovie: no video metadata for {filename:?}"))?;
        let audio = decoded_audio(source.as_mut(), lookup, spec.audio_streams)
            .map_err(|error| format!("Layer.openMovie: {error}"))?;
        (spec, audio)
    };

    let (movie_width, movie_height) = if alpha {
        (spec.width / 2, spec.height)
    } else {
        (spec.width, spec.height)
    };
    {
        let scene = scene_mut(ctx)?;
        if !scene.set_layer_image_size(layer_id, movie_width, movie_height) {
            return Err("Layer.openMovie: could not allocate the movie image".to_string());
        }
        if let Some(layer) = scene.layer_mut(layer_id) {
            layer.rect.w = movie_width;
            layer.rect.h = movie_height;
            layer.image_width = movie_width;
            layer.image_height = movie_height;
            layer.blend = if alpha {
                BlendMode::SourceOver
            } else {
                BlendMode::Replace
            };
        }
    }

    let mut state = services.state::<LayerExMovieState>();
    let slot = state
        .slots
        .entry(layer_id)
        .or_insert_with(|| MovieSlot::new(layer_id));
    slot.release_audio(services);
    slot.overlay.close();
    slot.overlay
        .open(spec)
        .map_err(|error| format!("Layer.openMovie: {error}"))?;
    slot.path = lookup.to_string();
    slot.alpha = alpha;
    slot.movie_width = movie_width as i32;
    slot.movie_height = movie_height as i32;
    if let Some(decoded) = audio {
        match MovieAudio::attach(services, decoded) {
            Ok(movie_audio) => {
                slot.movie_audio = Some(movie_audio);
                slot.sync_audio(services);
            }
            Err(error) => {
                slot.overlay.close();
                slot.path.clear();
                return Err(format!("Layer.openMovie: cannot play movie audio: {error}"));
            }
        }
    }
    Ok(())
}

/// Reference `layerExMovie::startMovie`: start playback and fire
/// `onStartMovie`.
fn start_movie(
    ctx: &CompatCtx,
    host: &Tjs2Engine,
    layer_id: u32,
    looping: bool,
) -> Result<(), String> {
    // SAFETY: the caller holds a live, bound context.
    let services = unsafe { ctx.services() };
    {
        let mut state = services.state::<LayerExMovieState>();
        let slot = state
            .slots
            .get_mut(&layer_id)
            .ok_or_else(|| "Layer.startMovie: no movie opened".to_string())?;
        slot.overlay.looping = looping;
        slot.overlay
            .play()
            .map_err(|error| format!("Layer.startMovie: {error}"))?;
        slot.playing = true;
        slot.sync_audio(services);
    }
    dispatch_callback(ctx, host, layer_id, "onStartMovie");
    Ok(())
}

/// Reference `layerExMovie::stopMovie` (`reference/cpp/plugins/
/// layerExMovie.cpp:214-227`): stop the overlay, remove the continuous
/// hook, and *release* the movie (`clearMovie()`, `:219`), firing
/// `onStopMovie` only when it had been playing (`:220-226`). Releasing the
/// overlay means a later `startMovie` without a fresh `openMovie` finds no
/// movie — the port reports that as a typed error instead of the
/// reference's silent `if(VideoOverlay)` skip (`:199`).
fn stop_movie(ctx: &CompatCtx, host: &Tjs2Engine, layer_id: u32) -> Result<(), String> {
    // SAFETY: the caller holds a live, bound context.
    let services = unsafe { ctx.services() };
    let was_playing = {
        let mut state = services.state::<LayerExMovieState>();
        let Some(slot) = state.slots.get_mut(&layer_id) else {
            return Ok(());
        };
        let was = slot.playing;
        let _ = slot.overlay.stop();
        slot.overlay.clear_frame();
        slot.overlay.close();
        slot.playing = false;
        slot.release_audio(services);
        was
    };
    if was_playing {
        dispatch_callback(ctx, host, layer_id, "onStopMovie");
    }
    Ok(())
}

/// Reference `layerExMovie::isPlayingMovie`.
fn is_playing(ctx: &CompatCtx, layer_id: u32) -> bool {
    // SAFETY: the caller holds a live, bound context.
    let services = unsafe { ctx.services() };
    let state = services.state::<LayerExMovieState>();
    state.slots.get(&layer_id).is_some_and(|slot| slot.playing)
}

/// Retain the layer object from `objthis` and store it on the slot.
fn remember_layer_object(ctx: &CompatCtx, host: &Tjs2Engine, layer_id: u32, objthis: *mut c_void) {
    if objthis.is_null() {
        return;
    }
    let Ok(object) = host.retain_object_detached(objthis) else {
        return;
    };
    // SAFETY: the caller holds a live, bound context.
    let services = unsafe { ctx.services() };
    let mut state = services.state::<LayerExMovieState>();
    let slot = state
        .slots
        .entry(layer_id)
        .or_insert_with(|| MovieSlot::new(layer_id));
    if slot.layer_obj.is_none() {
        slot.layer_obj = Some(object);
    }
}

/// Invoke `layer.<member>`. The callbacks are registered as no-op native
/// methods, so the member always exists; a script override wins.
fn dispatch_callback(ctx: &CompatCtx, host: &Tjs2Engine, layer_id: u32, member: &str) {
    // SAFETY: the caller holds a live, bound context.
    let services = unsafe { ctx.services() };
    let object = {
        let state = services.state::<LayerExMovieState>();
        state
            .slots
            .get(&layer_id)
            .and_then(|slot| slot.layer_obj.as_ref())
            .and_then(|object| host.duplicate_retained(object.raw_id()).ok())
    };
    let Some(object) = object else {
        return;
    };
    if let Err(error) = host.call_member(object.raw_id(), member, &[]) {
        log::warn!("LayerExMovie: {member}: {error}");
    }
}

/// Copy the overlay's current frame into the bound layer.
fn copy_frame(ctx: &CompatCtx, slot: &MovieSlot) {
    let Some(frame) = slot.overlay.current_frame() else {
        return;
    };
    let Ok(scene) = scene_mut(ctx) else {
        return;
    };
    if !blit_frame_to_layer(scene, slot.layer, frame, slot.alpha) {
        log::warn!(
            "LayerExMovie: layer #{} has no image to copy into",
            slot.layer
        );
    }
}

// ---------------------------------------------------------------------------
// Frame body
// ---------------------------------------------------------------------------

/// Advance every playing movie by `dt_ms`, present the frame at the new
/// position, copy it into the layer, sync the movie's audio voice and
/// dispatch the `onUpdateMovie` / `onStopMovie` callbacks.
///
/// The host calls this once per frame (reference
/// `layerExMovie::OnContinuousCallback`). It is a free function (like
/// `compat::video_poll`) so a host can drive it directly.
pub fn movie_poll(ctx: &CompatCtx, dt_ms: f64) -> Result<(), CompatError> {
    // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `movie_poll`.
    let host = unsafe { ctx.engine() };
    // SAFETY: the caller holds a live, bound context.
    let services = unsafe { ctx.services() };
    let dispatch: Vec<(u32, &'static str)> = {
        let mut state = services.state::<LayerExMovieState>();
        let mut source = state.source.take();
        let mut dispatch = Vec::new();
        for (&layer_id, slot) in state.slots.iter_mut() {
            if !slot.overlay.is_open() || !slot.playing {
                continue;
            }
            // Present the frame at the current position, advance, then
            // present at the new position so the copy below shows the frame
            // belonging to the new clock value.
            present(&mut source, slot);
            slot.overlay.advance(dt_ms);
            let events = slot.overlay.poll();
            present(&mut source, slot);
            slot.sync_audio(services);
            copy_frame(ctx, slot);

            let mut ended = false;
            for event in &events {
                match event {
                    VideoEvent::FrameUpdate(_) => dispatch.push((layer_id, "onUpdateMovie")),
                    VideoEvent::TransitionComplete => ended = true,
                    // Status changes and period events have no movie callback
                    // (the reference `layerExMovie` only handles update/end).
                    VideoEvent::StatusChanged(_) | VideoEvent::Period(_) => {}
                }
            }
            if ended && !slot.overlay.looping {
                // Reference `onEnded` posts `stopMovie` for a non-looping
                // movie (`reference/cpp/plugins/layerExMovie.cpp:281-289`),
                // which stops playback, releases the overlay through
                // `clearMovie()` and fires `onStopMovie` (`:214-227`).
                let _ = slot.overlay.stop();
                slot.overlay.clear_frame();
                slot.overlay.close();
                slot.playing = false;
                slot.release_audio(services);
                dispatch.push((layer_id, "onStopMovie"));
            }
        }
        state.source = source;
        dispatch
    };
    for (layer_id, member) in dispatch {
        dispatch_callback(ctx, host, layer_id, member);
    }
    Ok(())
}

/// Present the host frame at the overlay's current position, if any.
fn present(source: &mut Option<Box<dyn VideoSource>>, slot: &mut MovieSlot) {
    let Some(source) = source.as_mut() else {
        return;
    };
    let at = slot.overlay.position_ms();
    if let Some(frame) = source.frame_at(&slot.path, at)
        && let Err(error) = slot.overlay.present_frame(frame)
    {
        log::warn!("LayerExMovie: inconsistent decoded frame: {error}");
    }
}

// ---------------------------------------------------------------------------
// `Layer` members
// ---------------------------------------------------------------------------

/// `Layer.openMovie(filename, alpha)` — reference arity 2
/// (`void openMovie(const tjs_char *filename, bool alpha)`,
/// `reference/cpp/plugins/layerExMovie.cpp:72`, registered with
/// `NCB_METHOD(openMovie)` at `:338`, so NCB raises `TJS_E_BADPARAMCOUNT`
/// for fewer arguments — `reference/cpp/core/plugin/ncbind.hpp:1178`).
extern "C" fn layer_open_movie(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Layer.openMovie requires 2 arguments");
    }
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.openMovie: compat context is not bound");
    };
    let id = match layer_from_objthis(resolved.engine(), objthis) {
        Ok(Some(id)) => id,
        Ok(None) => return report_error(out_error, "Layer.openMovie: the receiver is not a Layer"),
        Err(error) => return report_error(out_error, &format!("Layer.openMovie: {error}")),
    };
    remember_layer_object(&ctx, resolved.engine(), id, objthis);
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let filename = value_string(&a[0]);
    let alpha = a.get(1).is_some_and(crate::natives::system::value_as_bool);
    if let Err(error) = open_movie(&ctx, id, &filename, alpha) {
        return report_error(out_error, &error);
    }
    set_void(out);
    0
}

/// `Layer.startMovie(loop)` — reference arity 1 (`void startMovie(bool
/// loop)`, `reference/cpp/plugins/layerExMovie.cpp:74`, `NCB_METHOD` at
/// `:339` → `reference/cpp/core/plugin/ncbind.hpp:1178`).
extern "C" fn layer_start_movie(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Layer.startMovie requires 1 argument");
    }
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.startMovie: compat context is not bound");
    };
    let id = match layer_from_objthis(resolved.engine(), objthis) {
        Ok(Some(id)) => id,
        Ok(None) => {
            return report_error(out_error, "Layer.startMovie: the receiver is not a Layer");
        }
        Err(error) => return report_error(out_error, &format!("Layer.startMovie: {error}")),
    };
    remember_layer_object(&ctx, resolved.engine(), id, objthis);
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let looping = a.first().is_some_and(crate::natives::system::value_as_bool);
    if let Err(error) = start_movie(&ctx, resolved.engine(), id, looping) {
        return report_error(out_error, &error);
    }
    set_void(out);
    0
}

extern "C" fn layer_stop_movie(
    engine: *mut c_void,
    objthis: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.stopMovie: compat context is not bound");
    };
    let id = match layer_from_objthis(resolved.engine(), objthis) {
        Ok(Some(id)) => id,
        Ok(None) => return report_error(out_error, "Layer.stopMovie: the receiver is not a Layer"),
        Err(error) => return report_error(out_error, &format!("Layer.stopMovie: {error}")),
    };
    remember_layer_object(&ctx, resolved.engine(), id, objthis);
    if let Err(error) = stop_movie(&ctx, resolved.engine(), id) {
        return report_error(out_error, &error);
    }
    set_void(out);
    0
}

extern "C" fn layer_is_playing_movie(
    engine: *mut c_void,
    objthis: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.isPlayingMovie: compat context is not bound",
        );
    };
    let id = match layer_from_objthis(resolved.engine(), objthis) {
        Ok(Some(id)) => id,
        Ok(None) => {
            return report_error(
                out_error,
                "Layer.isPlayingMovie: the receiver is not a Layer",
            );
        }
        Err(error) => return report_error(out_error, &format!("Layer.isPlayingMovie: {error}")),
    };
    set_bool(out, is_playing(&ctx, id));
    0
}

/// The `Layer`-instance method definitions.
pub(super) fn layer_method_defs() -> Vec<NativeObjThisMethodDef> {
    vec![
        NativeObjThisMethodDef {
            name: "openMovie",
            f: layer_open_movie,
        },
        NativeObjThisMethodDef {
            name: "startMovie",
            f: layer_start_movie,
        },
        NativeObjThisMethodDef {
            name: "stopMovie",
            f: layer_stop_movie,
        },
        NativeObjThisMethodDef {
            name: "isPlayingMovie",
            f: layer_is_playing_movie,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CompatHost;
    use api::media::VideoSpec;
    use engine::Scene;
    use std::sync::MutexGuard;
    use tjs::TjsValue;

    fn vm_lock() -> MutexGuard<'static, ()> {
        crate::native_util::test_vm_lock()
    }

    struct TestSource {
        spec: VideoSpec,
        frames: Vec<api::media::RgbaFrame>,
    }

    impl VideoSource for TestSource {
        fn spec(&self, _path: &str) -> Option<VideoSpec> {
            Some(self.spec)
        }

        fn frame_at(&mut self, _path: &str, at_ms: i64) -> Option<api::media::RgbaFrame> {
            let mut chosen = self.frames.first()?.clone();
            for frame in &self.frames {
                if frame.pts_ms as i64 <= at_ms {
                    chosen = frame.clone();
                } else {
                    break;
                }
            }
            Some(chosen)
        }
    }

    struct Env {
        host: CompatHost,
        engine: Box<Tjs2Engine>,
        scene: Box<Scene>,
    }

    impl Env {
        fn new() -> Self {
            let engine = Box::new(Tjs2Engine::new().expect("create engine"));
            let mut scene = Box::new(Scene::default());
            let mut host = CompatHost::new();
            // SAFETY: the boxed scene outlives the host (declared after it).
            unsafe {
                host.services_mut()
                    .attach_scene(scene.as_mut() as *mut Scene);
            }
            host.install(&engine).expect("install compat surface");
            Self {
                host,
                engine,
                scene,
            }
        }

        fn run(&self, script: &str) {
            self.engine
                .exec_script(script, "layer-ex-movie-unit")
                .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
        }

        fn eval_int(&self, expression: &str) -> i64 {
            match self.engine.eval(expression, "layer-ex-movie-unit") {
                Ok(TjsValue::Integer(value)) => value,
                other => panic!("{expression:?} returned {other:?}"),
            }
        }

        /// The scene id of the layer the first test scripts create, read from
        /// the L2 scene the unit test owns. The reference resolves a `Layer`
        /// through `NativeInstanceSupport(TJS_NIS_GETINSTANCE,
        /// tTJSNC_Layer::ClassID, …)` (`reference/cpp/core/visual/
        /// VideoOvlIntf.cpp:382-403`, the `setMixingLayer` shape) — there is no
        /// `Layer.id` script member — so the assertion must not read one.
        fn layer_id(&self) -> u32 {
            self.scene
                .layers
                .iter()
                .find(|layer| layer.name == "movie")
                .expect("the movie test layer")
                .id
        }

        fn bitmap_color(&self, x: u32, y: u32) -> [u8; 4] {
            let id = self.layer_id();
            let bitmap_id = self
                .scene
                .layer(id)
                .and_then(|layer| layer.bitmap)
                .expect("layer bitmap");
            let bitmap = self.scene.bitmap(bitmap_id).expect("bitmap");
            let offset = bitmap.pixel_offset(x, y).expect("pixel in range");
            [
                bitmap.rgba[offset],
                bitmap.rgba[offset + 1],
                bitmap.rgba[offset + 2],
                bitmap.rgba[offset + 3],
            ]
        }
    }

    fn frame(color: [u8; 4]) -> api::media::RgbaFrame {
        let mut rgba = Vec::with_capacity(16 * 4);
        for _ in 0..16 {
            rgba.extend_from_slice(&color);
        }
        api::media::RgbaFrame::new(4, 4, 0, rgba)
    }

    fn spec() -> VideoSpec {
        VideoSpec {
            width: 4,
            height: 4,
            fps: 10.0,
            total_frames: Some(10),
            sample_rate: 0,
            channels: 0,
            ..VideoSpec::default()
        }
    }

    #[test]
    fn open_sizes_the_layer_and_playback_copies_frames() {
        // `layerExMovie::openMovie` sizes the layer to the movie and
        // `onUpdate` copies the current front buffer into the MainImage
        // (`reference/cpp/plugins/layerExMovie.cpp:154-196` and `:254-275`),
        // driven here through the port's `movie_poll`.
        let _lock = vm_lock();
        let env = Env::new();
        env.run(
            "var w = new Window(); var l = new Layer(w, null); l.name = 'movie'; \
             l.setImageSize(4, 4);",
        );
        let ctx = env.host.ctx().expect("bound context");
        set_movie_source(
            &ctx,
            Box::new(TestSource {
                spec: spec(),
                frames: vec![frame([255, 0, 0, 255])],
            }),
        );
        let id = env.layer_id();
        open_movie(&ctx, id, "movie.mpg", false).expect("open");
        start_movie(&ctx, &env.engine, id, false).expect("start");
        movie_poll(&ctx, 50.0).expect("poll");
        assert_eq!(env.bitmap_color(1, 1), [255, 0, 0, 255]);
        assert!(is_playing(&ctx, id));
    }

    #[test]
    fn callbacks_fire_through_the_layer_object() {
        // `onStartMovie`/`onUpdateMovie`/`onStopMovie` are invoked on the
        // layer object (`reference/cpp/plugins/layerExMovie.cpp:205,222,274`),
        // so a script override on the instance is what fires.
        let _lock = vm_lock();
        let env = Env::new();
        env.run(
            "var w = new Window(); var l = new Layer(w, null); l.setImageSize(4, 4); \
             var started = 0; var updated = 0; var stopped = 0; \
             l.onStartMovie = function() { started = 1; }; \
             l.onUpdateMovie = function() { updated = 1; }; \
             l.onStopMovie = function() { stopped = 1; };",
        );
        let ctx = env.host.ctx().expect("bound context");
        set_movie_source(
            &ctx,
            Box::new(TestSource {
                spec: spec(),
                frames: vec![frame([0, 255, 0, 255])],
            }),
        );
        env.run("l.openMovie('movie.mpg', false); l.startMovie(false);");
        assert_eq!(env.eval_int("started"), 1);
        movie_poll(&ctx, 50.0).expect("poll");
        assert_eq!(env.eval_int("updated"), 1);
        env.run("l.stopMovie();");
        assert_eq!(env.eval_int("stopped"), 1);
    }

    #[test]
    fn open_without_a_source_raises() {
        // The reference logs and returns when the stream cannot be opened
        // (`reference/cpp/plugins/layerExMovie.cpp:160-166`); the port owns
        // no decoder and reports the missing source as a typed error.
        let _lock = vm_lock();
        let env = Env::new();
        env.run(
            "var w = new Window(); var l = new Layer(w, null); \
             var caught = 0; \
             try { l.openMovie('movie.mpg', false); } catch (e) { caught = 1; }",
        );
        assert_eq!(env.eval_int("caught"), 1);
    }
}
