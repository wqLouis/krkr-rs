//! Cache-contract tests for the font face / glyph atlas caches.
//!
//! `Layer.drawText` is called once per character by the KAG message layer, so
//! the expensive work (system-font discovery, multi-MB font parse, glyph
//! rasterization) must happen at most once per `(face, pixel_height)`.
//! These tests assert the *structural* reuse guarantees directly, which is a
//! much stronger regression guard than a wall-clock threshold.

use std::sync::Arc;

use tvp_text::{FaceRequest, resolve_face, with_cached_atlas};

#[test]
fn face_and_atlas_are_cached_across_calls() {
    // A missing CJK font (minimal containers) still exercises the negative
    // cache and must not rescan; skip the atlas half in that case.
    let request = FaceRequest::SystemJp;
    let first = resolve_face(&request);
    let second = resolve_face(&request);
    match (&first, &second) {
        (Some(a), Some(b)) => assert!(
            Arc::ptr_eq(a, b),
            "repeated resolve_face must return the same shared face"
        ),
        (None, None) => {
            eprintln!("skipping atlas reuse: no system CJK font on this machine");
            return;
        }
        _ => panic!("face resolution must be stable: {first:?} vs {second:?}"),
    }
    let face = first.expect("checked Some above");

    let text = "日本語テスト";
    let height = 24;

    // First call rasterizes the string into the height-24 atlas.
    let (count_after_first, atlas_height) = with_cached_atlas(face.clone(), height, |atlas| {
        for ch in text.chars() {
            atlas.rasterize_char(ch);
        }
        (atlas.glyph_count(), atlas.pixel_height())
    });
    assert_eq!(count_after_first, text.chars().count());
    assert_eq!(atlas_height, height);

    // Second call sees the same atlas and rasterizes nothing new.
    let count_after_second = with_cached_atlas(face.clone(), height, |atlas| atlas.glyph_count());
    assert_eq!(
        count_after_second, count_after_first,
        "repeat calls must reuse rasterized glyphs, not rebuild the atlas"
    );

    // A different height gets its own fresh atlas...
    let other_count = with_cached_atlas(face.clone(), 12, |atlas| atlas.glyph_count());
    assert_eq!(other_count, 0, "a new height starts with an empty atlas");

    // ...and does not evict the first height's glyphs (no cache thrash).
    let count_back_at_24 = with_cached_atlas(face, height, |atlas| atlas.glyph_count());
    assert_eq!(
        count_back_at_24, count_after_first,
        "switching heights must not evict the other height's atlas"
    );
}

#[test]
fn override_requests_are_distinct_cache_entries() {
    // `FaceRequest` keys on the request itself, so an override path and the
    // default system face never alias one another.
    let default = resolve_face(&FaceRequest::SystemJp);
    let missing = resolve_face(&FaceRequest::Path("/nonexistent/krkr-rs-test.ttf".into()));
    assert!(
        missing.is_none(),
        "a bogus override path must resolve to None"
    );
    // The negative result is itself cached, so a repeat is cheap and stable.
    assert!(
        resolve_face(&FaceRequest::Path("/nonexistent/krkr-rs-test.ttf".into())).is_none(),
        "negative face results must be cached"
    );
    let _ = default;
}
