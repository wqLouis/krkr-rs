//! Integration tests for the `motion_player` / `emote_player` plugin family
//! (`Motion`, `Player`, `EmotePlayer`, `D3DEmotePlayer`, `D3DEmoteModule`,
//! `ResourceManager`, `SeparateLayerAdaptor`, `D3DAdaptor`, `SourceCache`,
//! `ObjSource`, `Point`, `Circle`, `Rect`, `Quad`, `LayerGetter`).
//!
//! The reference splits the classes across `motionplayer.dll` and
//! `emoteplayer.dll`; both are registered into the same VM here, so the tests
//! drive the assembled surface. They attach a caller-owned L2 [`Scene`] and
//! exercise the script-visible behaviour: the namespace constants, the
//! geometry hit-test classes, the resource manager's layer-id allocator and
//! module cache, the player's property/animation/variable/timeline model and
//! the EmotePlayer shell. Every behaviour assertion cites the reference
//! `file:line` it pins.

mod common;

use common::vm_lock;
use compat::CompatHost;
use engine::{Scene, Storage};
use fs::FileSystem;
use tjs::{Tjs2Engine, TjsValue};

/// A bound VM + host + attached scene, optionally with a host-backed storage.
struct Env {
    // Drop order matters: the host removes the process-wide binding before
    // the boxed engine/scene/storage it points at are freed.
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    _scene: Box<Scene>,
    _storage: Option<Box<Storage>>,
}

impl Env {
    fn new() -> Env {
        Env::build(None)
    }

    /// A VM whose storage is the reference `emote` fixture directory.
    fn with_reference_storage() -> Env {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../reference/tests/test_files/emote");
        Env::build(Some(dir))
    }

    /// A VM whose storage holds a copy of the reference motion fixture at
    /// `motion/bare_motion.psb`, so a name without an extension exercises the
    /// candidate list.
    fn with_motion_alias() -> Env {
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../reference/tests/test_files/emote/e-mote3.0バニラパジャマa.psb");
        let dir = std::env::temp_dir().join("krkr-motion-player-alias");
        let nested = dir.join("motion");
        std::fs::create_dir_all(&nested).expect("create alias directory");
        std::fs::copy(source, nested.join("bare_motion.psb")).expect("copy motion fixture");
        Env::build(Some(dir))
    }

    fn build(storage_dir: Option<std::path::PathBuf>) -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut storage = storage_dir.map(|dir| {
            let fs = FileSystem::host(&dir);
            Box::new(
                Storage::mount_archives(fs, dir.clone(), std::iter::empty::<&str>())
                    .expect("mount storage"),
            )
        });
        let mut host = CompatHost::new();
        // SAFETY: the boxed scene and storage outlive the host binding (they
        // are declared after `_host` and dropped later).
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            if let Some(storage) = storage.as_mut() {
                host.services_mut()
                    .attach_storage(storage.as_mut() as *mut Storage);
            }
        }
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
            _scene: scene,
            _storage: storage,
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "motion-player-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "motion-player-test")
            .map_err(|error| error.to_string())
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.eval(expression) {
            Ok(TjsValue::Integer(value)) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    fn eval_real(&self, expression: &str) -> f64 {
        match self.eval(expression) {
            Ok(TjsValue::Real(value)) => value,
            Ok(TjsValue::Integer(value)) => value as f64,
            other => panic!("{expression:?} returned {other:?}, expected a real"),
        }
    }

    fn eval_bool(&self, expression: &str) -> bool {
        self.eval_int(expression) != 0
    }

    fn eval_str(&self, expression: &str) -> String {
        match self.eval(expression) {
            Ok(TjsValue::String(value)) => value,
            other => panic!("{expression:?} returned {other:?}, expected a string"),
        }
    }

    /// The pixels of the layer bitmap a `Player.draw` wrote, found in the
    /// attached scene.
    fn drawn_bitmap(&self) -> (u32, u32, Vec<u8>) {
        self.bitmap_of(800, 1080)
    }

    fn bitmap_of(&self, width: u32, height: u32) -> (u32, u32, Vec<u8>) {
        let bitmap = self
            ._scene
            .bitmaps
            .iter()
            .find(|bitmap| bitmap.width == width && bitmap.height == height)
            .expect("the draw target's bitmap");
        assert!(
            bitmap.rgba.chunks_exact(4).any(|pixel| pixel[3] != 0),
            "Player.draw must write pixels into the target layer"
        );
        (bitmap.width, bitmap.height, bitmap.rgba.clone())
    }

    /// The raw RGBA pixels of the layer bitmap with the given dimensions.
    fn pixels_of(&self, width: u32, height: u32) -> &[u8] {
        self._scene
            .bitmaps
            .iter()
            .find(|bitmap| bitmap.width == width && bitmap.height == height)
            .map(|bitmap| bitmap.rgba.as_slice())
            .expect("the target bitmap")
    }
}

#[test]
fn motion_namespace_registers_constants_and_subclasses() {
    let _lock = vm_lock();
    let env = Env::new();
    // The `Motion` namespace constants are registered as `Variant`s
    // (`reference/cpp/plugins/motionplayer/main.cpp:444-477`).
    assert_eq!(env.eval_int("Motion.LayerTypeObj"), 0);
    assert_eq!(env.eval_int("Motion.LayerTypeShape"), 1);
    assert_eq!(env.eval_int("Motion.LayerTypeCamera"), 5);
    assert_eq!(env.eval_int("Motion.ShapeTypeQuad"), 3);
    assert_eq!(env.eval_int("Motion.PlayFlagForce"), 1);
    assert_eq!(env.eval_int("Motion.PlayFlagStealth"), 16);
    assert_eq!(env.eval_int("Motion.TransformOrderSlant"), 3);
    assert_eq!(env.eval_int("Motion.CoordinateRecutangularXZ"), 1);
    assert_eq!(env.eval_int("Motion.MaskModeAlpha"), 1);
    // `getD3DAvailable` is a method on `Player`, not a static `Motion`
    // helper (`reference/cpp/plugins/motionplayer/main.cpp:288`).
    env.run("var p = new Motion.Player(null); p.getD3DAvailable();");

    // `Motion` registers the helper classes as subclasses
    // (`reference/cpp/plugins/motionplayer/main.cpp:430-441`); `Player` is a
    // top-level class (`main.cpp:128`) aliased into the namespace by the
    // post-registration callback (`main.cpp:522-524`).
    for name in [
        "Player",
        "EmotePlayer",
        "ResourceManager",
        "SeparateLayerAdaptor",
        "D3DAdaptor",
        "SourceCache",
        "ObjSource",
        "Point",
        "Circle",
        "Rect",
        "Quad",
        "LayerGetter",
    ] {
        assert_ne!(
            env.eval_str(&format!("typeof Motion.{name}")),
            "undefined",
            "Motion.{name} missing"
        );
    }
    assert_ne!(env.eval_str("typeof Player"), "undefined");
    // The post-registration callback installs an empty object as a static
    // `Player.useD3D` member, so the class-level probe reads `"Object"`
    // (`reference/cpp/plugins/motionplayer/main.cpp:498-509`).
    assert_eq!(env.eval_str("typeof Motion.Player.useD3D"), "Object");
    // `D3DEmoteModule` and `D3DEmotePlayer` are top-level classes of the
    // emoteplayer module, not `NCB_SUBCLASS` members of `Motion`
    // (`reference/cpp/plugins/motionplayer/main.cpp:565,588`; the nested-only
    // list is `main.cpp:430-441`).
    assert_eq!(env.eval_str("typeof Motion.D3DEmotePlayer"), "undefined");
    assert_eq!(env.eval_str("typeof Motion.D3DEmoteModule"), "undefined");
    assert_ne!(env.eval_str("typeof D3DEmoteModule"), "undefined");
    assert_ne!(env.eval_str("typeof D3DEmotePlayer"), "undefined");
}

#[test]
fn namespace_only_classes_are_not_top_level_globals() {
    let _lock = vm_lock();
    let env = Env::new();
    // `NCB_REGISTER_SUBCLASS_DELAY` only defines members: its `RegistEnd`
    // adds a dummy constructor and never publishes a global
    // (`reference/cpp/core/plugin/ncbind.hpp:1963-1970`). `NCB_SUBCLASS`
    // attaches the class under `Motion` as a `TJS_STATICMEMBER`
    // (`ncbind.hpp:1982-1987`, `reference/cpp/plugins/motionplayer/
    // main.cpp:427-441`). A missing member reads back as the string
    // "undefined" through the VM's `typeof`
    // (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:1899-1904`).
    for name in [
        "ResourceManager",
        "SeparateLayerAdaptor",
        "D3DAdaptor",
        "SourceCache",
        "ObjSource",
        "Point",
        "Circle",
        "MotionRect",
        "Quad",
        "LayerGetter",
        "EmotePlayer",
    ] {
        assert_eq!(
            env.eval_str(&format!("typeof global.{name}")),
            "undefined",
            "{name} must not be published as a top-level global"
        );
    }
    // Each class stays reachable through the namespace it was attached to
    // (`reference/cpp/plugins/motionplayer/main.cpp:427-441`); a class object
    // reports the VM's `typeof` spelling "Object", not JavaScript's "object"
    // (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2612-2614`).
    for name in [
        "Motion.ResourceManager",
        "Motion.SeparateLayerAdaptor",
        "Motion.D3DAdaptor",
        "Motion.SourceCache",
        "Motion.ObjSource",
        "Motion.Point",
        "Motion.Circle",
        "Motion.Rect",
        "Motion.Quad",
        "Motion.LayerGetter",
        "Motion.EmotePlayer",
    ] {
        assert_eq!(
            env.eval_str(&format!("typeof {name}")),
            "Object",
            "{name} must remain reachable"
        );
    }
    // `Player`, `D3DEmotePlayer` and `D3DEmoteModule` are the reference's
    // genuine top-level classes (`reference/cpp/plugins/motionplayer/
    // main.cpp:128,565,588`).
    for name in ["Player", "D3DEmotePlayer", "D3DEmoteModule"] {
        assert_eq!(
            env.eval_str(&format!("typeof {name}")),
            "Object",
            "{name} must stay top-level"
        );
    }
}

#[test]
fn geometry_hit_test_classes_work() {
    let _lock = vm_lock();
    let env = Env::new();
    // Each class exposes the reference `type` value
    // (`reference/cpp/plugins/motionplayer/SourceCache.h:129-138`, `:140-153`,
    // `:155-168`, `:170-180`).
    assert_eq!(env.eval_int("(new Motion.Point()).type"), 0);
    assert_eq!(env.eval_int("(new Motion.Circle()).type"), 1);
    assert_eq!(env.eval_int("(new Motion.Rect()).type"), 2);
    assert_eq!(env.eval_int("(new Motion.Quad()).type"), 3);
    // `Point::contains` and `Quad::contains` are unimplemented stubs that
    // always miss (`reference/cpp/plugins/motionplayer/SourceCache.h:136`,
    // `:177`).
    assert!(!env.eval_bool("(new Motion.Point()).contains(0, 0) ? true : false"));
    assert!(!env.eval_bool("(new Motion.Quad()).contains(0, 0) ? true : false"));
    // A short call fails the ncbind arity gate instead of defaulting
    // (`reference/cpp/core/plugin/ncbind.hpp:1178`).
    assert!(env.eval("(new Motion.Point()).contains(0)").is_err());
    // `Circle::contains` is a real radius test; the default radius is 0
    // (`reference/cpp/plugins/motionplayer/SourceCache.h:148-151`).
    assert!(env.eval_bool("(new Motion.Circle()).contains(0, 0) ? true : false"));
    assert!(!env.eval_bool("(new Motion.Circle()).contains(1, 0) ? true : false"));
    // `Quad::getP` is an unimplemented stub returning an unset variant, so
    // the property reads back as `void`
    // (`reference/cpp/plugins/motionplayer/SourceCache.h:176`).
    assert!(matches!(
        env.eval("(new Motion.Quad()).p"),
        Ok(TjsValue::Void)
    ));
}

#[test]
fn layer_getter_matches_reference_field_types_and_defaults() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var g = new Motion.LayerGetter();");
    // `getType` is an `int` returning 0
    // (`reference/cpp/plugins/motionplayer/SourceCache.h:212`, `:241`).
    assert!(matches!(env.eval("g.type"), Ok(TjsValue::Integer(0))));
    // `getOpacity` returns the raw `int` field, which defaults to 255
    // (`reference/cpp/plugins/motionplayer/SourceCache.h:231`, `:250`).
    assert!(matches!(env.eval("g.opacity"), Ok(TjsValue::Integer(255))));
    // The boolean visibility flags default true
    // (`reference/cpp/plugins/motionplayer/SourceCache.h:214`, `:243`).
    for name in ["visible", "branchVisible", "layerVisible"] {
        assert!(matches!(
            env.eval(&format!("g.{name}")),
            Ok(TjsValue::Integer(1))
        ));
    }
    // The zoom fields are `double`, defaulting to 1.0
    // (`reference/cpp/plugins/motionplayer/SourceCache.h:223`, `:246`).
    for name in ["zoomX", "zoomY"] {
        assert!((env.eval_real(&format!("g.{name}")) - 1.0).abs() < 1e-9);
    }
    // Position/origin fields are `double` and default to 0.0
    // (`reference/cpp/plugins/motionplayer/SourceCache.h:217-220`, `:244`,
    // `:249`).
    for name in ["x", "y", "left", "top", "originX", "originY"] {
        assert!(env.eval_real(&format!("g.{name}")).abs() < 1e-9);
    }
    // `getLabel` is a `ttstr`, defaulting to empty
    // (`reference/cpp/plugins/motionplayer/SourceCache.h:213`, `:242`).
    assert_eq!(env.eval_str("g.label"), "");
    // The variant-valued getters return an unset `tTJSVariant` (`void`)
    // (`reference/cpp/plugins/motionplayer/SourceCache.h:232-238`).
    for name in [
        "mtx",
        "vtx",
        "color",
        "bezierPatch",
        "shape",
        "motion",
        "particle",
    ] {
        assert!(
            matches!(env.eval(&format!("g.{name}")), Ok(TjsValue::Void)),
            "{name} must read back as void"
        );
    }
}

#[test]
fn player_draw_samples_the_icon_texture() {
    let _lock = vm_lock();
    let env = Env::with_reference_storage();
    let path = "e-mote3.0バニラパジャマa.psb";
    env.run("Motion.ResourceManager.setEmotePSBDecryptSeed(742877301);");
    // The default clip's object layers name `src/body_parts/...` icons, whose
    // pixel chunks live in `source/<group>/icon/<name>/pixel`.
    // `reference/cpp/plugins/motionplayer/PlayerInternal.h:1600-1835`.
    env.run(&format!(
        "var rm = new Motion.ResourceManager(null, 1024); \
         var w = new Window(); \
         var target = new Layer(w, null); \
         target.setSize(800, 1080); \
         var p = new Motion.Player(rm); \
         p.motionKey = \"{path}\"; \
         p.draw(new Motion.SeparateLayerAdaptor(target));"
    ));
    let (width, height, pixels) = env.drawn_bitmap();
    assert_eq!((width, height), (800, 1080));
    let texel = |x: usize, y: usize| {
        let i = (y * width as usize + x) * 4;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };
    // The sampled texel is fully opaque artwork: the draw blits the decoded
    // icon pixels, so an opaque texel keeps alpha 255
    // (`reference/cpp/plugins/motionplayer/PlayerInternal.h:1693-1745`).
    assert_eq!(
        texel(20, 300),
        [221, 255, 255, 255],
        "the drawn texel keeps the scene RGBA channels"
    );
    // A solid quad would paint every covered pixel with one value; the
    // texture carries thousands of distinct colours.
    let distinct: std::collections::HashSet<[u8; 4]> = pixels
        .chunks_exact(4)
        .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
        .collect();
    assert!(
        distinct.len() > 1000,
        "the draw must sample the texture, got {} distinct values",
        distinct.len()
    );
    // The drawn icons and shape placeholders cover only part of the layer, so
    // the untouched corner keeps the layer's transparent fill.
    assert_eq!(texel(799, 1079)[3], 0);
}

#[test]
fn player_draw_applies_the_draw_affine_translate() {
    let _lock = vm_lock();
    let env = Env::with_reference_storage();
    let path = "e-mote3.0バニラパジャマa.psb";
    env.run("Motion.ResourceManager.setEmotePSBDecryptSeed(742877301);");
    // The title maps its 1920x1080 canvas into the 1920x1440 target with
    // `setDrawAffineTranslateMatrix(1,0,0,1,960,540)`; the draw applies the
    // matrix to every node quad (`reference/cpp/plugins/motionplayer/
    // PlayerDrawDispatch.cpp:11-61`, `PlayerRenderItems.cpp:700-715`).
    env.run(&format!(
        "var rm = new Motion.ResourceManager(null, 1024); \
         var w = new Window(); \
         var plain = new Layer(w, null); \
         plain.setSize(1600, 1080); \
         var moved = new Layer(w, null); \
         moved.setSize(1600, 1081); \
         var p = new Motion.Player(rm); \
         p.motionKey = \"{path}\"; \
         p.draw(new Motion.SeparateLayerAdaptor(plain)); \
         p.setDrawAffineTranslateMatrix(1, 0, 0, 1, 800, 0); \
         p.draw(new Motion.SeparateLayerAdaptor(moved));"
    ));
    let alpha = |pixels: &[u8], x: usize, y: usize| pixels[(y * 1600 + x) * 4 + 3];
    let (_, _, plain) = env.bitmap_of(1600, 1080);
    let (_, _, moved) = env.bitmap_of(1600, 1081);
    // The body icon sits at x=20 without the affine and is moved by +800 px;
    // nothing else covers x=820 in the untranslated frame.
    assert_eq!(alpha(&plain, 820, 300), 0, "the untranslated spot is empty");
    assert_eq!(alpha(&moved, 820, 300), 255, "the affine moves the icon");
}

#[test]
fn player_draw_clears_the_target_before_compositing() {
    let _lock = vm_lock();
    let env = Env::with_reference_storage();
    let path = "e-mote3.0バニラパジャマa.psb";
    env.run("Motion.ResourceManager.setEmotePSBDecryptSeed(742877301);");
    // A fully red target with a player that paints no node: the reference
    // clears the render layer before compositing, so the old red pose must not
    // survive (`reference/cpp/plugins/motionplayer/PlayerRenderInternal.cpp:369-387`,
    // `PlayerRenderExecute.cpp:421-422`).
    env.run(&format!(
        "var rm = new Motion.ResourceManager(null, 1024); \
         var w = new Window(); \
         var target = new Layer(w, null); \
         target.setSize(800, 1080); \
         target.fillRect(0, 0, 800, 1080, 0xFFFF0000); \
         var p = new Motion.Player(rm); \
         p.motionKey = \"{path}\"; \
         p.setOpacity(0); \
         p.draw(new Motion.SeparateLayerAdaptor(target));"
    ));
    let pixels = env.pixels_of(800, 1080);
    for (x, y) in [(10usize, 10usize), (400, 540), (790, 1070)] {
        let offset = (y * 800 + x) * 4;
        assert_eq!(
            &pixels[offset..offset + 4],
            &[0, 0, 0, 0],
            "pixel ({x},{y}) keeps the prefilled red pose"
        );
    }
}

#[test]
fn player_draw_leaves_the_target_layer_geometry_and_visibility_alone() {
    let _lock = vm_lock();
    let env = Env::with_reference_storage();
    let path = "e-mote3.0バニラパジャマa.psb";
    env.run("Motion.ResourceManager.setEmotePSBDecryptSeed(742877301);");
    // The reference sizes and paints the target image; it does not move the
    // target layer (`reference/cpp/plugins/motionplayer/PlayerRenderTargets.cpp:1094-1135`,
    // `PlayerDrawDispatch.cpp:236-285`).
    env.run(&format!(
        "var rm = new Motion.ResourceManager(null, 1024); \
         var w = new Window(); \
         var base = new Layer(w, null); \
         var target = new Layer(w, null); \
         target.setSize(800, 1080); \
         target.left = 7; \
         target.top = 9; \
         target.visible = false; \
         target.opacity = 64; \
         var p = new Motion.Player(rm); \
         p.motionKey = \"{path}\"; \
         p.draw(new Motion.SeparateLayerAdaptor(target));"
    ));
    assert_eq!(env.eval_int("target.left"), 7);
    assert_eq!(env.eval_int("target.top"), 9);
    assert_eq!(env.eval_int("target.width"), 800);
    assert_eq!(env.eval_int("target.height"), 1080);
    assert_eq!(env.eval_int("target.opacity"), 64);
    assert_eq!(env.eval_int("target.visible ? 1 : 0"), 0);
}

#[test]
fn player_draw_reuses_the_target_allocation_and_marks_the_image_modified() {
    let _lock = vm_lock();
    let mut env = Env::with_reference_storage();
    let path = "e-mote3.0バニラパジャマa.psb";
    env.run("Motion.ResourceManager.setEmotePSBDecryptSeed(742877301);");
    // The reference composites into the target layer's image with
    // `AffineCopy` and keys the loaded source by its name and colour
    // (`reference/cpp/plugins/motionplayer/PlayerRenderExecute.cpp:456-472`,
    // `SourceCache.cpp:497-552`). A second draw of the same pose therefore
    // reuses both the target pixels and the decoded source chunks.
    env.run(&format!(
        "var rm = new Motion.ResourceManager(null, 1024); \
         var w = new Window(); \
         var target = new Layer(w, null); \
         target.setSize(800, 1080); \
         var p = new Motion.Player(rm); \
         p.motionKey = \"{path}\"; \
         p.draw(new Motion.SeparateLayerAdaptor(target));"
    ));
    let layer_id = env._scene.layers.last().expect("the target layer").id;
    let bitmap_id = env
        ._scene
        .layer(layer_id)
        .and_then(|layer| layer.bitmap)
        .expect("the target bitmap");
    // Writes through `GetMainImagePixelBufferForWrite` set `ImageModified`
    // (`reference/cpp/core/visual/LayerIntf.cpp:3012-3016`); the bitmap dirty
    // flag makes the renderer re-upload it.
    env._scene
        .layer_mut(layer_id)
        .expect("target")
        .image_modified = false;
    env._scene
        .bitmap_mut(bitmap_id)
        .expect("target bitmap")
        .dirty = false;
    let allocated = env
        ._scene
        .bitmap(bitmap_id)
        .expect("the target bitmap")
        .rgba
        .as_ptr() as usize;
    let read_bytes = env
        ._storage
        .as_ref()
        .expect("the fixture storage")
        .read_bytes();
    env.run("p.draw(new Motion.SeparateLayerAdaptor(target));");
    assert!(env._scene.layer(layer_id).expect("target").image_modified);
    assert!(env._scene.bitmap(bitmap_id).expect("target bitmap").dirty);
    assert_eq!(
        env._scene
            .bitmap(bitmap_id)
            .expect("the target bitmap")
            .rgba
            .as_ptr() as usize,
        allocated,
        "the draw keeps the target allocation"
    );
    assert_eq!(
        env._storage
            .as_ref()
            .expect("the fixture storage")
            .read_bytes(),
        read_bytes,
        "the second draw reads no pixel chunk again"
    );
}

#[test]
fn motion_load_registers_nested_psb_resources() {
    let _lock = vm_lock();
    let env = Env::with_reference_storage();
    let path = "e-mote3.0バニラパジャマa.psb";
    env.run("Motion.ResourceManager.setEmotePSBDecryptSeed(742877301);");
    // Loading a motion registers its container's resources in the `psb://`
    // media (`reference/cpp/plugins/motionplayer/RuntimeSupport.cpp:1425-1432`),
    // walking the whole value tree so a nested chunk is reachable by its
    // `/`-joined member path
    // (`reference/cpp/plugins/psbfile/PSBMediaRegistry.cpp:16-77`).
    env.run(&format!(
        "var rm = new Motion.ResourceManager(null, 1024); \
         var p = new Motion.Player(rm); \
         p.motionKey = \"{path}\";"
    ));
    let storage = env._storage.as_ref().expect("the fixture storage");
    // `source/#custom/icon/7/pixel` is resource chunk 57 of the fixture,
    // 35085 `RL`-compressed bytes.
    let bytes = storage
        .read("psb://e-mote3.0バニラパジャマa.psb/source/#custom/icon/7/pixel")
        .expect("the nested icon chunk resolves through psb://");
    assert_eq!(bytes.len(), 35085);
    // A second nested chunk, reached through the group/icon member path.
    let icon = storage
        .read("psb://e-mote3.0バニラパジャマa.psb/source/body_parts/icon/腕r00/pixel")
        .expect("the per-icon chunk resolves through psb://");
    assert_eq!(icon.len(), 270_140);
}

#[test]
fn separate_layer_adaptor_absolute_is_an_integer_and_survives_clear() {
    let _lock = vm_lock();
    let env = Env::new();
    // `absolute` is a `tjs_int` whose reference default is 0
    // (`reference/cpp/plugins/motionplayer/SeparateLayerAdaptor.h:97-98`,
    // `:147`).
    assert!(matches!(
        env.eval("(new Motion.SeparateLayerAdaptor()).absolute"),
        Ok(TjsValue::Integer(0))
    ));
    env.run(
        "var s = new Motion.SeparateLayerAdaptor(); \
         var target = %[]; \
         s.absolute = 2; \
         s.targetLayer = target;",
    );
    // The property round-trips an integer, not a boolean
    // (`reference/cpp/plugins/motionplayer/SeparateLayerAdaptor.h:97-98`).
    assert!(matches!(env.eval("s.absolute"), Ok(TjsValue::Integer(2))));
    // `clear()` releases the private render state only; `absolute` and
    // `targetLayer` are untouched
    // (`reference/cpp/plugins/motionplayer/SeparateLayerAdaptor.cpp:262-279`).
    env.run("s.clear();");
    assert_eq!(env.eval_int("s.absolute"), 2);
    assert_eq!(env.eval_int("s.targetLayer === target ? 1 : 0"), 1);
    // `assign` copies managed render lists only; a non-adaptor argument is
    // a plain no-op and never overwrites `targetLayer`
    // (`reference/cpp/plugins/motionplayer/SeparateLayerAdaptor.cpp:438-467`).
    env.run("s.assign(%[]);");
    assert_eq!(env.eval_int("s.targetLayer === target ? 1 : 0"), 1);
}

#[test]
fn player_draw_routes_a_separate_layer_adaptor_to_its_target_layer() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var target = new Layer(w, null); \
         var p = new Motion.Player(null);",
    );
    // The draw dispatch checks the `SeparateLayerAdaptor` native class before
    // the ordinary render path (`reference/cpp/plugins/motionplayer/
    // PlayerDrawDispatch.cpp:182-208`) and renders into the layer named by the
    // adaptor's live `targetLayer` (`reference/cpp/plugins/motionplayer/
    // PlayerRenderTargets.cpp:1228-1252`).
    assert!(
        env.eval("p.draw(new Motion.SeparateLayerAdaptor(target))")
            .is_ok(),
        "an SLA draw target must not raise"
    );
    // An SLA with an unset `targetLayer` has nothing to render into; the
    // reference's failed SLA route returns void without raising
    // (`reference/cpp/plugins/motionplayer/PlayerRenderTargets.cpp:1228-1252`).
    assert!(
        matches!(
            env.eval("p.draw(new Motion.SeparateLayerAdaptor())"),
            Ok(TjsValue::Void)
        ),
        "an SLA with no targetLayer must return void"
    );
    // The same holds when `targetLayer` is set to a non-`Layer` object.
    env.run("var odd = new Motion.SeparateLayerAdaptor(); odd.targetLayer = %[];");
    assert!(
        matches!(env.eval("p.draw(odd)"), Ok(TjsValue::Void)),
        "an SLA whose targetLayer is not a Layer must return void"
    );
}

#[test]
fn resource_manager_allocates_and_releases_layer_ids() {
    let _lock = vm_lock();
    let env = Env::new();
    // `requireLayerId` hands out increasing ids starting from 1
    // (`reference/cpp/plugins/motionplayer/ResourceManager.cpp:292-305`).
    env.run(
        "var rm = new Motion.ResourceManager(null, 1024); \
         var id1 = rm.requireLayerId(); \
         var id2 = rm.requireLayerId();",
    );
    let id1 = env.eval_int("id1");
    let id2 = env.eval_int("id2");
    assert_eq!(id1, 1, "the first id must be 1");
    assert!(id2 > id1, "layer ids should increase ({id1} then {id2})");
    // `clearCache` resets the allocator and the id maps
    // (`reference/cpp/plugins/motionplayer/ResourceManager.cpp:207-224`).
    env.run("rm.releaseLayerId(id1); rm.clearCache();");
    assert_eq!(env.eval_int("rm.requireLayerId()"), 1);
    // A short `releaseLayerId` call fails the ncbind arity gate
    // (`reference/cpp/plugins/motionplayer/ResourceManager.h:37`).
    assert!(env.eval("rm.releaseLayerId()").is_err());
    // The constructor has an ncbind arity of 2
    // (`reference/cpp/plugins/motionplayer/main.cpp:405`).
    assert!(env.eval("new Motion.ResourceManager(null)").is_err());
    // The static decrypt seed requires exactly one integer
    // (`reference/cpp/plugins/motionplayer/ResourceManager.cpp:112-121`).
    assert!(
        env.eval("Motion.ResourceManager.setEmotePSBDecryptSeed()")
            .is_err()
    );
    let error = env
        .eval("Motion.ResourceManager.setEmotePSBDecryptSeed(1.5)")
        .unwrap_err();
    assert!(
        error.contains("invalid parameter"),
        "unexpected error: {error}"
    );
    env.run("Motion.ResourceManager.setEmotePSBDecryptSeed(742877301);");
}

#[test]
fn derived_players_construct_through_their_class_name_member() {
    let _lock = vm_lock();
    let env = Env::new();
    // `EmotePlayer` and `D3DEmotePlayer` each declare their own
    // `NCB_CONSTRUCTOR` (`reference/cpp/plugins/motionplayer/main.cpp:299,588`),
    // which ncbind registers under the class name (`ncbind.hpp:1745`), so a
    // script subclass reaches the native instance through
    // `super.EmotePlayer()` / `super.D3DEmotePlayer()`.
    env.run(
        "class MyEmote extends Motion.EmotePlayer { \
             function MyEmote(rm) { super.EmotePlayer(rm); } \
         } \
         class MyD3D extends D3DEmotePlayer { \
             function MyD3D(rm) { super.D3DEmotePlayer(rm); } \
         } \
         var rm = new Motion.ResourceManager(null, 1024); \
         var e = new MyEmote(rm); \
         var d = new MyD3D(rm);",
    );
    assert_eq!(env.eval_str("typeof e"), "Object");
    assert_eq!(env.eval_str("typeof d"), "Object");
}

#[test]
fn d3d_emote_module_settings_are_class_statics() {
    let _lock = vm_lock();
    let env = Env::new();
    // All `D3DEmoteModule` backing fields are `inline static`, so a fresh
    // instance reads the class default; `maskMode` starts at 1
    // (`reference/cpp/plugins/motionplayer/D3DEmoteModule.h:43-48`).
    assert_eq!(env.eval_int("(new D3DEmoteModule()).maskMode"), 1);
    env.run(
        "var a = new D3DEmoteModule(); \
         var b = new D3DEmoteModule(); \
         a.maskMode = 3; \
         a.alphaOp = 2; \
         a.pixelateDivision = 4;",
    );
    // Because the fields are static, a second instance observes the same
    // value (`reference/cpp/plugins/motionplayer/D3DEmoteModule.h:24-25`,
    // `:31-32`, `:43-48`).
    assert_eq!(env.eval_int("b.maskMode"), 3);
    assert_eq!(env.eval_int("b.alphaOp"), 2);
    assert_eq!(env.eval_int("b.pixelateDivision"), 4);
    // `setMaxTextureSize(int w, int h)` is an ncbind arity-2 method
    // (`reference/cpp/plugins/motionplayer/D3DEmoteModule.h:37-40`,
    // `reference/cpp/plugins/motionplayer/main.cpp:586`).
    assert!(env.eval("a.setMaxTextureSize(2048)").is_err());
    env.run("a.setMaxTextureSize(2048, 1024);");
}

#[test]
fn d3d_emote_module_exposes_settings() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var m = new D3DEmoteModule(); \
         m.maskMode = 1; \
         m.maskRegionClipping = true; \
         m.mipMapEnabled = true; \
         m.alphaOp = 1; \
         m.protectTranslucentTextureColor = true; \
         m.pixelateDivision = 2; \
         m.setMaxTextureSize(2048, 1024);",
    );
    // Each property is registered on the reference class
    // (`reference/cpp/plugins/motionplayer/main.cpp:573-583`).
    assert_eq!(env.eval_int("m.maskMode"), 1);
    assert_eq!(env.eval_int("m.maskRegionClipping ? 1 : 0"), 1);
    assert_eq!(env.eval_int("m.pixelateDivision"), 2);
    // `D3DEmoteModule` has only the `setMaxTextureSize` method, no readable
    // property (`reference/cpp/plugins/motionplayer/D3DEmoteModule.h:37-40`,
    // registered with `NCB_METHOD` at `main.cpp:585`).
    assert_eq!(
        env.eval_int("typeof m.maxTextureSize == 'undefined'"),
        1,
        "D3DEmoteModule.maxTextureSize must not be registered"
    );
}

#[test]
fn player_round_trips_its_property_surface() {
    let _lock = vm_lock();
    let env = Env::new();
    // Every property is registered on the reference class
    // (`reference/cpp/plugins/motionplayer/main.cpp:129-197`).
    env.run(
        "var rm = new Motion.ResourceManager(null, 1024); \
         var p = new Motion.Player(rm); \
         p.completionType = 3; \
         p.chara = \"neko\"; \
         p.motionKey = \"idle\"; \
         p.outline = \"line\"; \
         p.priorDraw = 1.5; \
         p.frameLastTime = 10; \
         p.frameLoopTime = 4; \
         p.loopTime = 2; \
         p.queuing = true; \
         p.directEdit = true; \
         p.selectorEnabled = true; \
         p.speed = false; \
         p.frameTickCount = 12.5; \
         p.maskMode = 1; \
         p.colorWeight = 0x12345678; \
         p.independentLayerInherit = true; \
         p.zFactor = 2.5; \
         p.cameraFOV = 45; \
         p.canvasCaptureEnabled = true; \
         p.clearEnabled = true; \
         p.hitThreshold = 0.5; \
         p.preview = true; \
         p.outsideFactor = 0.25; \
         p.stealthChara = \"ghost\"; \
         p.stealthMotion = \"none\"; \
         p.useD3D = true; \
         p.meshline = \"m\";",
    );
    assert_eq!(env.eval_int("p.completionType"), 3);
    assert_eq!(env.eval_str("p.chara"), "neko");
    assert_eq!(env.eval_str("p.motionKey"), "idle");
    assert_eq!(env.eval_str("p.outline"), "line");
    assert!((env.eval_real("p.priorDraw") - 1.5).abs() < 1e-9);
    assert!((env.eval_real("p.frameLastTime") - 10.0).abs() < 1e-9);
    assert_eq!(env.eval_int("p.queuing ? 1 : 0"), 1);
    assert_eq!(env.eval_int("p.speed ? 1 : 0"), 0);
    assert_eq!(env.eval_int("p.maskMode"), 1);
    assert_eq!(env.eval_int("p.useD3D ? 1 : 0"), 1);
    assert!((env.eval_real("p.zFactor") - 2.5).abs() < 1e-9);
    assert!((env.eval_real("p.hitThreshold") - 0.5).abs() < 1e-9);
}

#[test]
fn player_variable_surface_round_trips() {
    let _lock = vm_lock();
    let env = Env::new();
    // `setVariable(ttstr, double)` is a raw callback; `getVariable` is an
    // ncbind arity-1 real getter
    // (`reference/cpp/plugins/motionplayer/main.cpp:253-255`,
    // `Player.h:344`).
    env.run("var p = new Motion.Player(null); p.setVariable(\"face_mouth\", 3.5);");
    assert!((env.eval_real("p.getVariable(\"face_mouth\")") - 3.5).abs() < 1e-9);
    // `countVariables` counts the snapshot's declared variables
    // (`reference/cpp/plugins/motionplayer/PlayerVariable.cpp:568-574`).
    assert!(env.eval_int("p.countVariables()") >= 0);
    // `getVariable` requires its label
    // (`reference/cpp/plugins/motionplayer/Player.h:344`).
    assert!(env.eval("p.getVariable()").is_err());
}

#[test]
fn player_timeline_surface_is_queryable() {
    let _lock = vm_lock();
    let env = Env::new();
    // `playTimeline(ttstr, tjs_int)` requires two arguments
    // (`reference/cpp/plugins/motionplayer/Player.h:364`).
    env.run(
        "var p = new Motion.Player(null); \
         p.playTimeline(\"idle\", 1); \
         var info = p.getPlayingTimelineInfoList();",
    );
    assert!(env.eval("p.playTimeline(\"idle\")").is_err());
    // `getPlayingTimelineInfoList` returns an array (the reference returns
    // an empty array when no motion is active)
    // (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:337-339`).
    assert_ne!(env.eval_str("typeof info"), "undefined");
    // `getTimelinePlaying` reports the state of the named timeline
    // (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:64-71`).
    assert_eq!(env.eval_int("p.getTimelinePlaying(\"idle\") ? 1 : 0"), 0);
    // `stopTimeline` requires its label
    // (`reference/cpp/plugins/motionplayer/Player.h:365`).
    env.run("p.stopTimeline(\"idle\");");
    assert!(env.eval("p.stopTimeline()").is_err());
}

#[test]
fn player_methods_reject_missing_arguments() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var p = new Motion.Player(null);");
    // Each call is one argument short of the ncbind arity registered by the
    // reference, so it must fail with a typed error rather than defaulting
    // (`reference/cpp/core/plugin/ncbind.hpp:1178`):
    // `setSize(w, h)` (`Player.h:319`), `draw(tTJSVariant)`
    // (`Player.h:322-323`, `main.cpp:234`), `frameProgress(double)`
    // (`Player.h:324`), `setMirror(bool)` (`Player.h:266`),
    // `setRotate(double, double, double)` (`Player.h:263`),
    // `fadeInTimeline(ttstr, double, tjs_int)` (`Player.h:368`),
    // `onFindMotion(ttstr, tjs_int)` (`Player.h:380`),
    // `initPhysics(tTJSVariant)` (`Player.h:256`),
    // `setStereovisionCameraPosition(double, double, double)` (`Player.h:339`).
    for call in [
        "p.setSize(1)",
        "p.draw()",
        "p.frameProgress()",
        "p.setMirror()",
        "p.setRotate(1, 0)",
        "p.fadeInTimeline(\"idle\", 1)",
        "p.onFindMotion(\"idle\")",
        "p.initPhysics()",
        "p.setStereovisionCameraPosition(1, 2)",
        "p.setDrawAffineTranslateMatrix(1, 2, 3)",
    ] {
        assert!(env.eval(call).is_err(), "{call} must fail");
    }
    // A scalar `draw` argument is a no-op, and so is an object that names no
    // layer — the reference's ordinary path returns without raising when the
    // target does not resolve to a `Layer`
    // (`reference/cpp/plugins/motionplayer/PlayerRenderTargets.cpp:1081-1090`).
    env.run("p.draw(1);");
    assert!(env.eval("p.draw(%[])").is_ok());
}

#[test]
fn player_on_find_motion_override_wins() {
    let _lock = vm_lock();
    let env = Env::new();
    // `setMotionCompat` builds `{chara, motion}`, calls
    // `objthis.onFindMotion(dict)` through the object (so a script override
    // runs) and reads the returned object back
    // (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:445-474`).
    env.run(
        "var p = new Motion.Player(null); \
         var seen = \"\"; \
         p.onFindMotion = function(d) { \
             seen = d.chara + \"/\" + d.motion; \
             return %[chara: \"overridden\", motion: \"M2\"]; \
         }; \
         p.motion = \"M1\";",
    );
    assert_eq!(env.eval_str("seen"), "/M1");
    // The override's returned object rewrites both identity fields
    // (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:458-472`).
    assert_eq!(env.eval_str("p.motionKey"), "M2");
    assert_eq!(env.eval_str("p.chara"), "overridden");
}

#[test]
fn emote_player_shell_transforms_animate() {
    let _lock = vm_lock();
    let env = Env::new();
    // `setCoord`/`setScale`/`setRot`/`setColor` are the EmotePlayer raw
    // callbacks (`reference/cpp/plugins/motionplayer/main.cpp:343-350`).
    env.run(
        "var rm = new Motion.ResourceManager(null, 1024); \
         var e = new Motion.EmotePlayer(rm); \
         e.setCoord(10, 20, 0); \
         e.setScale(2, 0); \
         e.setRot(0.5, 0); \
         e.setColor(0xFF00FF, 0);",
    );
    // The binary getters are hardcoded (`getRot` 0.0, `getScale` 1.0,
    // `getColor` 0) (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:325-326`,
    // `:387-388`, `:426-427`).
    assert_eq!(env.eval_real("e.getScale()"), 1.0);
    assert_eq!(env.eval_real("e.getRot()"), 0.0);
    assert_eq!(env.eval_int("e.getColor()"), 0);
    // The shell member set includes the parameterless `pass`/`skip`
    // (`reference/cpp/plugins/motionplayer/main.cpp:385-388`).
    env.run("e.show(); e.hide(); e.progress(16); e.pass(); e.skip();");
}

#[test]
fn resource_manager_loads_and_unloads_a_psb_module() {
    let _lock = vm_lock();
    let env = Env::with_reference_storage();
    let path = "e-mote3.0バニラパジャマa.psb";
    env.run("Motion.ResourceManager.setEmotePSBDecryptSeed(742877301);");
    // `load(ttstr)` parses the PSB and returns the module object
    // (`reference/cpp/plugins/motionplayer/ResourceManager.cpp:143-170`).
    env.run(&format!(
        "var rm = new Motion.ResourceManager(null, 1024); \
         var m = rm.load(\"{path}\");"
    ));
    // A parsed module is an object; the VM's `typeof` spells that "Object"
    // (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2612-2614`).
    assert_eq!(env.eval_str("typeof m"), "Object");
    assert_eq!(env.eval_str("m.path"), path);
    // The module bridges the parsed metadata as a TJS dictionary
    // (`reference/cpp/plugins/motionplayer/ResourceManager.cpp:143-170`).
    assert_eq!(env.eval_str("m.metadata.base.chara"), "all_parts");
    // The logical canvas size comes from the root's `screenSize` sibling
    // (`reference/cpp/plugins/motionplayer/docs/MOTIONPLAYER_PSB_STRUCT.md`
    // §2); the fixture's `screenSize` is 800x1080.
    assert!((env.eval_real("m.width") - 800.0).abs() < 1e-9);
    assert!((env.eval_real("m.height") - 1080.0).abs() < 1e-9);
    // `findSource` returns the cached module object itself
    // (`reference/cpp/plugins/motionplayer/ResourceManager.cpp:263-265`).
    assert!(env.eval_bool(&format!("rm.findSource(\"{path}\") === m ? true : false")));
    // A Player bound to the module loads it lazily on the first query
    // (`reference/cpp/plugins/motionplayer/PlayerVariable.cpp:568-574`,
    // `PlayerTimeline.cpp:75-76`).
    env.run("var p = new Motion.Player(rm); p.project = m;");
    // The reference fixture's `metadata.variableList` declares 24 variables,
    // the first of which is `move_UD`
    // (`reference/tests/test_files/emote/e-mote3.0バニラパジャマa.json`); the
    // list is a direct child of `metadata`, not of `metadata.base`
    // (`reference/cpp/plugins/motionplayer/docs/MOTIONPLAYER_PSB_STRUCT.md`
    // §4.1).
    assert!(env.eval_int("p.countVariables()") >= 24);
    assert_eq!(env.eval_str("p.getVariableLabelAt(0)"), "move_UD");
    // `metadata.timelineControl` declares 18 timelines in the same fixture,
    // so the clip/timeline model is populated too.
    assert!(env.eval_int("p.countMainTimelines()") > 0);
    assert!(!env.eval_str("p.getMainTimelineLabelAt(0)").is_empty());
    // `unload(ttstr)` drops every cache key that points at the module
    // (`reference/cpp/plugins/motionplayer/ResourceManager.cpp:172-205`).
    env.run(&format!("rm.unload(\"{path}\");"));
    // `findSource` on a miss returns an unset variant, which the VM's
    // `typeof` spells "void" (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2608-2610`).
    assert_eq!(
        env.eval_str(&format!("typeof rm.findSource(\"{path}\")")),
        "void"
    );
}

#[test]
fn player_variable_keys_is_an_empty_array_before_a_motion_is_bound() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var p = new Motion.Player(null);");
    // `Player::getVariableKeys` returns an empty array whenever `_variableKeys`
    // is still unset, so the property is always an object
    // (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:1248-1254`).
    assert_eq!(env.eval_str("typeof p.variableKeys"), "Object");
    assert_eq!(env.eval_int("p.variableKeys.count"), 0);
    // A void value raises when a caller reads a member from it
    // (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:1421`), which is the
    // failure this fallback prevents; a fresh player must stay readable.
    assert!(env.eval("p.variableKeys.count").is_ok());
}

#[test]
fn player_without_an_argument_installs_a_default_resource_manager() {
    let _lock = vm_lock();
    let env = Env::new();
    // The constructor installs the TJS manager object it returns, whether the
    // caller passes one or not, so a fresh player never exposes void
    // (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:175-185`,
    // `Player.h:228-229`).
    env.run("var p = new Motion.Player(null);");
    assert_eq!(env.eval_str("typeof p.resourceManager"), "Object");
    // The same object is returned on every read.
    assert!(env.eval_bool("p.resourceManager === p.resourceManager"));
}

#[test]
fn player_project_binding_syncs_variable_keys_from_the_motion_labels() {
    let _lock = vm_lock();
    let env = Env::with_reference_storage();
    let path = "e-mote3.0バニラパジャマa.psb";
    env.run("Motion.ResourceManager.setEmotePSBDecryptSeed(742877301);");
    env.run(&format!(
        "var rm = new Motion.ResourceManager(null, 1024); \
         var m = rm.load(\"{path}\"); \
         var p = new Motion.Player(rm); \
         p.project = m;"
    ));
    // `ensureMotionLoaded` activates the object bound to `project` and
    // `syncVariableKeysFromActiveMotion` copies the motion's
    // `variableLabels` into `_variableKeys`
    // (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:503-550`,
    // `:1068-1076`).
    assert_eq!(env.eval_str("typeof p.variableKeys"), "Object");
    assert!(env.eval_int("p.variableKeys.count") >= 24);
    assert_eq!(env.eval_str("p.variableKeys[0]"), "move_UD");
}

#[test]
fn player_variable_keys_getter_does_not_replace_a_void_value_with_the_motion_labels() {
    let _lock = vm_lock();
    let env = Env::with_reference_storage();
    let path = "e-mote3.0バニラパジャマa.psb";
    env.run("Motion.ResourceManager.setEmotePSBDecryptSeed(742877301);");
    env.run(&format!(
        "var rm = new Motion.ResourceManager(null, 1024); \
         var p = new Motion.Player(rm); \
         p.motionKey = \"{path}\"; \
         p.variableKeys = void; \
         var first = p.variableKeys.count; \
         var second = p.variableKeys.count;"
    ));
    // The getter returns a fresh empty array while the stored `_variableKeys`
    // stays void, and it never replaces that value with the active motion's
    // labels (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:1248-1254`).
    assert_eq!(env.eval_str("typeof p.variableKeys"), "Object");
    assert_eq!(env.eval_int("first"), 0);
    assert_eq!(env.eval_int("second"), 0);
}

#[test]
fn player_motion_key_resolves_a_storage_like_name_and_syncs_variable_keys() {
    let _lock = vm_lock();
    let env = Env::with_reference_storage();
    let path = "e-mote3.0バニラパジャマa.psb";
    env.run("Motion.ResourceManager.setEmotePSBDecryptSeed(742877301);");
    // A name carrying a path separator or an extension is looked up as it is
    // (`reference/cpp/plugins/motionplayer/RuntimeSupport.cpp:1319-1349`),
    // and binding it resets the runtime before the load
    // (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:471-490`).
    env.run(&format!(
        "var rm = new Motion.ResourceManager(null, 1024); \
         var p = new Motion.Player(rm); \
         p.motionKey = \"{path}\";"
    ));
    assert_eq!(env.eval_str("p.motionKey"), path);
    assert!(env.eval_int("p.variableKeys.count") >= 24);
    assert_eq!(env.eval_str("p.variableKeys[0]"), "move_UD");
}

#[test]
fn player_motion_key_expands_a_bare_name_through_the_candidate_list() {
    let _lock = vm_lock();
    let env = Env::with_motion_alias();
    env.run("Motion.ResourceManager.setEmotePSBDecryptSeed(742877301);");
    // A bare name is looked up as `<name>.mtn`, `<name>.psb`,
    // `motion/<name>.mtn` and `motion/<name>.psb` in that order
    // (`reference/cpp/plugins/motionplayer/RuntimeSupport.cpp:1319-1349`).
    env.run(
        "var p = new Motion.Player(null); \
         p.motionKey = \"bare_motion\";",
    );
    assert_eq!(env.eval_str("p.motionKey"), "bare_motion");
    assert!(env.eval_int("p.variableKeys.count") >= 24);
    assert_eq!(env.eval_str("p.variableKeys[0]"), "move_UD");
}

#[test]
fn player_find_motion_returns_the_module_and_syncs_variable_keys() {
    let _lock = vm_lock();
    let env = Env::with_reference_storage();
    let path = "e-mote3.0バニラパジャマa.psb";
    env.run("Motion.ResourceManager.setEmotePSBDecryptSeed(742877301);");
    env.run(&format!(
        "var rm = new Motion.ResourceManager(null, 1024); \
         var p = new Motion.Player(rm); \
         var found = p.findMotion(\"{path}\");"
    ));
    // `findMotion` activates the resolved motion, binds its key, syncs the
    // variable keys and returns the module object; only a miss returns void
    // (`reference/cpp/plugins/motionplayer/PlayerResource.cpp:60-73`,
    // `PlayerCore.cpp:1068-1076`).
    assert_eq!(env.eval_str("typeof found"), "Object");
    assert_eq!(env.eval_str("found.path"), path);
    assert_eq!(env.eval_str("found.metadata.base.chara"), "all_parts");
    assert_eq!(env.eval_str("p.motionKey"), path);
    assert!(env.eval_int("p.variableKeys.count") >= 24);
    assert_eq!(env.eval_str("p.variableKeys[0]"), "move_UD");
}

#[test]
fn player_is_exist_motion_resolves_the_candidate_list() {
    let _lock = vm_lock();
    let env = Env::with_reference_storage();
    let path = "e-mote3.0バニラパジャマa.psb";
    env.run("Motion.ResourceManager.setEmotePSBDecryptSeed(742877301);");
    env.run(&format!(
        "var rm = new Motion.ResourceManager(null, 1024); \
         var p = new Motion.Player(rm); \
         var present = p.isExistMotion(\"{path}\"); \
         var absent = p.isExistMotion(\"no-such-motion\");"
    ));
    // `isExistMotion` is true exactly when the name resolves through the
    // player's motion lookup (`reference/cpp/plugins/motionplayer/
    // PlayerResource.cpp:75-78`).
    assert_eq!(env.eval_int("present ? 1 : 0"), 1);
    assert_eq!(env.eval_int("absent ? 1 : 0"), 0);
}

#[test]
fn player_unload_all_drops_the_bound_motion_and_its_variable_keys() {
    let _lock = vm_lock();
    let env = Env::with_reference_storage();
    let path = "e-mote3.0バニラパジャマa.psb";
    env.run("Motion.ResourceManager.setEmotePSBDecryptSeed(742877301);");
    env.run(&format!(
        "var rm = new Motion.ResourceManager(null, 1024); \
         var p = new Motion.Player(rm); \
         p.motionKey = \"{path}\"; \
         var before = p.variableKeys.count; \
         p.unloadAll(); \
         var after = p.variableKeys.count;"
    ));
    // `unloadAll` clears the cache, the runtime motion state and `motionKey`
    // (`reference/cpp/plugins/motionplayer/PlayerResource.cpp:35-56`), so the
    // next variable query has no motion and reports an empty array
    // (`PlayerCore.cpp:1248-1254`).
    assert!(env.eval_int("before") >= 24);
    assert_eq!(env.eval_str("p.motionKey"), "");
    assert_eq!(env.eval_int("after"), 0);
}

#[test]
fn emote_player_load_activates_the_registered_module_snapshot() {
    let _lock = vm_lock();
    let env = Env::with_reference_storage();
    let path = "e-mote3.0バニラパジャマa.psb";
    env.run("Motion.ResourceManager.setEmotePSBDecryptSeed(742877301);");
    env.run(&format!(
        "var rm = new Motion.ResourceManager(null, 1024); \
         var m = rm.load(\"{path}\"); \
         var e = new Motion.EmotePlayer(rm); \
         e.load(m);"
    ));
    // `EmotePlayer::load` stores the module and activates its registered
    // snapshot (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:189-196`),
    // then `syncVariableKeysFromActiveMotion` fills the labels
    // (`PlayerCore.cpp:1068-1076`).
    assert!(env.eval_int("e.variableKeys.count") >= 24);
    assert_eq!(env.eval_str("e.variableKeys[0]"), "move_UD");
}

/// Reference: the `PostRegistCallback` defines `global.ShortCutInitialPadKeyMap`
/// and `global.ShortCutInitialGamePadKeyMap` as empty dictionaries when they
/// are unset, because an encrypted `keybinder.tjs` dereferences them
/// (`reference/cpp/plugins/motionplayer/main.cpp:534-544`).
#[test]
fn shortcut_initial_globals_are_defined() {
    let _lock = vm_lock();
    let env = Env::new();
    assert_eq!(
        env.eval_str("typeof global.ShortCutInitialPadKeyMap"),
        "Object",
        "ShortCutInitialPadKeyMap must be a dictionary global"
    );
    assert_eq!(
        env.eval_str("typeof global.ShortCutInitialGamePadKeyMap"),
        "Object",
        "ShortCutInitialGamePadKeyMap must be a dictionary global"
    );
    // The globals are ordinary writable members (the reference uses
    // `PropSet`), so a game can replace them.
    env.run("global.ShortCutInitialPadKeyMap = 1;");
    assert_eq!(env.eval_int("global.ShortCutInitialPadKeyMap"), 1);
}

#[test]
fn separate_layer_adaptor_layer_tree_owner_interface_delegates_to_its_owner() {
    let _lock = vm_lock();
    let env = Env::new();
    // `_owner` is the construction-time `targetLayer.window`
    // (`reference/cpp/plugins/motionplayer/SeparateLayerAdaptor.cpp:26-34`,
    // `:231`); the hidden `layerTreeOwnerInterface` method returns that
    // owner's own `layerTreeOwnerInterface` property (`:297-330`).
    env.run(
        "var iface = %[]; \
         var win = %[ layerTreeOwnerInterface: iface ]; \
         var sla = new Motion.SeparateLayerAdaptor(%[ window: win ]); \
         var got = sla.layerTreeOwnerInterface(); \
         var first = %[]; var second = %[]; \
         var fixed = new Motion.SeparateLayerAdaptor(%[ window: %[ layerTreeOwnerInterface: first ] ]); \
         fixed.targetLayer = %[ window: %[ layerTreeOwnerInterface: second ] ]; \
         var fixedGot = fixed.layerTreeOwnerInterface();",
    );
    assert_eq!(env.eval_int("got === iface ? 1 : 0"), 1);
    // `setTargetLayer` does not recompute `_owner`.
    assert_eq!(env.eval_int("fixedGot === first ? 1 : 0"), 1);
    assert_eq!(env.eval_int("fixedGot === second ? 1 : 0"), 0);
    // No owner (no constructor argument) leaves the result void.
    assert_eq!(
        env.eval_str("typeof (new Motion.SeparateLayerAdaptor()).layerTreeOwnerInterface()"),
        "void"
    );
}

/// The layer getter is an instance of the reference class and reports the
/// node's own state
/// (`reference/cpp/plugins/motionplayer/PlayerLayerQuery.cpp:24-71`).
#[test]
fn the_layer_getter_is_a_motion_layer_getter_instance() {
    let _lock = vm_lock();
    let env = Env::with_reference_storage();
    let path = "e-mote3.0バニラパジャマa.psb";
    env.run("Motion.ResourceManager.setEmotePSBDecryptSeed(742877301);");
    env.run(&format!(
        "var rm = new Motion.ResourceManager(null, 1024); \
         var p = new Motion.Player(rm); \
         p.motionKey = \"{path}\";"
    ));
    env.run(
        "var names = p.getLayerNames(); \
         var layerLabel = \"\"; \
         for (var i = 0; i < names.count; i++) { if (names[i] != \"\") { layerLabel = names[i]; break; } } \
         var layerGetter = p.getLayerGetter(layerLabel);",
    );
    // The getter is an instance of the registered class. `instanceof` takes a
    // class-name string, and a native class answers under the name it was
    // registered with
    // (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2684-2701`).
    assert_eq!(env.eval_int("layerGetter instanceof \"LayerGetter\""), 1);
    // A plain dictionary is not an instance of it.
    assert_eq!(env.eval_int("(%[]) instanceof \"LayerGetter\""), 0);
    // The class is reachable under the Motion namespace.
    assert_eq!(env.eval_int("Motion.LayerGetter != null"), 1);
    // The label is the one the getter was asked for.
    assert_eq!(env.eval_int("layerGetter.label == layerLabel"), 1);
    // The type and the visibility are readable.
    assert_eq!(env.eval_int("layerGetter.type >= 0"), 1);
    assert_eq!(
        env.eval_int("layerGetter.visible == 0 || layerGetter.visible == 1"),
        1
    );
    // The opacity is the integer 0..255 scale.
    assert_eq!(
        env.eval_int("layerGetter.opacity >= 0 && layerGetter.opacity <= 255"),
        1
    );
    // The matrix carries the node's transform and its position, and the
    // vertex list holds the four corners of the node's box.
    assert_eq!(env.eval_real("layerGetter.mtx.count"), 6.0);
    assert_eq!(env.eval_real("layerGetter.vtx.count"), 8.0);
    // A node that names no colour of its own reports the default half gray
    // with a full alpha, not the player's colour
    // (`reference/cpp/plugins/motionplayer/PlayerLayerQuery.cpp:66-71`,
    // `PlayerInternal.h:1102-1120`).
    assert_eq!(env.eval_int("layerGetter.color[0]"), 128);
    assert_eq!(env.eval_int("layerGetter.color[1]"), 128);
    assert_eq!(env.eval_int("layerGetter.color[2]"), 128);
    assert_eq!(env.eval_int("layerGetter.color[3]"), 255);
}

/// The color weight is read and written in the swapped channel order, so a
/// non-gray value keeps its channels apart
/// (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:24-27`, `:144-152`).
#[test]
fn the_color_weight_round_trips_through_the_script_surface() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var p = new Motion.Player(null);");
    env.run("p.colorWeight = 0x00112233;");
    assert_eq!(env.eval_int("p.colorWeight"), 0x0011_2233);
    // A value whose red and blue channels differ keeps them apart.
    env.run("p.colorWeight = 0x00FF0080;");
    assert_eq!(env.eval_int("p.colorWeight"), 0x00FF_0080);
    // The stored weight is the swapped one.
    env.run("p.colorWeight = 0x000000FF;");
    assert_eq!(env.eval_int("p.colorWeight"), 0x0000_00FF);
}

/// A freshly activated motion sits behind the frame gate: the first tick
/// presents the pose without advancing, and a delta outside the accepted
/// window is dropped rather than clamped
/// (`reference/cpp/plugins/motionplayer/PlayerFrameProgress.cpp:819-826`,
/// `:1002-1004`). The `progress` member reports the playback ratio
/// (`PlayerCore.cpp:1281-1305`).
#[test]
fn progress_gates_the_first_tick_and_drops_an_out_of_range_delta() {
    let _lock = vm_lock();
    let env = Env::with_reference_storage();
    let path = "e-mote3.0バニラパジャマa.psb";
    env.run("Motion.ResourceManager.setEmotePSBDecryptSeed(742877301);");
    env.run(&format!(
        "var rm = new Motion.ResourceManager(null, 1024); \
         var p = new Motion.Player(rm); \
         p.motionKey = \"{path}\";"
    ));
    // No timeline is playing yet, so the tick count starts at zero.
    assert_eq!(env.eval_real("p.frameTickCount"), 0.0);
    env.run("p.progress(16);");
    assert_eq!(
        env.eval_real("p.frameTickCount"),
        0.0,
        "the gated tick does not advance the clock"
    );
    env.run("p.progress(16);");
    let advanced = env.eval_real("p.frameTickCount");
    assert!(advanced > 0.0, "the second tick advances");
    // Nothing plays, so the reported ratio is the finished 1.0.
    assert_eq!(env.eval_real("p.progress(0)"), 1.0);
    env.run("p.progress(1000000000);");
    assert_eq!(
        env.eval_real("p.frameTickCount"),
        advanced,
        "a delta past 60000 ms is dropped, not clamped"
    );
    env.run("p.progress(-5);");
    assert_eq!(env.eval_real("p.frameTickCount"), advanced);
}

#[test]
fn d3d_adaptor_constructor_validates_its_window_and_size_arguments() {
    let _lock = vm_lock();
    let env = Env::new();
    // `Factory` requires five parameters (`reference/cpp/plugins/motionplayer/
    // D3DAdaptor.cpp:19-20`) and a `Window` first argument, otherwise it throws
    // `"must set Window object"` (`:21-27`).
    assert!(env.eval("new Motion.D3DAdaptor()").is_err());
    assert!(env.eval("new Motion.D3DAdaptor(0, 1, 1, 0, 0)").is_err());
    assert!(env.eval("new Motion.D3DAdaptor(%[], 1, 1, 0, 0)").is_err());
}
