//! Integration tests for the backend-selected L2 text rasterization seam.
//!
//! `rasterize_run` turns one shaped [`TextLayout`](engine::text::TextLayout)
//! into either the CPU coverage form the layer bitmap consumes or the GPU
//! atlas form the render scene projects. Both forms must describe the same
//! glyph geometry, and the glyph atlas must stay at the reference character
//! cache bound. A machine without a usable test font skips the font-dependent
//! cases rather than failing.

use api::glyph::GlyphBackend;
use raw_window_handle::{RawWindowHandle, XlibWindowHandle};

use engine::projection::{Presentation, ScalingMode};
use engine::text::{
    GlyphAtlas, MAX_TEXT_CACHE_ENTRIES, RasterizedRun, TextCoverageCache, TextEngine, TextError,
    TextStyle, rasterize_run,
};
use render::{PipelineConfig, begin_render_state, create_render_pipeline};

/// System fonts tried by the tests; a missing font skips rather than fails.
const TEST_FONT_PATHS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
];

/// Reads the first available test font, if any.
fn load_test_font() -> Option<Vec<u8>> {
    TEST_FONT_PATHS
        .iter()
        .find_map(|path| std::fs::read(path).ok())
}

/// Builds a headless raw window handle for the null backend.
fn headless_window() -> RawWindowHandle {
    RawWindowHandle::Xlib(XlibWindowHandle::new(0))
}

/// A pipeline/state pair for the atlas tests.
fn null_pipeline() -> (render::RenderPipeline, render::Surface, render::RenderState) {
    let pipeline = create_render_pipeline(PipelineConfig::null()).expect("null pipeline");
    let (surface, state) = begin_render_state(&pipeline, &headless_window()).expect("null state");
    (pipeline, surface, state)
}

/// Reference: `tTVPFontAndCharacterData` keys the character cache on the face
/// and character (`reference/cpp/core/visual/CharacterData.h:89-115`) and
/// `TVPGetCharacter` rasterizes on a miss
/// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:244-320`). The glyph
/// rasterizer is chosen by the caller ([`api::glyph::GlyphBackend`]), but the
/// glyph positions and advances come from the single shaped layout, so both
/// backends must place each glyph at the same point. This compares the CPU
/// coverage mask's top-left and size with the GPU atlas glyph's destination
/// rectangle for the same string.
#[test]
fn cpu_and_gpu_runs_share_glyph_geometry() {
    let Some(bytes) = load_test_font() else {
        return;
    };
    let mut engine = TextEngine::new();
    let Ok(font) = engine.add_font(&bytes, 0) else {
        return;
    };
    let (pipeline, _surface, mut state) = null_pipeline();
    let style = TextStyle::new(24.0);
    let layout = engine
        .layout(font, "Hello", &style, None)
        .expect("layout of a Latin run");

    let mut cache = TextCoverageCache::new();
    let mut atlas = GlyphAtlas::new(&pipeline, 256).expect("atlas");
    let cpu = rasterize_run(
        GlyphBackend::Cpu,
        1.0,
        &mut cache,
        &mut atlas,
        &pipeline,
        &mut state,
        &engine,
        &layout,
        &style,
        true,
        1.0,
    )
    .expect("cpu run");
    let gpu = rasterize_run(
        GlyphBackend::Gpu,
        1.0,
        &mut cache,
        &mut atlas,
        &pipeline,
        &mut state,
        &engine,
        &layout,
        &style,
        true,
        1.0,
    )
    .expect("gpu run");

    let RasterizedRun::Coverage(masks) = cpu else {
        panic!("the cpu backend must return coverage masks");
    };
    let RasterizedRun::Atlas(glyphs) = gpu else {
        panic!("the gpu backend must return atlas glyphs");
    };
    assert_eq!(
        masks.len(),
        glyphs.len(),
        "one layout must yield the same glyph count on both backends"
    );
    assert!(!atlas.is_empty(), "the gpu run must fill the atlas");
    for (mask, positioned) in masks.iter().zip(glyphs.iter()) {
        let dst = positioned.destination().expect("every Latin glyph has ink");
        assert_eq!(mask.x(), dst.x, "glyph left edge differs between backends");
        assert_eq!(mask.y(), dst.y, "glyph top edge differs between backends");
        assert_eq!(mask.width() as f32, dst.width, "glyph width differs");
        assert_eq!(mask.height() as f32, dst.height, "glyph height differs");
    }
}

/// Reference: the `tTVPFontCache` character-cache limit is
/// `TVP_CH_MAX_CACHE_COUNT`
/// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:108`), and eviction is
/// least-recently-used (`FindAndTouchWithHash`/`ChopLast`,
/// `reference/cpp/core/tjs2/tjsHashSearch.h:358-364,424-433,593-599`). The GPU
/// glyph atlas keeps the same bound, so making it a selectable path cannot
/// grow its entry map without limit; `clear` reclaims the packed pixel space
/// between frames.
#[test]
fn glyph_atlas_is_bounded_and_clear_reclaims_space() {
    let Some(bytes) = load_test_font() else {
        return;
    };
    let mut engine = TextEngine::new();
    let Ok(font) = engine.add_font(&bytes, 0) else {
        return;
    };
    let (pipeline, _surface, _state) = null_pipeline();
    let mut atlas = GlyphAtlas::new(&pipeline, 1024).expect("atlas");

    // Distinct (codepoint, size) pairs so every request is a cache miss: 94
    // printable ASCII codepoints across 14 sizes is more than the bound.
    for index in 0..=MAX_TEXT_CACHE_ENTRIES {
        let codepoint = char::from_u32(0x21 + u32::try_from(index % 94).expect("small index"))
            .expect("ASCII codepoint");
        let style = TextStyle::new(8.0 + (index / 94) as f32);
        atlas
            .glyph(&engine, font, codepoint, &style)
            .expect("distinct glyph");
    }
    assert_eq!(
        atlas.len(),
        MAX_TEXT_CACHE_ENTRIES,
        "the atlas entry map must stay at the reference bound"
    );

    atlas.clear();
    assert!(atlas.is_empty(), "clear must drop every cached glyph");
    assert!(
        atlas.pixels().iter().all(|byte| *byte == 0),
        "clear must zero the CPU mirror"
    );
}

/// Reference: `TVPGetCharacter` rasterizes a character on a cache miss and
/// caches it (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:244-320`);
/// the atlas is the GPU form of that cache. A run of N new glyphs must be
/// queued and flushed as one upload batch, so a frame's upload work grows with
/// the number of text runs, not the number of characters. The null backend
/// records the uploaded pixels, which is the host-observable evidence that one
/// flush carried the whole run.
#[test]
fn atlas_batches_a_run_into_one_flush() {
    let Some(bytes) = load_test_font() else {
        return;
    };
    let mut engine = TextEngine::new();
    let Ok(font) = engine.add_font(&bytes, 0) else {
        return;
    };
    let (pipeline, _surface, mut state) = null_pipeline();
    let style = TextStyle::new(24.0);
    let layout = engine
        .layout(font, "Hello", &style, None)
        .expect("layout of a Latin run");
    let mut cache = TextCoverageCache::new();
    let mut atlas = GlyphAtlas::new(&pipeline, 256).expect("atlas");

    let gpu = rasterize_run(
        GlyphBackend::Gpu,
        1.0,
        &mut cache,
        &mut atlas,
        &pipeline,
        &mut state,
        &engine,
        &layout,
        &style,
        true,
        1.0,
    )
    .expect("gpu run");
    let RasterizedRun::Atlas(glyphs) = gpu else {
        panic!("the gpu backend must return atlas glyphs");
    };
    assert!(
        glyphs.len() > 1,
        "the run must contain more than one glyph, got {}",
        glyphs.len()
    );

    // `rasterize_layout` flushed the run, so no region is left queued.
    assert_eq!(
        atlas.pending_len(),
        0,
        "one flush must drain every glyph of the run"
    );

    // The single flush copied exactly the CPU mirror the glyphs wrote.
    let uploaded = render::NullRenderPipeline::texture_pixels(&state, atlas.texture())
        .expect("the flushed atlas must be uploaded");
    assert_eq!(
        uploaded,
        atlas.pixels(),
        "the GPU atlas must match the CPU mirror after one flush"
    );

    // A second flush with no new glyphs must not upload anything again, which
    // is the null-backend evidence that the first flush was a single batch.
    let bytes_after = render::NullRenderPipeline::texture_uploaded_bytes(&state, atlas.texture())
        .expect("uploaded byte count");
    atlas.flush(&pipeline, &mut state).expect("second flush");
    let bytes_again = render::NullRenderPipeline::texture_uploaded_bytes(&state, atlas.texture())
        .expect("uploaded byte count");
    assert_eq!(
        bytes_after, bytes_again,
        "a flush with nothing pending must upload nothing"
    );
}

/// Reference: the glyph atlas is a cache, not a per-character upload stream
/// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:244-320`). Rasterizing
/// glyphs only queues their regions; a single `flush` submits them together.
#[test]
fn glyphs_queue_until_one_flush() {
    let Some(bytes) = load_test_font() else {
        return;
    };
    let mut engine = TextEngine::new();
    let Ok(font) = engine.add_font(&bytes, 0) else {
        return;
    };
    let (pipeline, _surface, mut state) = null_pipeline();
    let style = TextStyle::new(24.0);
    let mut atlas = GlyphAtlas::new(&pipeline, 256).expect("atlas");

    atlas.glyph(&engine, font, 'A', &style).expect("glyph A");
    atlas.glyph(&engine, font, 'B', &style).expect("glyph B");
    atlas.glyph(&engine, font, 'C', &style).expect("glyph C");
    assert_eq!(
        atlas.pending_len(),
        3,
        "three new glyphs must be queued, not uploaded one by one"
    );

    atlas.flush(&pipeline, &mut state).expect("flush");
    assert_eq!(
        atlas.pending_len(),
        0,
        "one flush drains every queued glyph"
    );
}

/// Reference: `TVPGetCharacter` rasterizes a character at the requested font
/// height (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:244-320`) while
/// the shaped advance comes from the layout, not the bitmap. A presentation
/// that renders at the framebuffer scale therefore rasterizes the coverage at
/// `size_px * render_scale` pixels while the logical bearings, advance and
/// destination quad stay at `size_px`: the physical bitmap grows, the
/// script-visible geometry does not.
#[test]
fn fit_sharp_rasterizes_a_larger_bitmap_with_unchanged_logical_metrics() {
    let Some(bytes) = load_test_font() else {
        return;
    };
    let mut engine = TextEngine::new();
    let Ok(font) = engine.add_font(&bytes, 0) else {
        return;
    };
    let (pipeline, _surface, mut state) = null_pipeline();
    let style = TextStyle::new(24.0);
    let layout = engine
        .layout(font, "A", &style, None)
        .expect("layout of a Latin glyph");

    let design = (1280u32, 720u32);
    let framebuffer = (2560u32, 1440u32);
    let fit = Presentation::compute(ScalingMode::Fit, design, framebuffer);
    let fit_sharp = Presentation::compute(ScalingMode::FitSharp, design, framebuffer);
    assert_eq!(fit.view, fit_sharp.view, "FitSharp inherits Fit's geometry");
    assert_eq!(fit.logical_size, fit_sharp.logical_size);
    assert_eq!(fit.render_scale, 1.0);
    assert_eq!(fit_sharp.render_scale, 2.0);

    let mut cache = TextCoverageCache::new();
    let mut atlas = GlyphAtlas::new(&pipeline, 256).expect("atlas");

    let fit_run = rasterize_run(
        GlyphBackend::Gpu,
        fit.render_scale,
        &mut cache,
        &mut atlas,
        &pipeline,
        &mut state,
        &engine,
        &layout,
        &style,
        true,
        1.0,
    )
    .expect("gpu fit run");
    let RasterizedRun::Atlas(fit_glyphs) = fit_run else {
        panic!("the gpu backend must return atlas glyphs");
    };
    let fit_glyph = fit_glyphs[0].glyph;
    let fit_dst = fit_glyphs[0].destination().expect("ink");

    atlas.clear();
    let sharp_run = rasterize_run(
        GlyphBackend::Gpu,
        fit_sharp.render_scale,
        &mut cache,
        &mut atlas,
        &pipeline,
        &mut state,
        &engine,
        &layout,
        &style,
        true,
        1.0,
    )
    .expect("gpu fit-sharp run");
    let RasterizedRun::Atlas(sharp_glyphs) = sharp_run else {
        panic!("the gpu backend must return atlas glyphs");
    };
    let sharp_glyph = sharp_glyphs[0].glyph;
    let sharp_dst = sharp_glyphs[0].destination().expect("ink");

    // The physical bitmap grows with the render scale.
    assert!(
        sharp_glyph.region.width > fit_glyph.region.width
            && sharp_glyph.region.height > fit_glyph.region.height,
        "FitSharp must rasterize a larger bitmap: fit={}x{}, fit-sharp={}x{}",
        fit_glyph.region.width,
        fit_glyph.region.height,
        sharp_glyph.region.width,
        sharp_glyph.region.height,
    );
    assert_eq!(fit_glyph.raster_scale, 1.0);
    assert_eq!(sharp_glyph.raster_scale, 2.0);

    // The logical quad, bearings and advance stay at `size_px`.
    assert!(
        (fit_dst.x - sharp_dst.x).abs() < 1.0,
        "{fit_dst:?} {sharp_dst:?}"
    );
    assert!(
        (fit_dst.y - sharp_dst.y).abs() < 1.0,
        "{fit_dst:?} {sharp_dst:?}"
    );
    assert!(
        (fit_dst.width - sharp_dst.width).abs() < 1.0,
        "{fit_dst:?} {sharp_dst:?}"
    );
    assert!(
        (fit_dst.height - sharp_dst.height).abs() < 1.0,
        "{fit_dst:?} {sharp_dst:?}"
    );
    assert!(
        (fit_glyph.advance - sharp_glyph.advance).abs() < 1.0,
        "fit={}, fit-sharp={}",
        fit_glyph.advance,
        sharp_glyph.advance,
    );
    assert!((fit_glyph.bearing_x - sharp_glyph.bearing_x).abs() < 1.0);
    assert!((fit_glyph.bearing_y - sharp_glyph.bearing_y).abs() < 1.0);
}

/// A non-positive or non-finite rasterization scale is a typed error rather
/// than a silent fallback to the logical size.
#[test]
fn a_bad_render_scale_is_a_typed_error() {
    let Some(bytes) = load_test_font() else {
        return;
    };
    let mut engine = TextEngine::new();
    let Ok(font) = engine.add_font(&bytes, 0) else {
        return;
    };
    let (pipeline, _surface, mut state) = null_pipeline();
    let style = TextStyle::new(24.0);
    let layout = engine
        .layout(font, "A", &style, None)
        .expect("layout of a Latin glyph");
    let mut cache = TextCoverageCache::new();
    let mut atlas = GlyphAtlas::new(&pipeline, 256).expect("atlas");

    for scale in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        let error = rasterize_run(
            GlyphBackend::Gpu,
            scale,
            &mut cache,
            &mut atlas,
            &pipeline,
            &mut state,
            &engine,
            &layout,
            &style,
            true,
            1.0,
        )
        .expect_err("a bad scale must not fall back to the logical size");
        let debug = format!("{error:?}");
        assert!(
            matches!(error, TextError::InvalidRenderScale(_)),
            "scale {scale} produced {debug}"
        );
    }
}
