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

use engine::text::{
    GlyphAtlas, MAX_TEXT_CACHE_ENTRIES, RasterizedRun, TextCoverageCache, TextEngine, TextStyle,
    rasterize_run,
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
    let (pipeline, _surface, mut state) = null_pipeline();
    let mut atlas = GlyphAtlas::new(&pipeline, 1024).expect("atlas");

    // Distinct (codepoint, size) pairs so every request is a cache miss: 94
    // printable ASCII codepoints across 14 sizes is more than the bound.
    for index in 0..=MAX_TEXT_CACHE_ENTRIES {
        let codepoint = char::from_u32(0x21 + u32::try_from(index % 94).expect("small index"))
            .expect("ASCII codepoint");
        let style = TextStyle::new(8.0 + (index / 94) as f32);
        atlas
            .glyph(&pipeline, &mut state, &engine, font, codepoint, &style)
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
