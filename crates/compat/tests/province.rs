//! Integration tests for the province plane: the 8-bit index plane a layer
//! reads through `getProvincePixel`.
//!
//! The plane is the image's own 8-bit sample — the palette index of a
//! palettized image — and not the colour the palette maps that index to
//! (`reference/cpp/core/visual/LayerIntf.cpp:2893-2914`,
//! `reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1709,1838`,
//! `reference/cpp/core/visual/LoadPNG.cpp:278-290`). A map region is
//! therefore addressed by its index, and a region whose index the action
//! table does not know is not a region at all.
//!
//! The tests install the real L1 `media` decoder through the host's codec
//! seam, so the committed fixture is decoded by the code the game runs.

mod common;

use std::cell::RefCell;
use std::path::Path;
use std::time::Duration;

use api::fs::{FsPath, OpenMode};
use common::vm_lock;
use compat::CompatHost;
use engine::input::InputState;
use engine::{Scene, Storage};
use fs::FileSystem;
use media::{Media, decode_image, decode_index_plane, probe_image};
use platform::{InputEvent, MouseButton, PlatformEvent};
use tjs::{Tjs2Engine, TjsValue};

/// The committed fixture: a 4x4 palettized PNG whose palette index is the
/// region id.
///
/// The palette is decorative — index 16 is the red (235, 0, 0), index 17 the
/// darker (215, 0, 0) — so a plane that carried the colour instead of the
/// index would answer 235 where the region id is 16. The first row is outside
/// every region (index 0).
const FIXTURE: &[u8] = include_bytes!("fixtures/province.png");

/// Writes `data` to `path` in an in-memory file system, creating parents.
fn write_memory_file(fs: &FileSystem, path: &str, data: &[u8]) {
    if let Some(parent) = Path::new(path).parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(fs, &FsPath::host(parent)).expect("create parent dirs");
    }
    let file = fs::open(fs, &FsPath::host(path), OpenMode::Write).expect("open memory file");
    fs::write_at(fs, &file, 0, data).expect("write memory file");
    fs::flush(fs, &file).expect("flush memory file");
}

/// A bound VM + host + scene + input state, plus an in-memory storage holding
/// the fixture.
///
/// The boxed engine, scene, input state and storage are declared **after**
/// `host` so the host (which stores their raw addresses) is dropped first.
struct Env {
    /// Kept alive for the host's raw pointer; never read directly.
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    /// Kept alive for the host's raw pointer; never read directly.
    _scene: Box<Scene>,
    /// Kept alive for the host's raw pointer; never read directly.
    _input: Box<InputState>,
    /// Kept alive for the host's raw pointer; never read directly.
    _storage: Box<Storage>,
    /// The adapter the events are delivered through. It outlives one call
    /// because it keeps the events a frame deferred.
    adapter: RefCell<compat::EventAdapter>,
}

impl Env {
    /// Builds an environment whose codec is the real L1 `media` decoder.
    fn new() -> Env {
        let fs = FileSystem::memory();
        write_memory_file(&fs, "/province.png", FIXTURE);
        let storage =
            Storage::mount_archives(fs, "/", std::iter::empty::<&str>()).expect("mount storage");

        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut input = Box::new(InputState::new());
        let mut storage = Box::new(storage);
        let mut host = CompatHost::new();
        let media = Media::new();
        host.set_image_codec(
            move |bytes, hint| decode_image(&media, bytes, hint).map_err(|error| error.to_string()),
            move |bytes, hint| probe_image(&media, bytes, hint).map_err(|error| error.to_string()),
            |_name, _type, _width, _height, _rgba| Ok(()),
        );
        host.set_index_plane_decoder(move |bytes, hint| {
            decode_index_plane(&media, bytes, hint).map_err(|error| error.to_string())
        });
        // SAFETY: the boxed scene, input state and storage outlive the host.
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_input(input.as_mut() as *mut InputState);
            host.services_mut()
                .attach_storage(storage.as_mut() as *mut Storage);
        }
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
            _scene: scene,
            _input: input,
            _storage: storage,
            // The adapter's default budget is a wall-clock slice, so on a
            // loaded machine it can cut a test's event sequence short. This
            // one delivers every event it is given.
            adapter: RefCell::new(compat::EventAdapter::with_dispatch_budget(Duration::MAX)),
        }
    }

    /// Feed one frame's platform events through the adapter.
    fn dispatch_events(&self, events: &[PlatformEvent]) {
        let mut adapter = self.adapter.borrow_mut();
        let mut ctx = self._host.ctx().expect("bound compat context");
        adapter.begin_input_frame(&mut ctx);
        adapter
            .dispatch_frame(&mut ctx, events)
            .expect("dispatch events");
        adapter.end_input_frame(&mut ctx);
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "province-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "province-test")
            .map_err(|error| error.to_string())
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.eval(expression) {
            Ok(TjsValue::Integer(value)) => value,
            Ok(other) => panic!("{expression:?} returned {other:?}, expected an integer"),
            Err(error) => panic!("eval {expression:?} failed: {error}"),
        }
    }

    /// Evaluate an expression that must raise, returning the message.
    fn error(&self, expression: &str) -> String {
        self.eval(expression)
            .expect_err(&format!("{expression:?} should have raised"))
    }
}

/// The province plane holds the palette index of the image, not the colour
/// the palette maps it to.
///
/// Reference: the province image is loaded as a palettized bitmap whose
/// plane keeps the palette indexes
/// (`reference/cpp/core/visual/LayerIntf.cpp:2893-2914`,
/// `reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1709,1838`), and the PNG
/// loader leaves the 8-bit plane alone
/// (`reference/cpp/core/visual/LoadPNG.cpp:278-290,1092-1116`). The fixture's
/// index 16 is the decorative red (235, 0, 0), so a plane that carried the
/// colour would answer 235 where the region id is 16.
#[test]
fn the_province_plane_holds_the_palette_index() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(4, 4); \
         l.loadProvinceImage('province.png');",
    );

    assert_eq!(
        env.eval_int("l.getProvincePixel(0, 1)"),
        16,
        "the plane holds the palette index, not the palette colour"
    );
    assert_eq!(env.eval_int("l.getProvincePixel(2, 1)"), 17);
    assert_eq!(env.eval_int("l.getProvincePixel(0, 2)"), 18);
    assert_eq!(env.eval_int("l.getProvincePixel(2, 3)"), 21);
    assert_eq!(
        env.eval_int("l.getProvincePixel(0, 0)"),
        0,
        "the row outside every region stays 0"
    );
}

/// A region is addressed by its index, so the action table of a map image
/// finds the region a click landed on.
///
/// The shape is the one a KAG map layer uses: the click is turned into the
/// point's province value, and the value selects the action
/// (`reference/cpp/core/visual/LayerIntf.cpp:2973-2981` reads the plane).
/// The hit test reads the same plane, so the region outside every id is not
/// a hit at all (`reference/cpp/core/visual/LayerIntf.cpp:2893-2914` sizes
/// the plane, and the province hit test takes a non-zero plane byte).
#[test]
fn a_click_on_a_region_runs_the_region_action() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         w.setSize(320, 240); \
         var map = new Layer(w, null); \
         map.setPos(0, 0); map.setSize(4, 4); map.visible = true; map.focusable = true; \
         map.setImageSize(4, 4); \
         map.hitType = 1; \
         map.loadProvinceImage('province.png'); \
         var actions = %[16 => 'start']; \
         var fired = 0; \
         function processProvince(x, y) { \
             var region = map.getProvincePixel(x, y); \
             if (region == 0) return; \
             var action = actions[region]; \
             if (action) { fired = fired + 1; } \
         } \
         map.onMouseDown = function(x, y, button, shift) { processProvince(x, y); };",
    );

    // The press inside the region 16 (the second row, first column).
    env.dispatch_events(&[
        PlatformEvent::Input(InputEvent::PointerMoved { x: 0.0, y: 1.0 }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: false,
        }),
    ]);
    assert_eq!(
        env.eval_int("fired"),
        1,
        "the click on region 16 runs the region's action"
    );

    // The press outside every region: the plane byte is 0, so the point is
    // not a hit and no action runs.
    env.dispatch_events(&[
        PlatformEvent::Input(InputEvent::PointerMoved { x: 0.0, y: 0.0 }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: false,
        }),
    ]);
    assert_eq!(
        env.eval_int("fired"),
        1,
        "the click outside every region runs no action"
    );
}

/// A region the action table does not know is not a region: the plane value
/// is looked up as it is, so a colour-derived value would find nothing.
///
/// Reference: the province value is the palette index
/// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1709`), and the map
/// layer's action table is keyed by that index.
#[test]
fn a_region_the_action_table_does_not_know_runs_no_action() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var map = new Layer(w, null); \
         map.setImageSize(4, 4); \
         map.loadProvinceImage('province.png'); \
         var actions = %[16 => 'start']; \
         var fired = 0; \
         function processProvince(x, y) { \
             var region = map.getProvincePixel(x, y); \
             if (region == 0) return; \
             var action = actions[region]; \
             if (action) { fired = fired + 1; } \
         } \
         processProvince(0, 1); \
         processProvince(2, 1);",
    );

    assert_eq!(
        env.eval_int("fired"),
        1,
        "only the region the table knows runs an action"
    );
}

/// A province image that cannot be read raises instead of answering a
/// colour.
///
/// Reference: the province load reports the storage failure unchanged
/// (`reference/cpp/core/visual/LayerIntf.cpp:2893-2914`), and the plane is
/// dropped when the load fails.
#[test]
fn a_missing_province_image_raises() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(4, 4);",
    );

    let message = env.error("l.loadProvinceImage('missing.png');");
    assert!(
        message.contains("not found"),
        "the missing province image raises: {message}"
    );
}
