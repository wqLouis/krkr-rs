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

/// A `TextStyle` with a non-default colour (the struct is `#[non_exhaustive]`,
/// so tests build it through the constructor).
fn text_style(size_px: f32, color: [u8; 4]) -> TextStyle {
    let mut style = TextStyle::new(size_px);
    style.color = color;
    style
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
    let style = text_style(24.0, [255, 255, 255, 255]);

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
    let style = text_style(24.0, [255, 255, 255, 255]);
    let glyph = atlas
        .glyph(&pipeline, &mut state, &engine, font, 'A', &style)
        .expect("glyph");
    let positioned = [PositionedGlyph::new(
        glyph,
        10.0,
        30.0,
        0.5,
        [255, 255, 255, 255],
    )];

    let mut scene = RenderScene::with_root_node();
    let root = scene.root;
    let ids = project_text(&mut scene, root, &atlas, &positioned, 7);
    assert_eq!(ids.len(), 1);
    let node = scene.node(ids[0]).expect("node");
    assert!(matches!(node.content, RenderContent::Texture(_)));
    assert_eq!(node.src, glyph.region);
    assert_eq!(node.opacity, 0.5);
    assert_eq!(node.color, [1.0, 1.0, 1.0, 1.0]);
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
    let style = text_style(24.0, [255, 0, 0, 255]);
    let glyph = atlas
        .glyph(&pipeline, &mut state, &engine, font, 'A', &style)
        .expect("glyph");
    let positioned = [PositionedGlyph::new(
        glyph,
        4.0,
        32.0,
        1.0,
        [255, 0, 0, 255],
    )];

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

#[test]
fn colour_change_reuses_the_cached_glyph() {
    let Some(bytes) = load_test_font() else {
        return;
    };
    let mut engine = TextEngine::new();
    let font = engine.add_font(&bytes, 0).expect("register font");
    let (pipeline, _surface, mut state) = null_pipeline();
    let mut atlas = GlyphAtlas::new(&pipeline, 128).expect("atlas");
    let white = text_style(24.0, [255, 255, 255, 255]);
    let mut red = white;
    red.color = [255, 0, 0, 255];

    let first = atlas
        .glyph(&pipeline, &mut state, &engine, font, 'A', &white)
        .expect("white glyph");
    let coverage = atlas.pixels().to_vec();
    let second = atlas
        .glyph(&pipeline, &mut state, &engine, font, 'A', &red)
        .expect("red glyph");

    assert_eq!(first, second, "a colour change must reuse the atlas cell");
    assert_eq!(atlas.len(), 1, "colour must not create a new cache entry");
    assert_eq!(
        atlas.pixels(),
        coverage.as_slice(),
        "recolouring must not rewrite the coverage"
    );
    assert!(
        atlas
            .pixels()
            .chunks_exact(4)
            .all(|px| px[0] == px[1] && px[1] == px[2]),
        "the atlas must store colour-independent coverage"
    );
    assert!(
        atlas
            .pixels()
            .chunks_exact(4)
            .any(|px| px[3] > 0 && px[0] == 255),
        "covered texels must be white so the node tint supplies the colour"
    );
}

#[test]
fn tinted_cpu_output_matches_the_baked_reference() {
    let Some(bytes) = load_test_font() else {
        return;
    };
    let mut engine = TextEngine::new();
    let font = engine.add_font(&bytes, 0).expect("register font");
    let (pipeline, _surface, mut state) = null_pipeline();
    let mut atlas = GlyphAtlas::new(&pipeline, 128).expect("atlas");
    let color = [40, 120, 200, 200];
    let style = text_style(24.0, color);
    let glyph = atlas
        .glyph(&pipeline, &mut state, &engine, font, 'A', &style)
        .expect("glyph");
    let positioned = [PositionedGlyph::new(glyph, 4.0, 32.0, 1.0, color)];

    let (width, height) = (64u32, 64u32);
    let composed = compose_text_cpu(&atlas, &positioned, width, height);

    // Reproduce the previous colour-baked compositor: bake the tint into the
    // coverage and source-over it onto a transparent buffer. With full opacity
    // over an empty destination each covered pixel is exactly the tinted
    // coverage pixel `[r, g, b, coverage * a / 255]`.
    let mut expected = vec![0u8; (width * height * 4) as usize];
    let region = glyph.region;
    let atlas_pixels = atlas.pixels();
    let (atlas_w, _) = atlas.dimensions();
    let dest_x = (4.0 + glyph.bearing_x).round() as i64;
    let dest_y = (32.0 - glyph.bearing_y).round() as i64;
    for row in 0..region.height as i64 {
        for column in 0..region.width as i64 {
            let sx = region.x as i64 + column;
            let sy = region.y as i64 + row;
            let coverage = atlas_pixels[((sy as usize) * atlas_w as usize + sx as usize) * 4 + 3];
            let alpha = ((coverage as u16 * color[3] as u16) / 255) as u8;
            if alpha == 0 {
                // Source-over leaves a fully transparent source pixel alone.
                continue;
            }
            let dx = dest_x + column;
            let dy = dest_y + row;
            if dx < 0 || dy < 0 || dx >= width as i64 || dy >= height as i64 {
                continue;
            }
            let index = ((dy as usize) * width as usize + dx as usize) * 4;
            expected[index] = color[0];
            expected[index + 1] = color[1];
            expected[index + 2] = color[2];
            expected[index + 3] = alpha;
        }
    }

    assert_eq!(
        composed.len(),
        expected.len(),
        "tinted coverage must match the previous colour-baked output"
    );
    for (index, (got, want)) in composed.iter().zip(expected.iter()).enumerate() {
        assert_eq!(
            got,
            want,
            "tinted coverage differs at byte {index} (pixel {}, channel {})",
            index / 4,
            index % 4
        );
    }
}

#[test]
fn layout_cache_memoizes_shaping_and_invalidates_on_change() {
    let Some(bytes) = load_test_font() else {
        return;
    };
    let mut engine = TextEngine::new();
    let font = engine.add_font(&bytes, 0).expect("register font");
    let style = TextStyle::new(20.0);

    let first = engine.layout(font, "cached", &style, None).expect("layout");
    assert_eq!(engine.cached_layouts(), 1);
    let second = engine.layout(font, "cached", &style, None).expect("layout");
    assert_eq!(first, second);
    assert_eq!(
        engine.cached_layouts(),
        1,
        "unchanged label reuses its layout"
    );

    // Colour does not affect shaping, so it must not invalidate the cache.
    let mut recoloured = style;
    recoloured.color = [1, 2, 3, 4];
    engine
        .layout(font, "cached", &recoloured, None)
        .expect("layout");
    assert_eq!(engine.cached_layouts(), 1, "colour must not re-shape");

    // A real shaping input (size) invalidates and inserts a new entry.
    engine
        .layout(font, "cached", &TextStyle::new(22.0), None)
        .expect("layout");
    assert_eq!(engine.cached_layouts(), 2, "a size change re-shapes");
}
