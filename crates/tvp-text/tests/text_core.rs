//! Integration tests for the tvp-text rendering core.
//!
//! All tests are pure CPU rasterization — no display device, no Bevy.

use tvp_text::{Align, FontFace, GlyphAtlas, LayoutOptions, layout, measure_width};

/// The machine's Noto Sans CJK JP collection (see `/usr/share/fonts/noto-cjk`).
const JP_TTC: &str = "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc";

/// Load the system's Japanese CJK face; skips (passes vacuously) when the
/// font is not installed, so the suite stays portable.
fn jp_face() -> Option<FontFace> {
    FontFace::discover_system_jp().or_else(|| {
        std::fs::read(JP_TTC).ok().and_then(|bytes| {
            FontFace::find_jp_face_index(&bytes)
                .and_then(|index| FontFace::from_memory_indexed(bytes, index).ok())
        })
    })
}

#[test]
fn ttc_index_selection_and_loading() {
    // The ttc collection must load with an explicit face index, and the JP
    // face must be discoverable by family name (fontdb parses the name table
    // because ab_glyph does not expose family names).
    let bytes = match std::fs::read(JP_TTC) {
        Ok(bytes) => bytes,
        Err(_) => {
            eprintln!("skipping: {JP_TTC} not present");
            return;
        }
    };
    let index = FontFace::find_jp_face_index(&bytes).expect("JP face present in the ttc");
    assert!(
        (0..=7).contains(&index),
        "JP face should be found within indexes 0..=7, got {index}"
    );

    let face = FontFace::from_memory_indexed(bytes, index).expect("face loads from memory");
    assert!(
        face.family_name().unwrap_or("").contains("JP"),
        "family name must identify the JP face, got {:?}",
        face.family_name()
    );
    assert_eq!(face.collection_index(), index);

    // Same face via path.
    let face_via_path = FontFace::from_path(JP_TTC, index).expect("face loads from path");
    assert_eq!(face_via_path.family_name(), face.family_name());

    // Out-of-range collection index is an error, not a panic.
    assert!(FontFace::from_path(JP_TTC, 1000).is_err());

    // The selected face rasterizes Japanese text.
    let mut atlas = GlyphAtlas::with_default_width(face, 32);
    let slot = atlas.rasterize_char('日');
    assert!(slot.w > 0 && slot.h > 0, "日 must have ink: {slot:?}");
}

#[test]
fn jp_glyphs_rasterize_to_atlas() {
    let Some(face) = jp_face() else {
        eprintln!("skipping: no Noto Sans CJK JP on this machine");
        return;
    };
    assert!(
        face.family_name().unwrap_or("").contains("JP"),
        "discovered face must be the JP face, got {:?}",
        face.family_name()
    );

    let mut atlas = GlyphAtlas::with_default_width(face, 48);
    let (atlas_w, atlas_h) = atlas.atlas_size();
    assert_eq!(atlas_h, atlas.atlas_rgba().len() as u32 / 4 / atlas_w);

    for ch in "日本語テスト".chars() {
        let slot = atlas.rasterize_char(ch);
        assert!(
            slot.w > 0 && slot.h > 0,
            "{ch:?} must produce ink: {slot:?}"
        );
        assert!(slot.advance > 0.0, "{ch:?} must advance: {slot:?}");

        // The ink quad must lie inside the atlas and contain opaque pixels.
        assert!(slot.u + slot.w <= atlas_w && slot.v + slot.h <= atlas_h);
        let rgba = atlas.atlas_rgba();
        let mut ink = 0u32;
        for dy in 0..slot.h {
            for dx in 0..slot.w {
                let i = (((slot.v + dy) * atlas_w + (slot.u + dx)) * 4 + 3) as usize;
                if rgba[i] > 0 {
                    ink += 1;
                }
            }
        }
        assert!(ink > 0, "{ch:?} quad must contain opaque pixels");
    }

    // The atlas has non-zero pixels somewhere (and grows with new glyphs).
    assert!(
        atlas.atlas_rgba().iter().any(|&b| b != 0),
        "atlas RGBA must contain non-zero pixels"
    );
    assert_eq!(atlas.glyph_count(), "日本語テスト".chars().count());

    // Full-width CJK advance equals the pixel height (em-width boxes).
    let slot = atlas.rasterize_char('中');
    assert!(
        (slot.advance - 48.0).abs() < 0.01,
        "full-width advance must be pixel_height, got {}",
        slot.advance
    );
}

#[test]
fn atlas_grows_by_rows_without_invalidating_slots() {
    let Some(face) = jp_face() else {
        eprintln!("skipping: no Noto Sans CJK JP on this machine");
        return;
    };
    let mut atlas = GlyphAtlas::with_default_width(face, 48);

    // Rasterize enough distinct glyphs to overflow the first row of cells.
    let text = "あいうえおかきくけこ";
    let mut first = None;
    for ch in text.chars() {
        let slot = atlas.rasterize_char(ch);
        if first.is_none() {
            first = Some(slot);
        }
        assert!(slot.w > 0 && slot.h > 0, "{ch:?} must have ink");
    }
    let (w, h) = atlas.atlas_size();
    let rgba = atlas.atlas_rgba();
    assert_eq!(rgba.len(), w as usize * h as usize * 4);
    assert!(h > 48, "atlas must have grown beyond one cell row");

    // The first slot's quad must still hold ink after the growth.
    let slot = first.expect("first glyph rasterized");
    assert!(slot.u + slot.w <= w && slot.v + slot.h <= h);
    let mut ink = 0u32;
    for dy in 0..slot.h {
        for dx in 0..slot.w {
            let i = (((slot.v + dy) * w + (slot.u + dx)) * 4 + 3) as usize;
            if rgba[i] > 0 {
                ink += 1;
            }
        }
    }
    assert!(ink > 0, "first glyph must survive atlas growth");
}

#[test]
fn ascii_and_fullwidth_mix_lays_out() {
    let Some(face) = jp_face() else {
        eprintln!("skipping: no Noto Sans CJK JP on this machine");
        return;
    };
    let mut atlas = GlyphAtlas::with_default_width(face, 48);

    let text = "abc あいう";
    let result = layout(text, 100_000.0, 48.0, &mut atlas, &LayoutOptions::default());
    assert_eq!(result.runs.len(), 1, "one line, no wrap");

    let run = &result.runs[0];
    // Spaces are measured but not placed as glyphs.
    let placed: String = run.chars.iter().map(|g| g.ch).collect();
    assert_eq!(placed, "abcあいう");
    assert!(run.width > 0.0);

    // x positions are strictly increasing (monotonic width sums).
    for pair in run.chars.windows(2) {
        assert!(
            pair[1].x > pair[0].x,
            "x must increase: {} then {}",
            pair[0].x,
            pair[1].x
        );
    }

    // Layout width agrees with measure_width (both sum advances).
    let measured = measure_width(text, atlas.font(), 48.0);
    assert!(
        (run.width - measured as f32).abs() < 1.01,
        "layout width {} vs measure {}",
        run.width,
        measured
    );

    // Width sums are monotonic as the string grows.
    let mut prev = 0u32;
    for end in 1..=text.chars().count() {
        let prefix: String = text.chars().take(end).collect();
        let w = measure_width(&prefix, atlas.font(), 48.0);
        assert!(
            w >= prev,
            "width must not shrink for {prefix:?}: {w} < {prev}"
        );
        prev = w;
    }
}

#[test]
fn wrapping_explicit_breaks_and_cjk_midword() {
    let Some(face) = jp_face() else {
        eprintln!("skipping: no Noto Sans CJK JP on this machine");
        return;
    };
    let mut atlas = GlyphAtlas::with_default_width(face, 24);

    // Long CJK string, no spaces: wraps anywhere (mid-word by construction).
    let long: String = "日本語テスト".chars().cycle().take(60).collect();
    let max_width = 120.0; // 5 full-width glyphs per line at 24 px
    let result = layout(
        &long,
        max_width,
        24.0,
        &mut atlas,
        &LayoutOptions {
            wrap: true,
            ..Default::default()
        },
    );
    assert!(
        result.runs.len() >= 4,
        "long CJK text must wrap into multiple lines, got {}",
        result.runs.len()
    );
    for run in &result.runs {
        assert!(
            run.width <= max_width + 1.0,
            "run width {} must respect max_width",
            run.width
        );
        assert!(!run.chars.is_empty());
    }
    // Lines stack downward without overlap.
    for pair in result.runs.windows(2) {
        assert!(pair[1].y > pair[0].y);
    }
    assert!(result.total_height >= result.runs.len() as f32 * 24.0);

    // Explicit \n breaks are honored.
    let result = layout(
        "ab\ncd\nef",
        100_000.0,
        24.0,
        &mut atlas,
        &LayoutOptions::default(),
    );
    assert_eq!(result.runs.len(), 3);
    assert_eq!(result.runs[0].chars.len(), 2);
    assert!(result.runs[1].y > result.runs[0].y);
    assert!(result.total_height > result.runs[2].y);

    // Blank lines from "\n\n" keep their vertical slot.
    let result = layout(
        "ab\n\ndef",
        100_000.0,
        24.0,
        &mut atlas,
        &LayoutOptions::default(),
    );
    assert_eq!(result.runs.len(), 3);
    assert!(result.runs[1].chars.is_empty());
    assert_eq!(result.total_height, 3.0 * result.runs[0].line_height);

    // wrap disabled: one run even when it overflows max_width.
    let result = layout(&long, 50.0, 24.0, &mut atlas, &LayoutOptions::default());
    assert_eq!(result.runs.len(), 1);
    assert!(result.runs[0].width > 50.0);

    // CJK breaks mid-word: a wrapped line must be able to end mid-string.
    // (The "日本語テスト…" string has no spaces at all, so any wrap is
    // inherently mid-word; the run-count assertion above covers this.)
}

#[test]
fn alignment_offsets_are_exact() {
    let Some(face) = jp_face() else {
        eprintln!("skipping: no Noto Sans CJK JP on this machine");
        return;
    };
    let mut atlas = GlyphAtlas::with_default_width(face, 32);
    let max_width = 320.0;

    for align in [Align::Left, Align::Center, Align::Right] {
        let result = layout(
            "abcdef",
            max_width,
            32.0,
            &mut atlas,
            &LayoutOptions {
                align,
                ..Default::default()
            },
        );
        assert_eq!(result.runs.len(), 1);
        let run = &result.runs[0];
        let width = run.width;
        let first_x = run.chars[0].x;
        let expected = match align {
            Align::Left => 0.0,
            Align::Center => (max_width - width) / 2.0,
            Align::Right => max_width - width,
        };
        assert!(
            (first_x - expected).abs() < 0.01,
            "{align:?}: first glyph x {first_x}, expected {expected} (line width {width})"
        );
        // Every glyph shifts by the same offset.
        let second_x = run.chars[1].x;
        assert!((second_x - first_x - run.chars[0].advance).abs() < 0.01);
    }
}

#[test]
fn missing_glyph_falls_back_without_panicking() {
    let Some(face) = jp_face() else {
        eprintln!("skipping: no Noto Sans CJK JP on this machine");
        return;
    };
    let mut atlas = GlyphAtlas::with_default_width(face, 32);

    // U+E000 (private use) is not in Noto Sans CJK JP: must fall back to the
    // replacement glyph (.notdef box or U+FFFD), not panic.
    let slot = atlas.rasterize_char('\u{E000}');
    assert!(
        slot.advance > 0.0,
        "fallback glyph must still advance: {slot:?}"
    );
    assert_eq!(atlas.glyph_count(), 1);

    // The same fallback applies through the layout path.
    let result = layout(
        "a\u{E000}b",
        1000.0,
        32.0,
        &mut atlas,
        &LayoutOptions::default(),
    );
    assert_eq!(result.runs.len(), 1);
    assert_eq!(result.runs[0].chars.len(), 3, "all three chars placed");
    let fallback = result.runs[0].chars[1];
    assert_eq!(fallback.ch, '\u{E000}');

    // The atlas still contains ink from the fallback glyph.
    assert!(atlas.atlas_rgba().iter().any(|&b| b != 0));
}

#[test]
fn baseline_placement_matches_font_metrics() {
    let Some(face) = jp_face() else {
        eprintln!("skipping: no Noto Sans CJK JP on this machine");
        return;
    };
    let mut atlas = GlyphAtlas::with_default_width(face, 48);
    let ascent = atlas.ascent();
    let result = layout("Ag", 1000.0, 48.0, &mut atlas, &LayoutOptions::default());
    let run = &result.runs[0];
    let a = run.chars[0];
    let g = run.chars[1];
    let baseline = run.y + ascent;

    // The reference draws each glyph at `top = y + ascent - bitmap_top`
    // (`LayerBitmapImpl.cpp:917`), i.e. relative to the line's baseline. `A`
    // has no descender, so its ink bottom sits on the baseline; `g` descends
    // below it. Vertically centering the ink would move `A`'s bottom up by
    // roughly `(line_height - ink_height) / 2`.
    assert!(
        (a.y + a.size.1 as f32 - baseline).abs() <= 1.0,
        "A bottom {} vs baseline {baseline}",
        a.y + a.size.1 as f32
    );
    assert!(
        g.y + g.size.1 as f32 > baseline,
        "g must descend below the baseline: bottom {} vs {baseline}",
        g.y + g.size.1 as f32
    );
    assert!(
        a.y < baseline && g.y < baseline,
        "ink starts above baseline"
    );
}

#[test]
fn blank_and_control_characters_do_not_place_glyphs() {
    let Some(face) = jp_face() else {
        eprintln!("skipping: no Noto Sans CJK JP on this machine");
        return;
    };
    let mut atlas = GlyphAtlas::with_default_width(face, 24);
    // A carriage return (CRLF leftovers) is a control character and must be
    // skipped, not placed as a zero-size glyph.
    let result = layout("a\rb", 1000.0, 24.0, &mut atlas, &LayoutOptions::default());
    let placed: String = result.runs[0].chars.iter().map(|g| g.ch).collect();
    assert_eq!(placed, "ab");
    assert_eq!(atlas.glyph_count(), 2, "control chars are not rasterized");
}

#[test]
fn empty_and_whitespace_only_produce_no_runs() {
    let Some(face) = jp_face() else {
        eprintln!("skipping: no Noto Sans CJK JP on this machine");
        return;
    };
    let mut atlas = GlyphAtlas::with_default_width(face, 24);

    for text in ["", "   ", "\t\t", " \t  "] {
        let result = layout(text, 200.0, 24.0, &mut atlas, &LayoutOptions::default());
        assert!(result.runs.is_empty(), "{text:?} must produce zero runs");
        assert_eq!(result.total_height, 0.0, "{text:?} must have zero height");
    }

    // ... while an explicit blank line from "\n" is preserved.
    let result = layout("\n", 200.0, 24.0, &mut atlas, &LayoutOptions::default());
    assert_eq!(result.runs.len(), 1);
    assert!(result.runs[0].chars.is_empty());
}
