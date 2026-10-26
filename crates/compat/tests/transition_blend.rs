//! Reference-behaviour tests for the cross-fade's blend.
//!
//! The blend a cross-fade runs is chosen by the destination layer's type
//! (`reference/cpp/core/visual/TransIntf.cpp:664-686`): a type that uses its
//! alpha channel gets the alpha-vs-alpha blend, the additive-alpha type gets
//! the blend that mixes all four channels, and every other type gets the
//! plain blend. The reference's graphics path mixes the plain and the
//! additive type with one script, `mix(s, d, opacity)`
//! (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3265-3268`), and the
//! alpha-vs-alpha type with the share of the destination the source covers
//! (`RenderManager_ogl.cpp:3278-3291`); the software path writes the same two
//! mixes (`reference/cpp/core/visual/tvpgl.cpp:4387-4404`, `:4446-4473`,
//! `:4475-4510`), and the alpha-vs-alpha one reads the byte-exact opacity
//! table (`reference/cpp/core/visual/tvpgl.cpp:224-251`,
//! `reference/cpp/core/visual/gl/blend_functor_c.h:689-707`).
//!
//! Every test asserts the pixels the transition wrote into the destination's
//! frame against the value the reference's arithmetic produces for the same
//! two sources and the same phase.

mod common;

use std::cell::RefCell;

use common::vm_lock;
use compat::CompatHost;
use engine::{FrameClock, Scene};
use tjs::Tjs2Engine;

/// Script label for diagnostics.
const SCRIPT: &str = "transition-blend-test";

/// The destination layer's fill: an opaque red (`0xAARRGGBB`).
const RED: i64 = 0xffff0000;
/// The transition source's fill: an opaque blue (`0xAARRGGBB`).
const BLUE: i64 = 0xff0000ff;
/// A source fill that carries half of its alpha.
const HALF_BLUE: i64 = 0x800000ff;

/// A bound VM + host + scene and the frame clock the transition's phase
/// follows.
///
/// The boxes are declared after `_host` so they outlive the host's raw
/// pointers (fields drop in declaration order).
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    _scene: Box<Scene>,
    clock: RefCell<Box<FrameClock>>,
}

impl Env {
    fn new() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut clock = Box::new(FrameClock::new());
        let mut host = CompatHost::new();
        // SAFETY: the boxed scene and clock outlive the host.
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_clock(clock.as_mut() as *mut FrameClock);
        }
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
            _scene: scene,
            clock: RefCell::new(clock),
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, SCRIPT)
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    /// The frame-body step: move the clock forward and advance every active
    /// transition to it.
    fn advance(&self, dt_ms: f64) {
        self.clock.borrow_mut().advance(dt_ms);
        let ctx = self._host.ctx().expect("bound host");
        compat::advance_transitions(&ctx).expect("advance transitions");
    }

    /// The `0xAARRGGBB` colour of the destination's transition frame at
    /// `(x, y)`.
    fn frame_pixel(&self, x: u32, y: u32) -> u32 {
        let layer = self
            ._scene
            .layers
            .iter()
            .rev()
            .find(|layer| layer.name == "dest")
            .expect("the destination layer");
        let frame = layer.transition_self.expect("the transition frame");
        let bitmap = self._scene.bitmap(frame).expect("the frame bitmap");
        let offset = bitmap.pixel_offset(x, y).expect("pixel offset");
        let px = &bitmap.rgba[offset..offset + 4];
        (u32::from(px[3]) << 24)
            | (u32::from(px[0]) << 16)
            | (u32::from(px[1]) << 8)
            | u32::from(px[2])
    }
}

/// The two fills as straight-alpha pixels.
fn sources(destination: i64, source: i64) -> ([u8; 4], [u8; 4]) {
    let to_rgba = |color: i64| {
        [
            (color >> 16) as u8,
            (color >> 8) as u8,
            color as u8,
            (color >> 24) as u8,
        ]
    };
    (to_rgba(destination), to_rgba(source))
}

/// The `0xAARRGGBB` colour of a blended pixel.
fn argb(pixel: [u8; 4]) -> u32 {
    (u32::from(pixel[3]) << 24)
        | (u32::from(pixel[0]) << 16)
        | (u32::from(pixel[1]) << 8)
        | u32::from(pixel[2])
}

/// One channel of the reference's const-alpha arithmetic:
/// `s1 + ((s2 - s1) * opa >> 8)`.
fn channel(s1: u8, s2: u8, opa: i32) -> u8 {
    let value = i32::from(s1) + (((i32::from(s2) - i32::from(s1)) * opa) >> 8);
    value.clamp(0, 255) as u8
}

/// The four-channel const-alpha blend: every channel is mixed with the
/// opacity as the ratio.
///
/// The reference's graphics path mixes the plain and the additive type with
/// one script, `mix(s, d, opacity)`
/// (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3265-3268`); the
/// software path writes the same for the additive type
/// (`reference/cpp/core/visual/tvpgl.cpp:4446-4473`).
fn blend_four_channel(s1: [u8; 4], s2: [u8; 4], opa: i32) -> [u8; 4] {
    [
        channel(s1[0], s2[0], opa),
        channel(s1[1], s2[1], opa),
        channel(s1[2], s2[2], opa),
        channel(s1[3], s2[3], opa),
    ]
}

/// The alpha-vs-alpha blend: the source covers the destination with the share
/// the two alphas give it, and the result's alpha is the two alphas mixed by
/// the opacity.
///
/// The reference's graphics path computes the share in floating point
/// (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3278-3291`) and the
/// software path reads it from the byte-exact opacity table
/// (`reference/cpp/core/visual/gl/blend_functor_c.h:689-707`,
/// `reference/cpp/core/visual/tvpgl.cpp:224-251`); the CPU path here is the
/// byte-exact one.
fn blend_straight(s1: [u8; 4], s2: [u8; 4], opa: i32) -> [u8; 4] {
    let opa_ = if opa > 127 { opa + 1 } else { opa };
    let iopa_ = 256 - opa_;
    let a1 = i32::from(s1[3]);
    let a2 = i32::from(s2[3]);
    let source = ((a2 * opa_) & 0xff00) as u32;
    let destination = ((a1 * iopa_) >> 8) as u32;
    let weight = i32::from(opacity_on_opacity(source >> 8, destination));
    [
        channel(s1[0], s2[0], weight),
        channel(s1[1], s2[1], weight),
        channel(s1[2], s2[2], weight),
        (a1 + (((a2 - a1) * opa_) >> 8)) as u8,
    ]
}

/// The share of a destination that a source covers: the table the
/// alpha-vs-alpha blend reads (`reference/cpp/core/visual/tvpgl.cpp:224-251`).
fn opacity_on_opacity(source: u32, destination: u32) -> u8 {
    if destination == 0 {
        return 255;
    }
    let at = (f64::from(destination) / 255.0) as f32;
    let bt = (f64::from(source) / 255.0) as f32;
    let mut share = bt / at;
    share = (f64::from(share) / (1.0 - f64::from(bt) + f64::from(share))) as f32;
    let scaled = (share * 255.0f32) as i32;
    if scaled >= 256 { 255 } else { scaled as u8 }
}

/// Run a cross-fade into a destination of the given type and return the
/// `0xAARRGGBB` colour the destination's frame holds.
///
/// The fill writes the colour's own alpha, and the type is set afterwards:
/// it is the type that decides the blend the transition runs.
///
/// 50 ms into a 100 ms cross-fade truncates to phase 127
/// (`reference/cpp/core/visual/TransIntf.cpp:536`).
fn blended(env: &Env, destination_type: &str, destination_fill: i64, source_fill: i64) -> u32 {
    env.run(&format!(
        "var w = new Window(); \
         var a = new Layer(w, null); a.setSize(2, 2); a.setImageSize(2, 2); a.name = 'src'; \
         a.fillRect(0, 0, 2, 2, 0x{source_fill:08x}); \
         var b = new Layer(w, null); b.setSize(2, 2); b.setImageSize(2, 2); b.visible = true; \
         b.name = 'dest'; b.fillRect(0, 0, 2, 2, 0x{destination_fill:08x}); \
         b.type = {destination_type}; \
         b.beginTransition('crossfade', false, a, %[time: 100]);"
    ));
    env.advance(50.0);
    env.frame_pixel(0, 0)
}

/// The plain blend: a destination that ignores its alpha mixes the four
/// channels alike.
///
/// The type is `ltOpaque`, which is not one of the types that use their
/// alpha channel (`reference/cpp/core/visual/drawable.h:53-65`), so the
/// blend is the plain one (`reference/cpp/core/visual/TransIntf.cpp:682-686`).
/// The reference's graphics path mixes it with the same script the additive
/// type uses (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3265-3268`).
#[test]
fn an_opaque_destination_mixes_the_four_channels_alike() {
    let _lock = vm_lock();
    let env = Env::new();
    let (red, blue) = sources(RED, BLUE);
    assert_eq!(
        blended(&env, "ltOpaque", RED, BLUE),
        argb(blend_four_channel(red, blue, 127)),
        "the plain blend mixes the four channels"
    );
    // The value the reference writes for the same sources:
    // `reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3265-3268`.
    assert_eq!(blend_four_channel(red, blue, 127), [0x80, 0x00, 0x7e, 0xff]);
}

/// The additive-alpha blend: a destination that stores an additive alpha
/// mixes all four channels.
///
/// The type is `ltAddAlpha`
/// (`reference/cpp/core/visual/drawable.h:63-65`), so the blend is the
/// additive one (`reference/cpp/core/visual/TransIntf.cpp:674-681`); the
/// reference's graphics path shares the plain blend's script with it
/// (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3274`).
#[test]
fn an_additive_destination_mixes_all_four_channels() {
    let _lock = vm_lock();
    let env = Env::new();
    let (red, half_blue) = sources(RED, HALF_BLUE);
    assert_eq!(
        blended(&env, "ltAddAlpha", RED, HALF_BLUE),
        argb(blend_four_channel(red, half_blue, 127)),
        "the additive blend mixes the alpha channel too"
    );
    // The value the reference writes for the same sources:
    // `reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3265-3268`.
    assert_eq!(
        blend_four_channel(red, half_blue, 127),
        [0x80, 0x00, 0x7e, 0xbf]
    );
}

/// The alpha-vs-alpha blend: a destination that uses its alpha channel mixes
/// by the share of the destination the source covers.
///
/// The type is `ltAlpha`, which uses its alpha channel
/// (`reference/cpp/core/visual/drawable.h:53-61`), so the blend is the
/// alpha-vs-alpha one (`reference/cpp/core/visual/TransIntf.cpp:664-673`,
/// `reference/cpp/core/visual/gl/blend_functor_c.h:689-707`).
#[test]
fn an_alpha_destination_mixes_by_the_source_coverage() {
    let _lock = vm_lock();
    let env = Env::new();
    let (red, blue) = sources(RED, BLUE);
    assert_eq!(
        blended(&env, "ltAlpha", RED, BLUE),
        argb(blend_straight(red, blue, 127)),
        "the alpha-vs-alpha blend mixes by the coverage"
    );
    // The value the reference's software path writes for the same sources:
    // `reference/cpp/core/visual/tvpgl.cpp:4475-4510`.
    assert_eq!(blend_straight(red, blue, 127), [0x57, 0x00, 0xa7, 0xff]);
}

/// The alpha-vs-alpha blend accounts for the alpha of both sources.
///
/// A source that carries half of its alpha is covered by the opaque source
/// in the share the two alphas give it, and the result's alpha is the two
/// alphas mixed by the phase
/// (`reference/cpp/core/visual/gl/blend_functor_c.h:689-707`).
#[test]
fn the_alpha_vs_alpha_blend_accounts_for_both_alphas() {
    let _lock = vm_lock();
    let env = Env::new();
    let (red, half_blue) = sources(RED, HALF_BLUE);
    assert_eq!(
        blended(&env, "ltAlpha", RED, HALF_BLUE),
        argb(blend_straight(red, half_blue, 127)),
        "a source that carries half of its alpha is covered in part"
    );
    // The value the reference's software path writes for the same sources:
    // `reference/cpp/core/visual/tvpgl.cpp:4475-4510`.
    assert_eq!(
        blend_straight(red, half_blue, 127),
        [0x9b, 0x00, 0x63, 0xbf]
    );
}
