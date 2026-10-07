//! Integration tests for L2 text layout, the glyph atlas and GPU projection.
//!
//! Layout itself is `cosmic-text`; these tests assert the L2 seam: registered
//! faces shape to positioned glyphs, the atlas rasterizes/packs/caches via the
//! L1 API, projection emits textured quads and the CPU compositor reads the
//! same atlas pixels. A machine without a usable test font skips the
//! font-dependent cases rather than failing.

use raw_window_handle::{RawWindowHandle, XlibWindowHandle};

use engine::text::{
    GlyphAtlas, PositionedGlyph, TextEngine, TextError, TextStyle, compose_text_cpu, project_text,
};
use render::{
    PipelineConfig, RenderContent, RenderScene, begin_render_state, create_render_pipeline,
};

/// System fonts tried by the tests; a missing font skips rather than fails.
const TEST_FONT_PATHS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
];

/// Builds a headless raw window handle for the null backend.
fn headless_window() -> RawWindowHandle {
    RawWindowHandle::Xlib(XlibWindowHandle::new(0))
}

/// Reads the first available test font, if any.
fn load_test_font() -> Option<Vec<u8>> {
    TEST_FONT_PATHS
        .iter()
        .find_map(|path| std::fs::read(path).ok())
}

/// A pipeline/state pair for atlas tests.
fn null_pipeline() -> (render::RenderPipeline, render::Surface, render::RenderState) {
    let pipeline = create_render_pipeline(PipelineConfig::null()).expect("null pipeline");
    let (surface, state) = begin_render_state(&pipeline, &headless_window()).expect("null state");
    (pipeline, surface, state)
}

#[test]
fn layout_positions_glyphs_across_lines() {
    let Some(bytes) = load_test_font() else {
        return;
    };
    let mut engine = TextEngine::new();
    let font = engine.add_font(&bytes, 0).expect("register font");
    let layout = engine
        .layout(font, "Hello\nWorld", &TextStyle::new(24.0), None)
        .expect("layout");

    assert!(layout.len() >= 8, "unexpected glyph count {}", layout.len());
    assert!(layout.height > 0.0);
    assert!(layout.width > 0.0);
    let mut lines: Vec<usize> = layout.glyphs.iter().map(|glyph| glyph.line).collect();
    lines.dedup();
    assert_eq!(lines.len(), 2, "expected two visual lines");
}

#[test]
fn layout_rejects_invalid_size() {
    let Some(bytes) = load_test_font() else {
        return;
    };
    let mut engine = TextEngine::new();
    let font = engine.add_font(&bytes, 0).expect("register font");
    assert!(matches!(
        engine.layout(font, "x", &TextStyle::new(0.0), None),
        Err(TextError::InvalidSize(_))
    ));
}

#[test]
fn atlas_rasterizes_and_caches_glyphs() {
    let Some(bytes) = load_test_font() else {
        return;
    };
    let mut engine = TextEngine::new();
    let font = engine.add_font(&bytes, 0).expect("register font");
    let (pipeline, _surface, mut state) = null_pipeline();
    let mut atlas = GlyphAtlas::new(&pipeline, 128).expect("atlas");
    let style = TextStyle {
        size_px: 24.0,
        bold: false,
        italic: false,
        color: [255, 255, 255, 255],
    };

    let first = atlas
        .glyph(&pipeline, &mut state, &engine, font, 'A', &style)
        .expect("first glyph");
    let second = atlas
        .glyph(&pipeline, &mut state, &engine, font, 'A', &style)
        .expect("cached glyph");

    assert_eq!(first, second, "repeat request must reuse the atlas cell");
    assert_eq!(atlas.len(), 1);
    assert!(!first.is_empty());
    assert!(atlas.pixels().chunks_exact(4).any(|px| px[3] > 0));
}

#[test]
fn atlas_full_is_a_typed_error() {
    let Some(bytes) = load_test_font() else {
        return;
    };
    let mut engine = TextEngine::new();
    let font = engine.add_font(&bytes, 0).expect("register font");
    let (pipeline, _surface, mut state) = null_pipeline();
    let mut atlas = GlyphAtlas::new(&pipeline, 2).expect("tiny atlas");
    let style = TextStyle::new(48.0);

    let error = atlas
        .glyph(&pipeline, &mut state, &engine, font, 'A', &style)
        .expect_err("a 48px glyph cannot fit a 2x2 atlas");
    assert!(matches!(error, TextError::AtlasFull { .. }));
}

#[test]
fn project_text_emits_textured_quads() {
    let Some(bytes) = load_test_font() else {
        return;
    };
    let mut engine = TextEngine::new();
    let font = engine.add_font(&bytes, 0).expect("register font");
    let (pipeline, _surface, mut state) = null_pipeline();
    let mut atlas = GlyphAtlas::new(&pipeline, 128).expect("atlas");
    let style = TextStyle {
        size_px: 24.0,
        bold: false,
        italic: false,
        color: [255, 255, 255, 255],
    };
    let glyph = atlas
        .glyph(&pipeline, &mut state, &engine, font, 'A', &style)
        .expect("glyph");
    let positioned = [PositionedGlyph {
        glyph,
        x: 10.0,
        baseline_y: 30.0,
        opacity: 0.5,
    }];

    let mut scene = RenderScene::with_root_node();
    let root = scene.root;
    let ids = project_text(&mut scene, root, &atlas, &positioned, 7);
    assert_eq!(ids.len(), 1);
    let node = scene.node(ids[0]).expect("node");
    assert!(matches!(node.content, RenderContent::Texture(_)));
    assert_eq!(node.src, glyph.region);
    assert_eq!(node.opacity, 0.5);
    assert_eq!(node.z, 7);
    assert!(scene.node(root).expect("root").children.contains(&ids[0]));
}

#[test]
fn cpu_composition_matches_atlas_ink() {
    let Some(bytes) = load_test_font() else {
        return;
    };
    let mut engine = TextEngine::new();
    let font = engine.add_font(&bytes, 0).expect("register font");
    let (pipeline, _surface, mut state) = null_pipeline();
    let mut atlas = GlyphAtlas::new(&pipeline, 128).expect("atlas");
    let style = TextStyle {
        size_px: 24.0,
        bold: false,
        italic: false,
        color: [255, 0, 0, 255],
    };
    let glyph = atlas
        .glyph(&pipeline, &mut state, &engine, font, 'A', &style)
        .expect("glyph");
    let positioned = [PositionedGlyph {
        glyph,
        x: 4.0,
        baseline_y: 32.0,
        opacity: 1.0,
    }];

    let buffer = compose_text_cpu(&atlas, &positioned, 64, 64);
    assert_eq!(buffer.len(), 64 * 64 * 4);
    assert!(
        buffer.chunks_exact(4).any(|px| px[3] > 0),
        "the composited text must contain ink"
    );
    assert!(
        buffer.chunks_exact(4).any(|px| px[3] > 0 && px[0] > 0),
        "ink must carry the requested colour"
    );
}
