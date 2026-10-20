//! The generic effect draw a transition records (no device needed).
//!
//! The cross-fade and the scroll are the two kinds the generic shader path
//! draws. This file checks what the draw *says*: which sources it binds, what
//! weight it carries and where it places them. The pixels it produces are
//! checked against the CPU blend in `trans_effect_gpu.rs`, which needs a
//! device.
//!
//! The blend the draw reproduces is the reference's byte-space one
//! (`reference/cpp/core/visual/TransIntf.cpp:592-600,660`), and the scroll
//! placements are the reference's blit rectangles
//! (`reference/cpp/core/visual/TransIntf.cpp:1208-1217`).

use engine::shader::builtin;
use engine::shader::{EffectDraw, Program};
use engine::trans::{PHASE_MAX, ScrollFrom, ScrollStay, Trans, TransKind};
use engine::{BitmapState, Rect, Scene};

/// A cross-fade at `elapsed_ms` of its 100 ms duration.
fn cross_fade(elapsed_ms: f64) -> Trans {
    let mut trans = Trans::cross_fade(100.0).expect("crossfade");
    trans.seek_ms(elapsed_ms);
    trans
}

/// The bitmaps the draw's passes sample, in pass order.
fn sampled(draw: &EffectDraw) -> Vec<u32> {
    draw.bitmaps().collect()
}

/// The cross-fade binds the two sources and carries the phase as the blend
/// weight the program reads.
#[test]
fn the_cross_fade_binds_both_sources_and_carries_the_phase() {
    let trans = cross_fade(50.0);
    let phase = trans.phase();
    assert!(
        phase > 0 && phase < PHASE_MAX,
        "the phase is mid-transition"
    );
    let draw = trans
        .effect_draw(11, 22, (4, 4), true)
        .expect("cross-fade draw");

    assert_eq!(draw.passes.len(), 1, "one blend covers the frame");
    let pass = &draw.passes[0];
    assert_eq!(pass.program, Program::CrossFade);
    assert_eq!(pass.textures.len(), 2, "the blend reads two sources");
    assert_eq!(pass.textures[0].bitmap, 11);
    assert_eq!(pass.textures[1].bitmap, 22);
    assert_eq!(
        pass.dst,
        Rect::new(0, 0, 4, 4),
        "the blend covers the frame"
    );
    assert_eq!(draw.size, (4, 4));
    assert!(draw.covers_layer);

    // The weight is the phase over 256, the fraction the reference's blend
    // divides by (`reference/cpp/core/visual/TransIntf.cpp:592-600`).
    let words = pass.uniforms.words();
    assert_eq!(
        words[builtin::PARAM_UNIFORM as usize],
        phase as f32 / 256.0,
        "the phase reaches the program as the blend weight"
    );
}

/// The endpoints are exact copies of one source
/// (`reference/cpp/core/visual/TransIntf.cpp:660`), so the draw binds that
/// one source in both slots: the blend of a source with itself is that
/// source, whatever the weight is.
#[test]
fn the_endpoints_bind_both_slots_to_one_source() {
    let start = cross_fade(0.0);
    assert_eq!(start.phase(), 0);
    let draw = start.effect_draw(11, 22, (4, 4), true).expect("start draw");
    assert_eq!(
        sampled(&draw),
        vec![11, 11],
        "the start copies the first source"
    );

    let end = cross_fade(100.0);
    assert_eq!(end.phase(), end.phase_max());
    let draw = end.effect_draw(11, 22, (4, 4), true).expect("end draw");
    assert_eq!(
        sampled(&draw),
        vec![22, 22],
        "the end copies the second source"
    );
}

/// The scroll places the two sources at the rectangles the reference blits
/// them at (`reference/cpp/core/visual/TransIntf.cpp:1208-1217`).
#[test]
fn the_scroll_places_the_two_sources() {
    let mut trans = Trans::scroll(100.0, ScrollFrom::Left, ScrollStay::NoStay).expect("scroll");
    trans.seek_ms(50.0);
    let draw = trans
        .effect_draw(11, 22, (4, 4), true)
        .expect("scroll draw");
    assert_eq!(draw.passes.len(), 2, "the scroll places two sources");

    // The frame the hybrid path draws uses the same placements.
    let frame = trans
        .transition_frame(11, 22, (4, 4), true)
        .expect("scroll frame");
    assert_eq!(draw.passes[0].dst, frame.src1.dst);
    assert_eq!(draw.passes[1].dst, frame.src2.dst);
    assert_eq!(draw.passes[0].program, Program::Scroll);
    assert_eq!(draw.passes[1].program, Program::Scroll);
    assert_eq!(sampled(&draw), vec![11, 22]);
}

/// The kinds this build has no program for have no draw: they are blended on
/// the CPU, which is the reference's own arithmetic.
#[test]
fn a_kind_without_a_program_has_no_draw() {
    let trans = Trans::new(TransKind::Mosaic { max_size: 30 }, 100.0).expect("mosaic");
    assert!(
        trans.effect_draw(11, 22, (4, 4), true).is_none(),
        "a kind without a program is not drawn with a shader"
    );
}

/// A scene without a pipeline for the program's blend has no draw either: the
/// transition blends on the CPU. This is the headless backend's situation.
#[test]
fn a_scene_without_a_pipeline_blends_on_the_cpu() {
    let scene = Scene::default();
    let trans = cross_fade(50.0);
    assert!(
        scene
            .transition_effect_draw(&trans, 11, 22, (4, 4), true)
            .is_none(),
        "without a pipeline the transition blends on the CPU"
    );
}

/// The generic path writes no bitmap: the sources are read, the frame is
/// drawn by the renderer, and the transition's display bitmap is never
/// touched. That is what makes the path cheaper than the CPU blend, which
/// writes the whole frame every tick.
#[test]
fn the_generic_path_writes_no_bitmap() {
    let mut scene = Scene::default();
    let window = scene.add_window("t", (4, 4));
    let layer = scene.add_layer(window, None);
    let pixels = 4 * 4 * 4;
    let source = scene.add_bitmap(4, 4, vec![0u8; pixels]);
    {
        let state = scene.layer_mut(layer).expect("layer");
        state.rect = Rect::new(0, 0, 4, 4);
        state.visible = true;
        state.bitmap = Some(source);
    }

    let trans = cross_fade(50.0);
    let draw = trans
        .effect_draw(source, source, (4, 4), true)
        .expect("cross-fade draw");
    // The bitmap was written before the transition; the transition itself
    // writes nothing.
    scene.set_bitmap_clean(source);
    scene.set_effect(layer, Some(draw));

    assert!(
        scene.bitmaps.iter().all(|bitmap| !bitmap.dirty),
        "the effect draw dirties no bitmap"
    );
    assert!(
        scene
            .bitmaps
            .iter()
            .all(|bitmap| bitmap.content_generation == 0),
        "the effect draw writes no pixel"
    );

    // The CPU blend writes the frame it composites, which is what the
    // generic path avoids.
    let mut target = BitmapState::new(4, 4, vec![0u8; pixels]);
    let source_state = scene.bitmap(source).expect("source bitmap").clone();
    trans
        .apply(&mut target, &source_state, &source_state, (0, 0, 4, 4))
        .expect("cpu blend");
    assert!(target.dirty, "the CPU blend writes the frame it composites");
}
