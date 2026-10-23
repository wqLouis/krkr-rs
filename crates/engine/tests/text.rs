//! Integration tests for L2 text layout, the glyph atlas and GPU projection.
//!
//! Layout itself is `cosmic-text`; these tests assert the L2 seam: registered
//! faces shape to positioned glyphs, the atlas rasterizes/packs/caches via the
//! L1 API, projection emits textured quads and the CPU compositor reads the
//! same atlas pixels. A machine without a usable test font skips the
//! font-dependent cases rather than failing.

use raw_window_handle::{RawWindowHandle, XlibWindowHandle};

use engine::BitmapState;
use engine::text::{
    GlyphAtlas, PositionedGlyph, TextCoverageCache, TextEngine, TextError, TextStyle,
    compose_text_cpu, compose_text_into, project_text, rasterize_text_coverage,
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

/// Reference: the outline rasterizer puts the baseline of a line at the
/// face's ascent in whole pixels — the ascender scaled to the pixel size and
/// truncated (`reference/cpp/core/visual/FreeType.cpp:494`) — and a glyph's
/// ink box starts at `y + baseline - bitmap_top`
/// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:911-914`).
///
/// A baseline that is centred in the line box, or a fractional ascent, puts
/// every glyph a pixel off its row, which crops the bottom of the text.
#[test]
fn a_line_baseline_sits_at_the_face_ascent() {
    let Some(bytes) = load_test_font() else {
        return;
    };
    let mut engine = TextEngine::new();
    let font = engine.add_font(&bytes, 0).expect("register font");
    let style = TextStyle::new(32.0);
    let metrics = engine.font_metrics(font, 32.0).expect("font metrics");
    let baseline = metrics.ascent.floor();
    let layout = engine.layout(font, "A", &style, None).expect("layout");
    assert_eq!(
        layout.glyphs[0].baseline_y, baseline,
        "the line baseline must be the face's ascent in whole pixels"
    );

    let glyph = engine
        .rasterize_glyph(font, 'A', &style)
        .expect("rasterize the glyph");
    let first_covered_row = glyph
        .coverage
        .iter()
        .position(|coverage| *coverage > 0)
        .map(|index| index / glyph.width as usize)
        .expect("the glyph has ink");
    let mut canvas = BitmapState::new(64, 64, vec![0u8; 64 * 64 * 4]);
    let mut cache = TextCoverageCache::new();
    let masks = rasterize_text_coverage(&mut cache, &engine, &layout, &style, true)
        .expect("rasterize the coverage");
    compose_text_into(
        &masks,
        &style,
        (0.0, 0.0),
        0.0,
        &mut canvas,
        (0, 0, 64, 64),
        255,
    )
    .expect("compose the text");
    let ink_row = canvas
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .position(|pixel| pixel[3] != 0)
        .map(|index| index / 64)
        .expect("the composed text has ink");
    assert_eq!(
        ink_row as i32,
        baseline as i32 - glyph.bearing_y as i32 + first_covered_row as i32,
        "the ink must start at the baseline minus the glyph's bitmap top"
    );
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
    let (pipeline, _surface, _state) = null_pipeline();
    let mut atlas = GlyphAtlas::new(&pipeline, 128).expect("atlas");
    let style = text_style(24.0, [255, 255, 255, 255]);

    let first = atlas
        .glyph(&engine, font, 'A', &style)
        .expect("first glyph");
    let second = atlas
        .glyph(&engine, font, 'A', &style)
        .expect("cached glyph");

    assert_eq!(first, second, "repeat request must reuse the atlas cell");
    assert_eq!(atlas.len(), 1);
    assert!(!first.is_empty());
    assert!(atlas.pixels().as_chunks::<4>().0.iter().any(|px| px[3] > 0));
}

#[test]
fn atlas_full_is_a_typed_error() {
    let Some(bytes) = load_test_font() else {
        return;
    };
    let mut engine = TextEngine::new();
    let font = engine.add_font(&bytes, 0).expect("register font");
    let (pipeline, _surface, _state) = null_pipeline();
    let mut atlas = GlyphAtlas::new(&pipeline, 2).expect("tiny atlas");
    let style = TextStyle::new(48.0);

    let error = atlas
        .glyph(&engine, font, 'A', &style)
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
    let (pipeline, _surface, _state) = null_pipeline();
    let mut atlas = GlyphAtlas::new(&pipeline, 128).expect("atlas");
    let style = text_style(24.0, [255, 255, 255, 255]);
    let glyph = atlas.glyph(&engine, font, 'A', &style).expect("glyph");
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
    let (pipeline, _surface, _state) = null_pipeline();
    let mut atlas = GlyphAtlas::new(&pipeline, 128).expect("atlas");
    let style = text_style(24.0, [255, 0, 0, 255]);
    let glyph = atlas.glyph(&engine, font, 'A', &style).expect("glyph");
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
        buffer.as_chunks::<4>().0.iter().any(|px| px[3] > 0),
        "the composited text must contain ink"
    );
    assert!(
        buffer
            .as_chunks::<4>()
            .0
            .iter()
            .any(|px| px[3] > 0 && px[0] > 0),
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
    let (pipeline, _surface, _state) = null_pipeline();
    let mut atlas = GlyphAtlas::new(&pipeline, 128).expect("atlas");
    let white = text_style(24.0, [255, 255, 255, 255]);
    let mut red = white;
    red.color = [255, 0, 0, 255];

    let first = atlas
        .glyph(&engine, font, 'A', &white)
        .expect("white glyph");
    let coverage = atlas.pixels().to_vec();
    let second = atlas.glyph(&engine, font, 'A', &red).expect("red glyph");

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
            .as_chunks::<4>()
            .0
            .iter()
            .all(|px| px[0] == px[1] && px[1] == px[2]),
        "the atlas must store colour-independent coverage"
    );
    assert!(
        atlas
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
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
    let (pipeline, _surface, _state) = null_pipeline();
    let mut atlas = GlyphAtlas::new(&pipeline, 128).expect("atlas");
    let color = [40, 120, 200, 200];
    let style = text_style(24.0, color);
    let glyph = atlas.glyph(&engine, font, 'A', &style).expect("glyph");
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

/// Reference: `tTVPNativeBaseBitmap::GetTextSize` sums the rasterizer's
/// per-character extent, one UTF-16 unit at a time
/// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:1435-1473`). The
/// measured advance of a character must not depend on how often it has been
/// measured before, so a memoized advance has to be the rasterizer's own
/// value.
#[test]
fn a_repeated_advance_measures_the_same_value() {
    let Some(bytes) = load_test_font() else {
        return;
    };
    let mut engine = TextEngine::new();
    let font = engine.add_font(&bytes, 0).expect("register font");
    let style = TextStyle::new(24.0);

    let first = engine
        .glyph_advance(font, 'A', &style)
        .expect("measure the advance");
    let second = engine
        .glyph_advance(font, 'A', &style)
        .expect("measure the advance");
    assert_eq!(
        first.to_bits(),
        second.to_bits(),
        "a repeated measurement must return the same advance"
    );

    // A second engine measures the same face bytes from scratch, so the
    // memoized value must equal the freshly measured one.
    let mut other = TextEngine::new();
    let other_font = other.add_font(&bytes, 0).expect("register font");
    let fresh = other
        .glyph_advance(other_font, 'A', &style)
        .expect("measure the advance");
    assert_eq!(
        first.to_bits(),
        fresh.to_bits(),
        "the memoized advance must equal a fresh measurement"
    );
}
