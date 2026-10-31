//! The L3-facing engine surface: every L2 item the compatibility layer names.
//!
//! This module is the single seam between `compat` and the `engine` crate.
//! Source files under `crates/compat/src` name the engine through
//! `crate::engine_api` and never through `engine` directly, so the re-exports
//! below are the engine contract the compatibility layer codes against.
//!
//! The seam is grouped by engine module. Every item is L2 state or an L2
//! action on it: L2 owns the layer tree, storage namespace, input/timer
//! registries, text engine, mixer, video overlays and transitions. Coordinates
//! are VM pixels — the engine applies the device mapping internally and L3
//! never names a device pixel. Fallible operations report their own typed
//! error (`StorageReadError`, `TextError`, `SoundError`, …).
//!
//! [`CompatServices`](crate::CompatServices) attaches raw pointers to the
//! stateful handles ([`Scene`], [`Storage`], [`InputState`], [`TimerState`],
//! [`FrameClock`], [`FrameTimeSource`]); each pointer must stay valid for as
//! long as the binding is installed.

// The logical scene: layers, bitmaps, fonts, windows, and decoded-image keys.
// The layer geometry operations L3 drives (`internal_set_image_size`,
// `set_layer_image_size`, `layer_size_changed`, the cache-count pair) are
// methods on the re-exported `Scene`.
pub(crate) use engine::{
    BitmapState, DecodedImageKey, DecodedImageMode, FontState, LayerState, Rect, Scene,
    TransitionFrame,
};

// The hit test's candidate: the layer that hit and the point in its own
// coordinates.
pub(crate) use engine::scene::HitCandidate;

// The storage namespace: mounts, archive names, locations, the storage-media
// providers and typed read errors.
pub(crate) use engine::{
    Location, Storage, StorageMedia, StorageReadError, normalize_in_archive_name,
};

// The image decode pool, the pending-image future and the pass a worker runs
// over decoded pixels: the visual natives submit decodes, wait for them and
// name their pass through these.
pub(crate) use engine::imagedecode::{DecodePass, DecodePool, PendingImage};

// The generic input state the platform events are accumulated into.
pub(crate) use engine::input::InputState;

// The timer registry, its ids and the stored callback shape.
pub(crate) use engine::timer::{TimerCallback, TimerId, TimerState};

// The frame clock the timers, the sound poll and the transitions read, and the
// clock source `System.getTickCount` reads on demand. The source is the same
// one the engine paces the frames from, so the script's clock and the frame's
// clock are one base read at two moments.
pub(crate) use engine::clock::{FrameClock, FrameTimeSource};

// Text shaping, coverage rasterization, the glyph cache and font identity.
pub(crate) use engine::text::{
    CoverageMask, FontId, TextCoverageCache, TextEngine, TextError, TextStyle, blit_coverage_into,
    blur_coverage, blurred_text_coverage, compose_text_into, rasterize_text_coverage,
};

// The sound mixer, its stream/buffer/channel state and typed errors.
pub(crate) use engine::sound::{
    AudioStream, LOOP_FLAG_COUNT, LoopCondition, LoopInfo, LoopLink, Mixer, SoundBuffer,
    SoundChannel, SoundError, SoundEvent, SoundLabel, SoundSource, Status,
};

// Video-overlay playback state, events, bounds and layer blits.
pub(crate) use engine::video::{
    PlayStatus, VideoBounds, VideoEvent, VideoOverlay, blit_frame_to_layer,
    blit_frame_to_layer_opacity,
};

// The transition state machine and its rule/scroll/destination vocabulary.
pub(crate) use engine::trans::{
    DestAlpha, RuleMask, ScrollFrom, ScrollStay, Trans, TransError, TransKind,
};

// The custom-shader draw a transition records when the backend can run effect
// pipelines. The compatibility layer carries the value from the transition
// tick to the layer tree; it never names a program, a pipeline or a shader.
pub(crate) use engine::shader::EffectDraw;

/// Rasterization primitives compat composes directly.
pub(crate) mod raster {
    pub(crate) use engine::raster::{
        blend_pixel, fill_polygon, fill_rect_replace, flatten_arc, flatten_cubic, hatch_foreground,
    };
}

/// Layer composition and pixel-region operations compat composes directly.
pub(crate) mod layer_ops {
    pub(crate) use engine::layer_ops::{
        AffineBlit, BlitOptions, BlitSource, COPY_MAIN, COPY_MASK, GammaAdjust, RectI,
        adjust_gamma, affine_blit, affine_blit_clipped, affine_blit_clipped_on,
        affine_blit_clipped_pixel_center_on, blit_copy, blit_over, blit_plane,
        blur_bitmap_in_place, clip_rect, convert_add_alpha_to_alpha, convert_alpha_to_add_alpha,
        copy_9patch, do_gray_scale, ensure_transition_display, ensure_transition_self_image,
        fill_color_hold_alpha, fill_color_on_add_alpha, fill_color_on_alpha, fill_mask, flip_lr,
        flip_ud, gaussian_blur, intersect, intersect_rect, light_contrast, operate_rect,
        operate_rect_on, remove_const_opacity, render_layer_subtree, stretch_blit_mode,
        stretch_blit_on, stretch_blit_plane,
    };
}
