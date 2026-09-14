//! Ignored perf probes for the text hot paths (not part of the normal test
//! run). Measure with:
//!
//! ```text
//! cargo test -p tvp-text --test perf_probe -- --ignored --nocapture
//! ```

use std::sync::Arc;
use std::time::Instant;

use tvp_text::{FontFace, GlyphAtlas, LayoutOptions, layout};

fn jp_face() -> Option<Arc<FontFace>> {
    FontFace::discover_system_jp().map(Arc::new)
}

#[test]
#[ignore = "perf probe; run explicitly with --ignored --nocapture"]
fn perf_atlas_rasterize_many_glyphs() {
    let Some(face) = jp_face() else {
        eprintln!("skipping: no system CJK font");
        return;
    };
    // A fresh atlas forces every glyph through `rasterize_new`.
    let mut atlas = GlyphAtlas::with_default_width(face, 24);
    let t = Instant::now();
    let mut count = 0u32;
    for code in 0x4E00u32..0x4E00 + 4000 {
        if let Some(ch) = char::from_u32(code) {
            atlas.rasterize_char(ch);
            count += 1;
        }
    }
    let elapsed = t.elapsed();
    println!(
        "atlas: rasterized {count} glyphs in {elapsed:?} ({:.1} us/glyph)",
        elapsed.as_micros() as f64 / f64::from(count)
    );
}

#[test]
#[ignore = "perf probe; run explicitly with --ignored --nocapture"]
fn perf_rasterize_cached() {
    let Some(face) = jp_face() else {
        eprintln!("skipping: no system CJK font");
        return;
    };
    let mut atlas = GlyphAtlas::with_default_width(face, 24);
    let chars: Vec<char> = (0..128u32)
        .filter_map(|i| char::from_u32(0x3040 + i))
        .collect();
    for &ch in &chars {
        atlas.rasterize_char(ch);
    }
    const ITERS: u32 = 2_000_000;
    let t = Instant::now();
    let mut n = 0u64;
    for i in 0..ITERS {
        let ch = chars[(i as usize) % chars.len()];
        n = n.wrapping_add(u64::from(atlas.rasterize_char(ch).advance as u32));
    }
    let elapsed = t.elapsed();
    println!(
        "rasterize_char (cached): {ITERS} lookups in {elapsed:?} ({:.1} ns/lookup)",
        elapsed.as_nanos() as f64 / f64::from(ITERS)
    );
    std::hint::black_box(n);
}

#[test]
#[ignore = "perf probe; run explicitly with --ignored --nocapture"]
fn perf_layout_paragraph() {
    let Some(face) = jp_face() else {
        eprintln!("skipping: no system CJK font");
        return;
    };
    let mut atlas = GlyphAtlas::with_default_width(face, 24);
    // Warm every glyph so the timed loop measures layout, not rasterization.
    let text = "日本語のテキストレイアウトの性能を測定します。改行や折り返しも含みます。\
                これは二行目です。Three lines of mixed ASCII and 日本語 text.";
    for ch in text.chars() {
        atlas.rasterize_char(ch);
    }
    let opts = LayoutOptions {
        wrap: true,
        ..Default::default()
    };
    const ITERS: u32 = 20_000;
    let t = Instant::now();
    let mut glyphs = 0usize;
    for _ in 0..ITERS {
        let out = layout(text, 480.0, 24.0, &mut atlas, &opts);
        glyphs += out.runs.iter().map(|r| r.chars.len()).sum::<usize>();
    }
    let elapsed = t.elapsed();
    println!(
        "layout: {ITERS} iterations in {elapsed:?} ({:.2} us/iter, {glyphs} glyphs total)",
        elapsed.as_micros() as f64 / f64::from(ITERS)
    );
}

#[test]
#[ignore = "perf probe; run explicitly with --ignored --nocapture"]
fn perf_measure_width() {
    let Some(face) = jp_face() else {
        eprintln!("skipping: no system CJK font");
        return;
    };
    let text = "日本語のテキストレイアウトの性能を測定します。";
    const ITERS: u32 = 100_000;
    let t = Instant::now();
    let mut w = 0u32;
    for _ in 0..ITERS {
        w = w.wrapping_add(tvp_text::measure_width(text, &face, 24.0));
    }
    let elapsed = t.elapsed();
    println!(
        "measure_width: {ITERS} iterations in {elapsed:?} ({:.2} us/iter)",
        elapsed.as_micros() as f64 / f64::from(ITERS)
    );
    std::hint::black_box(w);
}
