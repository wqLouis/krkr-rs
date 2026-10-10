//! Reference-behaviour parity tests for the **layer core**:
//! draw device, blend mapping, events, focus, order, pixels and util.
//!
//! Every test states the **KiriKiri (reference C++) expectation** with a
//! `reference/cpp/**` citation above it and asserts that expectation against
//! the observed script-visible behaviour, exactly like
//! `crates/compat/tests/timer_behavior.rs`. The tests attach a caller-owned
//! L2 [`Scene`] (plus the input and timer registries) to a [`CompatHost`]
//! and run TJS against it — the same seam the app uses — so side effects are
//! verified against the L2 state the natives actually mutate.
//!
//! Known reference divergences that **cannot** be fixed inside the audited
//! files (documented here so the exclusions are visible):
//! * `Layer.type` echoes only the *canonical* spelling of the generic blend
//!   it stores (`layer/mod.rs` getter + `engine::LayerState::blend`), while
//!   the reference stores and re-reads the raw `tTVPLayerType`
//!   (`LayerIntf.cpp:10832-10846`); the non-canonical spellings
//!   (0, 6, 7, 13–17, 21, 24, 25) therefore read back canonicalised. The
//!   canonical round-trip is pinned below.
//! * The reference `dtNone` … `dtDBD3D` members are reachable through an
//!   *instance* as well (`tTJSNativeClass` member fallback); the port
//!   registers them as class (static) members only, because adding
//!   instance properties would change the locked VM surface. The
//!   class-qualified access the reference documentation uses
//!   (`PassThroughDrawDevice.cpp:29-70`) is pinned below.

mod common;

use common::vm_lock;
use compat::CompatHost;
use engine::Scene;
use engine::input::InputState;
use engine::timer::TimerState;
use tjs::{Tjs2Engine, TjsValue};

/// Script label for diagnostics.
const SCRIPT: &str = "layer-behaviour-test";

/// A bound VM + host + attached scene, input state and timer registry.
///
/// Fields drop in declaration order: the host first (releasing every
/// retained script object), then the engine, the scene, the input state and
/// the timer registry.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    scene: Box<Scene>,
    /// Kept alive for the host's raw pointer; never read directly.
    _timer: Box<TimerState>,
    /// Kept alive for the host's raw pointer; never read directly.
    input: Box<InputState>,
}

impl Env {
    fn new() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut timer = Box::new(TimerState::new());
        let mut input = Box::new(InputState::new());
        let mut host = CompatHost::new();
        // SAFETY: the boxed scene, timer registry and input state outlive
        // the host (they are declared after `_host`).
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_timer(timer.as_mut() as *mut TimerState);
            host.services_mut()
                .attach_input(input.as_mut() as *mut InputState);
        }
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
            scene,
            _timer: timer,
            input,
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, SCRIPT)
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    /// Run a script and keep a failure observable instead of panicking (for
    /// cases where the reference *accepts* the script and the port may
    /// raise, or vice versa).
    fn try_run(&self, script: &str) -> Result<(), String> {
        self.engine
            .exec_script(script, SCRIPT)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, SCRIPT)
            .map_err(|error| error.to_string())
    }

    fn eval_ok(&self, expression: &str) -> TjsValue {
        self.eval(expression)
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.eval_ok(expression) {
            TjsValue::Integer(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    fn assert_script(&self, expression: &str) {
        assert_eq!(
            self.eval_int(expression),
            1,
            "script assertion {expression:?}"
        );
    }
}

/// Create a window + layer and give the layer a `width` x `height` image.
fn layer_with_image(env: &Env, width: u32, height: u32) {
    env.run(&format!(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize({width}, {height});"
    ));
}

/// The names of a window's layers, back to front (the maintained sibling
/// vector the order natives reorder).
fn window_layer_names(env: &Env) -> Vec<String> {
    let window = env.scene.windows.first().expect("one window");
    window
        .layers
        .iter()
        .map(|id| {
            env.scene
                .layer(*id)
                .map(|l| l.name.clone())
                .unwrap_or_else(|| "?".to_string())
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 1. Draw devices
// ---------------------------------------------------------------------------

/// Reference: `tTVPBasicDrawDevice` initializes both `DrawerType` and
/// `PreferredDrawerType` to `dtDrawDib` (= 1)
/// (`reference/cpp/core/visual/impl/BasicDrawDevice.h:88-96`), and the
/// `recreate()` native body is compiled out (`#if 0`)
/// (`reference/cpp/core/visual/impl/BasicDrawDevice.cpp:869-876`), so
/// `recreate()` changes nothing.
#[test]
fn basic_draw_device_defaults_and_recreate() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var d = new Window.BasicDrawDevice(); \
         var drawer0 = d.drawer; \
         var pref0 = d.preferredDrawer; \
         d.recreate(); \
         var drawer1 = d.drawer;",
    );
    assert_eq!(env.eval_int("drawer0"), 1); // dtDrawDib
    assert_eq!(env.eval_int("pref0"), 1); // dtDrawDib
    assert_eq!(env.eval_int("drawer1"), 1);
}

/// Reference: `tTVPPassThroughDrawDevice`'s constructor starts with
/// `Drawer = nullptr`, `DrawerType = dtNone`, `PreferredDrawerType =
/// TVPPreferredDrawType` (= `dtNone`) and `LockedWidth = LockedHeight = -1`
/// (`reference/cpp/core/visual/impl/PassThroughDrawDevice.cpp:197-206`);
/// `recreate()` runs `SetToRecreateDrawer()` + `EnsureDrawer()`
/// (`PassThroughDrawDevice.cpp:505-513`), and `EnsureDrawer` always builds
/// the software drawer `CreateDrawer(dtDrawDib)` when no drawer exists
/// (`PassThroughDrawDevice.cpp:274-278`, `293-340`) — so `drawer` reads
/// back `dtDrawDib` (= 1) afterwards.
#[test]
fn passthrough_draw_device_defaults_and_recreate() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var d = new Window.PassThroughDrawDevice(); \
         var drawer0 = d.drawer; \
         var pref0 = d.preferredDrawer; \
         d.recreate(); \
         var drawer1 = d.drawer;",
    );
    assert_eq!(env.eval_int("drawer0"), 0); // dtNone
    assert_eq!(env.eval_int("pref0"), 0); // dtNone
    assert_eq!(env.eval_int("drawer1"), 1); // dtDrawDib
}

/// Reference: the `preferredDrawer` setter casts through `(tjs_int)`
/// (`reference/cpp/core/visual/impl/BasicDrawDevice.cpp:890-891`,
/// `PassThroughDrawDevice.cpp:575-576`); `operator tjs_int` is
/// `(tjs_int)AsInteger()` (`reference/cpp/core/tjs2/tjsVariant.h:933-935`)
/// and `AsInteger` truncates a real toward zero / parses a string
/// (`tjsVariant.h:862-881`). So in-range integers round-trip, `2.7` reads
/// back `2`, `"4"` reads back `4`, and a value outside the 32-bit range
/// reads back **truncated** (`4294967296 == 2^32` → `0`).
#[test]
fn preferred_drawer_round_trips_and_truncates() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var d = new Window.BasicDrawDevice(); \
         d.preferredDrawer = 3; var v3 = d.preferredDrawer; \
         d.preferredDrawer = -1; var vm1 = d.preferredDrawer; \
         d.preferredDrawer = 2.7; var vreal = d.preferredDrawer; \
         d.preferredDrawer = '4'; var vstr = d.preferredDrawer; \
         d.preferredDrawer = 4294967296; var vbig = d.preferredDrawer;",
    );
    assert_eq!(env.eval_int("v3"), 3);
    assert_eq!(env.eval_int("vm1"), -1);
    assert_eq!(env.eval_int("vreal"), 2);
    assert_eq!(env.eval_int("vstr"), 4);
    assert_eq!(env.eval_int("vbig"), 0);
}

/// Reference: `drawer` is registered with `TJS_DENY_NATIVE_PROP_SETTER`
/// (`reference/cpp/core/visual/impl/BasicDrawDevice.cpp:933-941`,
/// `PassThroughDrawDevice.cpp:583-591`) — a write never reaches the stored
/// drawer type, so the value reads back unchanged.
#[test]
fn drawer_is_read_only() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var d = new Window.BasicDrawDevice(); \
         var before = d.drawer; \
         try { d.drawer = 9; } catch (e) {} \
         var after = d.drawer;",
    );
    assert_eq!(env.eval_int("before"), 1);
    assert_eq!(env.eval_int("after"), 1);
}

/// Reference: the class-qualified constants documented in the reference
/// header comment (`reference/cpp/core/visual/impl/PassThroughDrawDevice.cpp:29-70`)
/// are the `TVP_REGISTER_PTDD_ENUM` read-only properties
/// (`BasicDrawDevice.cpp:898-913`, `PassThroughDrawDevice.cpp:545-560`) in
/// the enum order `dtNone, dtDrawDib, dtDBGDI, dtDBDD, dtDBD3D`
/// (`BasicDrawDevice.h:88-95`).
#[test]
fn draw_device_constants_are_class_qualified() {
    let _lock = vm_lock();
    let env = Env::new();
    env.assert_script("Window.BasicDrawDevice.dtNone == 0");
    env.assert_script("Window.BasicDrawDevice.dtDrawDib == 1");
    env.assert_script("Window.BasicDrawDevice.dtDBGDI == 2");
    env.assert_script("Window.BasicDrawDevice.dtDBDD == 3");
    env.assert_script("Window.BasicDrawDevice.dtDBD3D == 4");
    env.assert_script("Window.PassThroughDrawDevice.dtNone == 0");
    env.assert_script("Window.PassThroughDrawDevice.dtDBD3D == 4");
}

/// Reference: `interface` returns the raw native device pointer as an
/// integer (`reference/cpp/core/visual/impl/BasicDrawDevice.cpp:886-896`,
/// `PassThroughDrawDevice.cpp:532-544`). The native payload address is the
/// port's stable stand-in for that pointer.
#[test]
fn interface_getter_is_a_synthetic_pointer() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var d = new Window.BasicDrawDevice();");
    let interface = env.eval_int("d.interface");
    assert_ne!(
        interface, 0,
        "interface must be a non-zero synthetic pointer"
    );
    assert_eq!(env.eval_int("d.interface"), interface);
}

/// Reference: `Invalidate()` runs `Device->Destruct()` and clears the
/// pointer (`reference/cpp/core/visual/impl/BasicDrawDevice.cpp:947-951`,
/// `PassThroughDrawDevice.cpp:618-622`), and the reference VM refuses
/// member access on an invalidated native object with "The object is
/// already invalidated" (`reference/cpp/core/tjs2/tjsError.h:298`). The
/// vendored VM performs that check, so member access after `invalidate`
/// must fail.
#[test]
fn invalidated_draw_device_refuses_member_access() {
    let _lock = vm_lock();
    let env = Env::new();
    let error = env
        .try_run("var d = new Window.BasicDrawDevice(); invalidate d; var x = d.drawer;")
        .expect_err("member access on an invalidated object must be refused");
    println!("observed invalidated-member error: {error}");
}

/// Reference: `lockTouchSize` exists only on `PassThroughDrawDevice`
/// (`reference/cpp/core/visual/impl/PassThroughDrawDevice.cpp:515-526`;
/// the `BasicDrawDevice` registration has only the constructor, `recreate`
/// and the properties, `BasicDrawDevice.cpp:854-944`) and raises
/// `TJS_E_BADPARAMCOUNT` with fewer than two arguments
/// (`PassThroughDrawDevice.cpp:517-518`).
#[test]
fn lock_touch_size_requires_two_arguments() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var d = new Window.PassThroughDrawDevice();");
    assert!(env.try_run("d.lockTouchSize(1);").is_err());
    env.run("d.lockTouchSize(1, 2);"); // accepted, result void
    // `BasicDrawDevice` has no `lockTouchSize` member at all.
    env.run("var b = new Window.BasicDrawDevice();");
    assert!(env.try_run("b.lockTouchSize(1, 2);").is_err());
}

// ---------------------------------------------------------------------------
// 2. Blend mapping (`Layer.type`)
// ---------------------------------------------------------------------------

/// Reference: `Layer.type` reads/writes `tTVPLayerType`
/// (`reference/cpp/core/visual/LayerIntf.cpp:10832-10846`, stored raw by
/// `SetType` `LayerIntf.cpp:1687-1700`) and the getter writes a
/// `tjs_int` — an **Integer**, never a real. The port stores the generic
/// blend, so only the canonical spellings round-trip: every value whose
/// canonical encoder (`blend::to_native`) reproduces it. See the file
/// header for the excluded non-canonical spellings.
#[test]
fn layer_type_round_trips_canonical_values_and_is_an_integer() {
    let _lock = vm_lock();
    let env = Env::new();
    layer_with_image(&env, 1, 1);
    // Canonical spellings of the shared blends (ltOpaque … ltPsExclusion),
    // `reference/cpp/core/visual/drawable.h:19-51`.
    for value in [
        1, 2, 3, 4, 5, 8, 9, 10, 11, 12, 18, 19, 20, 22, 23, 26, 27, 28,
    ] {
        env.run(&format!("l.type = {value};"));
        assert_eq!(
            env.eval("l.type").expect("type read"),
            TjsValue::Integer(value),
            "layer type {value} must round-trip as an Integer"
        );
    }
}

// ---------------------------------------------------------------------------
// 3. Event handlers
// ---------------------------------------------------------------------------

/// Reference: every `on*` native builds its event object behind
/// `TVP_ACTION_INVOKE_BEGIN(argnum, …)`, whose first statement is
/// `if(numparams < (argnum)) return TJS_E_BADPARAMCOUNT;`
/// (`reference/cpp/core/base/EventIntf.h:208-211`), and a normally
/// constructed layer always has an action owner (the constructor stores
/// `ActionOwner = param[0]`, `LayerIntf.cpp:476`). The argument counts are
/// the reference literals: `onClick`/`onDoubleClick` 2
/// (`LayerIntf.cpp:9976`), `onMouseDown`/`onMouseUp` 4 (`10008`, `10026`),
/// `onMouseMove` 3 (`10044`), `onFocus`/`onBlur` 1 (`10214`, `10199`),
/// `onKeyDown`/`onKeyUp` 3 (`10258`, `10279`), `onKeyPress` 2 (`10300`),
/// `onMouseWheel` 4 (`10327`), touch events 5 / `onTouchRotate` 6
/// (`10089`–`10165`), `onBeforeFocus` 3 (`10419`), the search handlers 1
/// (`10346`, `10383`), `onTransitionCompleted` 2 (`10472`), while
/// `onMouseEnter`/`onMouseLeave`/`onNodeEnabled`/`onNodeDisabled`/
/// `onMultiTouch`/`onPaint` take 0 (`10061`, `10075`, `10230`, `10244`,
/// `10185`, `10457`). `onHitTest`'s check is *unconditional*
/// (`LayerIntf.cpp:9960-9961`).
#[test]
fn event_handlers_enforce_reference_argument_counts() {
    let _lock = vm_lock();
    let env = Env::new();
    layer_with_image(&env, 1, 1);
    // Too few arguments raises…
    for call in [
        "l.onClick()",
        "l.onDoubleClick(1)",
        "l.onMouseDown(1, 2, 3)",
        "l.onMouseUp(1, 2, 3)",
        "l.onMouseMove(1, 2)",
        "l.onFocus()",
        "l.onBlur()",
        "l.onKeyDown(1, 2)",
        "l.onKeyUp(1, 2)",
        "l.onKeyPress(1)",
        "l.onMouseWheel(1, 2, 3)",
        "l.onTouchDown(1, 2, 3, 4)",
        "l.onTouchRotate(1, 2, 3, 4, 5)",
        "l.onBeforeFocus(1, 2)",
        "l.onSearchNextFocusable()",
        "l.onSearchPrevFocusable()",
        "l.onTransitionCompleted(1)",
        "l.onHitTest(1, 2)",
    ] {
        assert!(
            env.try_run(&format!("var r = {call};")).is_err(),
            "{call} must raise TJS_E_BADPARAMCOUNT like the reference"
        );
    }
    // …and the full counts succeed.
    for call in [
        "l.onClick(1, 2)",
        "l.onMouseDown(1, 2, 3, 4)",
        "l.onMouseMove(1, 2, 3)",
        "l.onFocus(1)",
        "l.onKeyDown(1, 2, 3)",
        "l.onKeyPress(1, 2)",
        "l.onMouseWheel(1, 2, 3, 4)",
        "l.onTouchDown(1, 2, 3, 4, 5)",
        "l.onTouchRotate(1, 2, 3, 4, 5, 6)",
        "l.onBeforeFocus(1, 2, 3)",
        "l.onSearchNextFocusable(1)",
        "l.onSearchPrevFocusable(1)",
        "l.onTransitionCompleted(1, 2)",
        "l.onHitTest(1, 2, true)",
        "l.onMouseEnter()",
        "l.onMouseLeave()",
        "l.onNodeEnabled()",
        "l.onNodeDisabled()",
        "l.onMultiTouch()",
        "l.onPaint()",
    ] {
        env.run(&format!("var r = {call};"));
    }
}

/// Reference: the event adapter reaches a handler **through the object**
/// (`BitmapLayerTreeOwner` dispatches by name; the reference fires the
/// owner's member, `reference/cpp/core/visual/LayerIntf.cpp:3388-3418`), so
/// a script assignment shadows the native class method — the instance
/// member is found first and the native's argument-count gate never runs.
/// The reference native itself only exists to forward to the action owner
/// (`LayerIntf.cpp:9970-9984`).
#[test]
fn script_overrides_shadow_the_native_handlers() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         var hits = 0; var sum = 0; \
         l.onClick = function(x, y) { hits++; sum = x + y; }; \
         var owner = new BitmapLayerTreeOwner(); \
         owner.focusedLayer = l; \
         var consumed = owner.fireClick(11, 22);",
    );
    // The override ran *through the object* with the dispatched payload…
    assert_eq!(env.eval_int("hits"), 1);
    assert_eq!(env.eval_int("sum"), 33);
    // …and its return value is the dispatch result.
    assert_eq!(
        env.eval("consumed").expect("consumed"),
        TjsValue::Integer(0)
    );
}

/// Reference: with no override the dispatch reaches the native class
/// method through the same by-name lookup; the reference native only runs
/// when the argument count is satisfied (`TVP_ACTION_INVOKE_BEGIN`,
/// `reference/cpp/core/base/EventIntf.h:208-211`), which the tree owner's
/// two-element `onClick` payload always is
/// (`crates/compat/src/natives/tree_owner.rs` `payload_xy`). The call must
/// therefore not raise, and the default handler consumes nothing.
#[test]
fn dispatch_without_an_override_reaches_the_native_without_raising() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         var owner = new BitmapLayerTreeOwner(); \
         owner.focusedLayer = l; \
         var consumed = owner.fireClick(); \
         var survived = 1;",
    );
    env.assert_script("survived == 1");
    assert_eq!(env.eval_int("consumed"), 0);
}

// ---------------------------------------------------------------------------
// 4. Focus / capture
// ---------------------------------------------------------------------------

/// Reference: `tTVPLayerManager::FocusNext` starts from the manager's
/// focused layer — **not** from the receiver — and falls back to
/// `SearchFirstFocusable` when nothing is focused
/// (`reference/cpp/core/visual/LayerManager.cpp:736-747`,
/// `LayerIntf.cpp:3574-3593`). With three focusable layers `a, b, c` and no
/// focus, `b.focusNext()` must return the *first* focusable layer `a`, not
/// the receiver's neighbour `c`.
#[test]
fn focus_search_starts_from_the_focused_layer_not_the_receiver() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.focusable = true; a.visible = true; a.name = 'a'; \
         var b = new Layer(w, null); b.focusable = true; b.visible = true; b.name = 'b'; \
         var c = new Layer(w, null); c.focusable = true; c.visible = true; c.name = 'c'; \
         var got = b.focusNext();",
    );
    assert_eq!(env.eval_int("got === a"), 1);
}

/// Reference: `FocusPrev` with no focused layer also uses
/// `SearchFirstFocusable(false)` — the *first* focusable layer, not the
/// last (`reference/cpp/core/visual/LayerManager.cpp:723-733`).
#[test]
fn focus_prev_without_focus_returns_the_first_focusable() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.focusable = true; a.visible = true; a.name = 'a'; \
         var b = new Layer(w, null); b.focusable = true; b.visible = true; b.name = 'b'; \
         var c = new Layer(w, null); c.focusable = true; c.visible = true; c.name = 'c'; \
         var got = c.focusPrev();",
    );
    assert_eq!(env.eval_int("got === a"), 1);
}

/// Reference: once a layer is focused, `FocusNext`/`FocusPrev` wrap around
/// the focus chain from the *focused* layer
/// (`reference/cpp/core/visual/LayerManager.cpp:723-747` with
/// `LayerIntf.cpp:3631-3660`: `GetNextFocusable` searches from `this`,
/// wrapping, skipping non-focusable nodes).
#[test]
fn focus_neighbours_walk_from_the_current_focus() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.focusable = true; a.visible = true; a.name = 'a'; \
         var b = new Layer(w, null); b.focusable = true; b.visible = true; b.name = 'b'; \
         a.focus(); \
         var next = a.focusNext(); \
         var prev = b.focusPrev();",
    );
    assert_eq!(env.eval_int("next.name == 'b'"), 1);
    assert_eq!(env.eval_int("prev.name == 'a'"), 1);
}

/// Reference: `FocusNext` returns `nullptr` when no focusable neighbour
/// exists and then does **not** touch the focused layer
/// (`reference/cpp/core/visual/LayerManager.cpp:736-747`: `if(l)
/// SetFocusTo(l, true); return l;`), and the script wrapper maps `nullptr`
/// to TJS `null` (`LayerIntf.cpp:9758-9766`).
#[test]
fn focus_next_returns_null_when_nothing_is_focusable() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         var got = l.focusNext();",
    );
    assert_eq!(env.eval("got === null"), Ok(TjsValue::Integer(1)));
}

/// Reference: `setAttentionPos` stores both components on the layer
/// (`reference/cpp/core/visual/LayerIntf.cpp:3207-3212`), and the native
/// requires two arguments (`LayerIntf.cpp:9800-9810`).
#[test]
fn set_attention_pos_stores_both_components() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setAttentionPos(3, 4);",
    );
    let layer = env.scene.layers.first().expect("one layer");
    assert_eq!((layer.attention_left, layer.attention_top), (3, 4));
    assert!(env.try_run("l.setAttentionPos(3);").is_err());
}

/// Reference: `SetCursorPos` converts **layer-local** coordinates to window
/// coordinates by adding this layer's and every *parented* ancestor's
/// offset, stopping before the parentless root
/// (`reference/cpp/core/visual/LayerIntf.cpp:3150-3163`), then hands the
/// result to the manager (`LayerIntf.cpp:9689-9697` requires two
/// arguments).
#[test]
fn set_cursor_pos_accumulates_the_parent_chain_offsets() {
    let _lock = vm_lock();
    let mut env = Env::new();
    env.run(
        "var w = new Window(); \
         var g = new Layer(w, null); g.left = 10; g.top = 20; \
         var p = new Layer(w, g); p.left = 30; p.top = 40; \
         var c = new Layer(w, p); c.left = 5; c.top = 6; \
         c.setCursorPos(1, 2);",
    );
    // c adds its own (5, 6), p adds (30, 40); g is parentless and
    // contributes nothing → (1 + 5 + 30, 2 + 6 + 40).
    assert_eq!(env.input.take_warp_request(), Some((36.0, 48.0)));
    assert!(env.try_run("c.setCursorPos(1);").is_err());
}

/// Reference: `releaseCapture()` ignores which layer captures and releases
/// the mouse capture globally (`reference/cpp/core/visual/LayerIntf.cpp:3467-3472`);
/// `releaseTouchCapture` with **no** argument releases every touch capture
/// (`ReleaseTouchCapture(0, true)` → `ReleaseTouchCaptureAll`,
/// `LayerIntf.cpp:9717-9719`) while a present argument selects the contact
/// id via `static_cast<tjs_uint32>(param[0]->AsInteger())`
/// (`LayerIntf.cpp:9714-9716`) — a `void` argument therefore means contact
/// `0` and negative values truncate to 32 bits. The queue drained by
/// `take_capture_releases` is the port's model of those two manager calls
/// (`None` = "all contacts").
#[test]
fn capture_release_members_queue_reference_shaped_requests() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.releaseCapture(); \
         l.releaseTouchCapture(5); \
         l.releaseTouchCapture(); \
         l.releaseTouchCapture(-1);",
    );
    let layer = env.scene.layers.first().expect("one layer");
    let ctx = env._host.ctx().expect("bound host");
    let (mouse, touch) = compat::take_capture_releases(&ctx);
    assert_eq!(mouse, vec![layer.id]);
    assert_eq!(
        touch,
        vec![
            (layer.id, Some(5)),
            (layer.id, None),              // no argument → release all
            (layer.id, Some(0xffff_ffff)), // (-1 as tjs_uint32)
        ]
    );
}

// ---------------------------------------------------------------------------
// 5. Sibling order and modal mode
// ---------------------------------------------------------------------------

/// Reference: `MoveBefore(lay)` places `this` directly **in front of** the
/// sibling and `MoveBehind(lay)` directly **behind** it — the two
/// `ChildChangeOrder` rotations of `LayerIntf.cpp:1356-1397` end `this` at
/// the front-side / back-side neighbour slot of `lay`
/// (`ChildChangeOrder`, `LayerIntf.cpp:1294-1330`). Starting from `[a, b,
/// c]`: `c.moveBefore(a)` → `[a, c, b]`; `b.moveBehind(c)` → `[a, b, c]`;
/// `b.moveBefore(c)` → `[a, c, b]`.
#[test]
fn move_before_and_move_behind_follow_child_change_order() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.name = 'a'; \
         var b = new Layer(w, null); b.name = 'b'; \
         var c = new Layer(w, null); c.name = 'c'; \
         c.moveBefore(a);",
    );
    assert_eq!(window_layer_names(&env), ["a", "c", "b"]);
    env.run("b.moveBehind(c);");
    assert_eq!(window_layer_names(&env), ["a", "b", "c"]);
    env.run("b.moveBefore(c);");
    assert_eq!(window_layer_names(&env), ["a", "c", "b"]);
}

/// Reference: the reordering keeps the sibling sort key consistent — the
/// port renumbers every sibling's `z_order` to its new back-to-front
/// index, matching the reference's order-index positions
/// (`reference/cpp/core/visual/LayerIntf.cpp:1294-1330` plus
/// `SetOrderIndex`'s index == position at `LayerIntf.cpp:1399-1411`).
#[test]
fn reordering_renumbers_the_sibling_z_order() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.name = 'a'; \
         var b = new Layer(w, null); b.name = 'b'; \
         var c = new Layer(w, null); c.name = 'c'; \
         c.moveBefore(a);",
    );
    let window = env.scene.windows.first().expect("one window");
    let order: Vec<(String, i32)> = window
        .layers
        .iter()
        .map(|id| {
            let layer = env.scene.layer(*id).expect("layer");
            (layer.name.clone(), layer.z_order)
        })
        .collect();
    assert_eq!(
        order,
        vec![
            ("a".to_string(), 0),
            ("c".to_string(), 1),
            ("b".to_string(), 2)
        ]
    );
}

/// Reference: `CheckZOrderMoveRule` throws
/// `TVPCannotMoveNextToSelfOrNotSiblings` when the target is the layer
/// itself (`reference/cpp/core/visual/LayerIntf.cpp:1282-1291`) — a
/// self-move is an **error**, not a silent no-op — and the `moveBefore` /
/// `moveBehind` natives throw `TVPSpecifyLayer` for a non-Layer argument
/// (`LayerIntf.cpp:8341-8374`).
#[test]
fn moving_a_layer_next_to_itself_or_a_non_layer_raises() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.name = 'a'; \
         var b = new Layer(w, null); b.name = 'b'; \
         var self_ok = 1; \
         try { b.moveBefore(b); self_ok = 0; } catch (e) {} \
         var nonlayer_ok = 1; \
         try { b.moveBehind(42); nonlayer_ok = 0; } catch (e) {}",
    );
    env.assert_script("self_ok == 1");
    env.assert_script("nonlayer_ok == 1");
    // The failed moves must not have disturbed the sibling order.
    assert_eq!(window_layer_names(&env), ["a", "b"]);
}

/// Reference: `BringToBack` moves the layer to order index 0 — the most
/// back position (`reference/cpp/core/visual/LayerIntf.cpp:1413-1421`).
#[test]
fn bring_to_back_puts_the_layer_first() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.name = 'a'; \
         var b = new Layer(w, null); b.name = 'b'; \
         var c = new Layer(w, null); c.name = 'c'; \
         c.bringToBack();",
    );
    assert_eq!(window_layer_names(&env), ["c", "a", "b"]);
}

/// Reference `SetModeTo` (`reference/cpp/core/visual/LayerManager.cpp:834-867`):
/// * an ancestor-or-self of the *current* modal layer throws
///   (`LayerManager.cpp:840-844` — which also rejects re-setting the
///   current modal itself);
/// * an invisible layer is forced visible first (`847-848`), then a
///   disabled layer throws (`849-850`);
/// * focus moves to the **first focusable node of the new modal's
///   subtree** (`SetFocusTo(layer->SearchFirstFocusable(), true)`,
///   `LayerManager.cpp:855`), so a non-focusable modal parent focuses its
///   focusable child.
#[test]
fn set_mode_follows_the_modal_stack_rules() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var g = new Layer(w, null); g.name = 'g'; \
         var p = new Layer(w, g); p.name = 'p'; \
         var ch = new Layer(w, p); ch.name = 'ch'; \
         ch.focusable = true; ch.visible = true; \
         p.setMode(); \
         var focus_is_child = (ch == w.focusedLayer); \
         var again_ok = 1; \
         try { p.setMode(); again_ok = 0; } catch (e) {} \
         var ancestor_ok = 1; \
         try { g.setMode(); ancestor_ok = 0; } catch (e) {}",
    );
    env.assert_script("focus_is_child");
    env.assert_script("again_ok == 1"); // already the top modal → error
    env.assert_script("ancestor_ok == 1"); // ancestor of the modal → error
}

/// Reference: `SetModeTo` throws `TVPCannotSetModeToDisabledOrModal` for a
/// disabled layer (`reference/cpp/core/visual/LayerManager.cpp:849-850`)
/// after forcing an invisible layer visible (`847-848`).
#[test]
fn set_mode_rejects_disabled_layers_but_forces_invisible_ones() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var hidden = new Layer(w, null); \
         hidden.setMode(); \
         var forced = hidden.visible; \
         var dis = new Layer(w, null); \
         dis.enabled = false; \
         var disabled_ok = 1; \
         try { dis.setMode(); disabled_ok = 0; } catch (e) {}",
    );
    env.assert_script("forced == 1"); // invisible → forced visible
    env.assert_script("disabled_ok == 1"); // disabled → error
}

/// Reference `RemoveModeFrom` (`reference/cpp/core/visual/LayerManager.cpp:869-890`):
/// removing a modal layer hands focus to `layer->GetNextFocusable()`
/// (`880`) — the next focusable layer after the removed one, skipping
/// non-focusable nodes — or clears focus when none remains
/// (`SetFocusTo(nullptr)`, `876-884`); removing a non-modal layer is a
/// silent no-op.
#[test]
fn remove_mode_hands_focus_to_the_next_focusable() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var p = new Layer(w, null); p.name = 'p'; \
         var ch = new Layer(w, p); ch.name = 'ch'; \
         ch.focusable = true; ch.visible = true; \
         var other = new Layer(w, null); other.name = 'other'; \
         other.focusable = true; other.visible = true; \
         p.setMode(); \
         ch.focusable = false; \
         p.removeMode(); \
         var focus_after = (w.focusedLayer == other);",
    );
    env.assert_script("focus_after");
}

/// Reference: `RemoveModeFrom` iterates the modal vector and only touches
/// focus for entries it actually erases
/// (`reference/cpp/core/visual/LayerManager.cpp:869-890`) — a non-modal
/// layer removes silently.
#[test]
fn remove_mode_is_silent_when_not_modal() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.removeMode(); \
         var survived = 1;",
    );
    env.assert_script("survived == 1");
}

// ---------------------------------------------------------------------------
// 6. Pixel / image operations
// ---------------------------------------------------------------------------

/// Reference natives (all raise `TJS_E_BADPARAMCOUNT` when short):
/// `colorRect` 5 (`LayerIntf.cpp:8672-8677`), `copyRect`/`pileRect`/
/// `blendRect`/`piledCopy` 7 (`8779-8784`, `8822-8827`, `8858-8863`,
/// `8752-8757`), `operateRect`/`operateStretch` 7/9 (`8941-8946`,
/// `9154-9159`), `stretchCopy` 9 (`9002-9007`), `affine*` 12 (`9227-9232`,
/// `9299-9304`, `9361-9366`, `9423-9428`), `copy9Patch` 1 (`8894-8898`),
/// `convertType` 1 (`9648-9653`), `getMainPixel`/`getMaskPixel` 2
/// (`8457-8462`, `8478-8483`), `setMainPixel`/`setMaskPixel` 3 (`8468-8472`,
/// `8489-8493`), `light` 2 (`8302-8309`), `gaussianBlur` 1 (`9565-9570`).
#[test]
fn pixel_methods_enforce_reference_argument_counts() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); l.setImageSize(2, 2); \
         var s = new Layer(w, null); s.setImageSize(2, 2);",
    );
    for call in [
        "l.colorRect(0, 0, 1, 1)",
        "l.copyRect(0, 0, s, 0, 0, 1)",
        "l.pileRect(0, 0, s, 0, 0, 1)",
        "l.blendRect(0, 0, s, 0, 0, 1)",
        "l.piledCopy(0, 0, s, 0, 0, 1)",
        "l.operateRect(0, 0, s, 0, 0, 1)",
        "l.operateStretch(0, 0, 1, 1, s, 0, 0, 1)",
        "l.stretchCopy(0, 0, 1, 1, s, 0, 0, 1)",
        "l.affineCopy(s, 0, 0, 1, 1, false, 0, 0, 1, 0, 0)",
        "l.copy9Patch()",
        "l.convertType()",
        "l.getMainPixel(0)",
        "l.setMainPixel(0, 0)",
        "l.getMaskPixel(0)",
        "l.setMaskPixel(0, 0)",
        "l.light(1)",
        "l.gaussianBlur()",
    ] {
        assert!(
            env.try_run(&format!("var r = {call};")).is_err(),
            "{call} must raise like the reference"
        );
    }
}

/// Reference: `colorRect` defaults `opa` to 255
/// (`reference/cpp/core/visual/LayerIntf.cpp:8677-8684`); on `dfAlpha` a
/// positive opacity fills colour *and* alpha while a negative opacity only
/// removes const opacity, leaving the colour untouched
/// (`ColorRect`, `LayerIntf.cpp:4352-4362`). `TVPRemoveConstOpacity_c`
/// multiplies alpha by `(255 - strength)` (`reference/cpp/core/visual/tvpgl.cpp:6401-6413`),
/// so `255 → 199` for `strength = 55`.
#[test]
fn color_rect_opacity_default_and_negative_removal() {
    let _lock = vm_lock();
    let env = Env::new();
    layer_with_image(&env, 2, 2);
    env.run(
        "l.colorRect(0, 0, 2, 2, 0xff102030); \
         var a_full = l.getMaskPixel(0, 0); \
         l.colorRect(0, 0, 2, 2, 0xff405060, -55); \
         var a_removed = l.getMaskPixel(0, 0); \
         var rgb_after = l.getMainPixel(0, 0);",
    );
    assert_eq!(env.eval_int("a_full"), 255);
    assert_eq!(env.eval_int("a_removed"), 199);
    // Negative opacity does not repaint the colour.
    assert_eq!(env.eval_int("rgb_after"), 0x102030);
}

/// Reference: `ColorRect` on `dfAddAlpha` throws
/// `TVPNegativeOpacityNotSupportedOnThisFace` for `opa < 0`
/// (`reference/cpp/core/visual/LayerIntf.cpp:4367-4372`).
#[test]
fn color_rect_negative_opacity_on_add_alpha_raises() {
    let _lock = vm_lock();
    let env = Env::new();
    layer_with_image(&env, 1, 1);
    env.run("l.face = 4;");
    assert!(
        env.try_run("l.colorRect(0, 0, 1, 1, 0xff102030, -1);")
            .is_err()
    );
}

/// Reference: the `gaussianBlur` native derives
/// `sigma = max(1.0f, radius / 2.5f)` **only when the second argument is
/// missing**; a present but `void` argument keeps the `1.0f` default
/// (`reference/cpp/core/visual/LayerIntf.cpp:9575-9597`). A non-positive
/// radius or sigma raises (`LayerIntf.cpp:9599-9606`).
#[test]
fn gaussian_blur_sigma_defaults_distinguish_void_from_missing() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "function seed(l) { \
             for (var y = 0; y < 5; y++) \
                 for (var x = 0; x < 5; x++) \
                     l.setMainPixel(x, y, ((x + y) % 2) ? 0xffffff : 0x000000); \
         } \
         function sig(l) { \
             var s = ''; \
             for (var i = 0; i < 25; i++) s += ',' + l.getMainPixel(i % 5, ((i / 5) | 0)); \
             return s; \
         } \
         function noret() {} \
         var w = new Window(); \
         var l = new Layer(w, null); l.setImageSize(5, 5); \
         seed(l); var base = sig(l); \
         l.gaussianBlur(5); var dflt = sig(l); \
         seed(l); l.gaussianBlur(5, 2.0); var expl = sig(l); \
         seed(l); l.gaussianBlur(5, noret()); var voided = sig(l); \
         var changed = (base != dflt); \
         var default_is_2 = (dflt == expl); \
         var void_is_1 = (dflt != voided);",
    );
    env.assert_script("changed");
    env.assert_script("default_is_2");
    env.assert_script("void_is_1");
    assert!(env.try_run("l.gaussianBlur(0);").is_err());
    assert!(env.try_run("l.gaussianBlur(1, -0.5);").is_err());
}

/// Reference: `adjustGamma` with **zero** arguments returns `TJS_S_OK`
/// without touching the image
/// (`reference/cpp/core/visual/LayerIntf.cpp:9529-9535`), and each gamma
/// argument is read with `AsReal`, which *parses strings*
/// (`tjsVariant.h:937-956`); anything unparsable becomes `0.0`.
#[test]
fn adjust_gamma_parses_string_reals_and_accepts_no_arguments() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "function fill(l) { l.colorRect(0, 0, 2, 2, 0xff808080); } \
         function sig(l) { \
             var s = ''; \
             for (var i = 0; i < 4; i++) s += ',' + l.getMainPixel(i % 2, (i / 2) | 0); \
             return s; \
         } \
         var w = new Window(); \
         var l = new Layer(w, null); l.setImageSize(2, 2); \
         fill(l); var plain = sig(l); \
         l.adjustGamma(); var noop = sig(l); \
         l.adjustGamma('1.5', 0, 255, 1.0, 0, 255, 1.0, 0, 255); var from_str = sig(l); \
         fill(l); l.adjustGamma(1.5, 0, 255, 1.0, 0, 255, 1.0, 0, 255); var from_real = sig(l); \
         fill(l); l.adjustGamma('junk', 0, 255, 1.0, 0, 255, 1.0, 0, 255); var junk = sig(l); \
         var noop_ok = (plain == noop); \
         var str_ok = (from_str == from_real); \
         var junk_differs = (from_str != junk);",
    );
    env.assert_script("noop_ok");
    env.assert_script("str_ok");
    env.assert_script("junk_differs");
}

/// Reference: `getMainPixel` masks the alpha plane off
/// (`& 0xffffff`, `reference/cpp/core/visual/LayerIntf.cpp:2917-2923`) and
/// writes a `tjs_int64` — an **Integer** (`LayerIntf.cpp:8457-8466`);
/// `setMainPixel` writes only the RGB planes through `SetPointMain`
/// (`LayerIntf.cpp:2925-2943`), so the mask plane survives.
#[test]
fn pixel_accessors_mask_alpha_and_hold_the_mask_plane() {
    let _lock = vm_lock();
    let env = Env::new();
    layer_with_image(&env, 1, 1);
    env.run(
        "l.setMainPixel(0, 0, 0xffabcdef); \
         l.setMaskPixel(0, 0, 77); \
         var rgb = l.getMainPixel(0, 0); \
         var alpha = l.getMaskPixel(0, 0); \
         l.setMainPixel(0, 0, 0xff00ff00); \
         var alpha_after = l.getMaskPixel(0, 0); \
         var rgb_after = l.getMainPixel(0, 0);",
    );
    assert_eq!(env.eval("rgb").expect("rgb"), TjsValue::Integer(0xab_cdef));
    assert_eq!(env.eval_int("alpha"), 77);
    assert_eq!(env.eval_int("alpha_after"), 77);
    // 0xff00ff00 is ARGB for green: r = 0, g = 255, b = 0.
    assert_eq!(env.eval_int("rgb_after"), 0x00ff00);
}

/// Reference: `SetPointMask` writes `(mask & 0xff) << 24` into the alpha
/// byte (`reference/cpp/core/visual/LayerBitmapIntf.cpp:210-220`), and the
/// `setMaskPixel` native passes the raw `tjs_int` (`LayerIntf.cpp:8489-8493`)
/// to `tTVSNI_BaseLayer::SetMaskPixel` (`LayerIntf.cpp:2955-2961`). The
/// script value is therefore truncated to its low eight bits — `256` reads
/// back `0`, `-1` reads back `255` — not clamped.
#[test]
fn set_mask_pixel_truncates_to_eight_bits() {
    let _lock = vm_lock();
    let env = Env::new();
    layer_with_image(&env, 1, 1);
    env.run(
        "l.setMaskPixel(0, 0, 256); var wrap = l.getMaskPixel(0, 0); \
         l.setMaskPixel(0, 0, -1); var neg = l.getMaskPixel(0, 0); \
         l.setMaskPixel(0, 0, 300); var high = l.getMaskPixel(0, 0);",
    );
    assert_eq!(env.eval_int("wrap"), 0);
    assert_eq!(env.eval_int("neg"), 255);
    assert_eq!(env.eval_int("high"), 44); // 300 & 0xff
}

/// Reference: `pileRect`, `blendRect`, `piledCopy`, `stretchPile`,
/// `stretchBlend`, `affinePile` and `affineBlend` accept **only** a Layer
/// source (`TVPSpecifyLayer`,
/// `reference/cpp/core/visual/LayerIntf.cpp:8830-8841`, `8866-8877`,
/// `8760-8771`, `9065-9078`, `9112-9125`, `9307-9322`, `9369-9384`),
/// while `copyRect`, `copy9Patch`, `stretchCopy`, `operateRect`,
/// `operateStretch`, `affineCopy` and `operateAffine` also accept a
/// `Bitmap` (`TVPSpecifyLayerOrBitmap`,
/// `LayerIntf.cpp:8793-8811`, `8908-8930`, `9016-9035`, `8953-8982`,
/// `9168-9196`, `9235-9259`, `9435-9463`).
#[test]
fn layer_only_members_reject_a_bitmap_source() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); l.setImageSize(2, 2); \
         var b = new Bitmap(2, 2); \
         var pile = 1; \
         try { l.pileRect(0, 0, b, 0, 0, 2, 2); pile = 0; } catch (e) {} \
         var blend = 1; \
         try { l.blendRect(0, 0, b, 0, 0, 2, 2); blend = 0; } catch (e) {} \
         var copied = 1; \
         try { l.piledCopy(0, 0, b, 0, 0, 2, 2); copied = 0; } catch (e) {} \
         var spile = 1; \
         try { l.stretchPile(0, 0, 2, 2, b, 0, 0, 2, 2); spile = 0; } catch (e) {} \
         var sblend = 1; \
         try { l.stretchBlend(0, 0, 2, 2, b, 0, 0, 2, 2); sblend = 0; } catch (e) {} \
         var apile = 1; \
         try { l.affinePile(b, 0, 0, 2, 2, false, 0, 0, 1, 0, 0, 1); apile = 0; } catch (e) {} \
         var ablend = 1; \
         try { l.affineBlend(b, 0, 0, 2, 2, false, 0, 0, 1, 0, 0, 1); ablend = 0; } catch (e) {} \
         l.copyRect(0, 0, b, 0, 0, 2, 2); \
         l.copy9Patch(b); \
         l.stretchCopy(0, 0, 2, 2, b, 0, 0, 2, 2); \
         l.operateRect(0, 0, b, 0, 0, 2, 2);",
    );
    for flag in [
        "pile", "blend", "copied", "spile", "sblend", "apile", "ablend",
    ] {
        env.assert_script(&format!("{flag} == 1"));
    }
}

/// Reference: with `omAuto` (the default) `operateRect` replaces the mode
/// by the source layer's `GetOperationModeFromType()`
/// (`reference/cpp/core/visual/LayerIntf.cpp:8996-8999`), which shares the
/// `tTVPLayerType` numbering (`LayerIntf.h:76-105`,
/// `LayerIntf.cpp:1621-1686`) — `ltAdditive` → `omAdditive` (3). Against a
/// non-black destination, additive and alpha differ, so the auto mode is
/// observable.
#[test]
fn operate_rect_om_auto_uses_the_source_layer_type() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "function seed(dst, src) { \
             dst.colorRect(0, 0, 2, 2, 0xff640000, 255); \
             src.colorRect(0, 0, 2, 2, 0xffc80000, 128); \
         } \
         var w = new Window(); \
         var dst = new Layer(w, null); dst.setImageSize(2, 2); \
         var src = new Layer(w, null); src.setImageSize(2, 2); \
         src.type = 3; \
         seed(dst, src); \
         dst.operateRect(0, 0, src, 0, 0, 2, 2); var auto = dst.getMainPixel(0, 0); \
         seed(dst, src); \
         dst.operateRect(0, 0, src, 0, 0, 2, 2, 3); var expl = dst.getMainPixel(0, 0); \
         seed(dst, src); \
         dst.operateRect(0, 0, src, 0, 0, 2, 2, 2); var alpha = dst.getMainPixel(0, 0);",
    );
    // omAdditive: 100 + 200 saturates to 255; omAlpha would blend to ~150.
    assert_eq!(env.eval_int("auto"), 0xff_0000);
    assert_eq!(env.eval_int("expl"), 0xff_0000);
    assert!(env.eval_int("alpha") < 0xff_0000);
}

/// Reference: `GetBltMethodFromOperationModeAndDrawFace` only knows the
/// `omAlpha` / `omAddAlpha` / `omOpaque` modes on the `dfAlpha` /
/// `dfAddAlpha` / `dfOpaque` faces and reports failure —
/// `TVPNotDrawableFaceType` — otherwise
/// (`reference/cpp/core/visual/LayerIntf.cpp:4118-4271`), while the
/// face-independent modes (e.g. `omAdditive` = 3) accept any face. The
/// check runs after the clip intersection (`OperateRect`,
/// `LayerIntf.cpp:5232-5243`).
#[test]
fn operate_rect_validates_the_mode_face_pair() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var dst = new Layer(w, null); dst.setImageSize(2, 2); dst.face = 2; \
         var src = new Layer(w, null); src.setImageSize(2, 2);",
    );
    // omAuto resolves to omAlpha (source type 2) on a dfMask destination.
    assert!(
        env.try_run("dst.operateRect(0, 0, src, 0, 0, 2, 2);")
            .is_err()
    );
    assert!(
        env.try_run("dst.operateRect(0, 0, src, 0, 0, 2, 2, 2);")
            .is_err()
    );
    assert!(
        env.try_run("dst.operateRect(0, 0, src, 0, 0, 2, 2, 1);")
            .is_err()
    );
    // The face-independent omAdditive (3) is accepted on every face.
    env.run("dst.operateRect(0, 0, src, 0, 0, 2, 2, 3);");
}

/// Reference face gates: `StretchCopy` throws `TVPNotDrawableFaceType`
/// outside `dfAlpha`/`dfAddAlpha`/`dfOpaque`
/// (`reference/cpp/core/visual/LayerIntf.cpp:4672-4710`), and `PileRect`
/// requires `dfAlpha`/`dfOpaque` (`LayerIntf.cpp:5112-5125`).
#[test]
fn stretch_and_pile_validate_the_destination_face() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var dst = new Layer(w, null); dst.setImageSize(2, 2); dst.face = 2; \
         var src = new Layer(w, null); src.setImageSize(2, 2);",
    );
    assert!(
        env.try_run("dst.stretchCopy(0, 0, 2, 2, src, 0, 0, 2, 2);")
            .is_err()
    );
    assert!(env.try_run("dst.pileRect(0, 0, src, 0, 0, 2, 2);").is_err());
}

/// Reference: `ColorRect`'s switch has no `default:` case — a face outside
/// the vocabulary falls through **silently** (the `dfAuto` arm is an empty
/// `break`, `reference/cpp/core/visual/LayerIntf.cpp:4348-4403`), as does
/// `CopyRect` (`LayerIntf.cpp:4586-4650`).
#[test]
fn color_rect_on_an_out_of_vocabulary_face_is_silent() {
    let _lock = vm_lock();
    let env = Env::new();
    layer_with_image(&env, 1, 1);
    env.run(
        "l.colorRect(0, 0, 1, 1, 0xff304050); var before = l.getMainPixel(0, 0); \
         l.face = 7; \
         l.colorRect(0, 0, 1, 1, 0xffffffff); \
         var after = l.getMainPixel(0, 0);",
    );
    assert_eq!(env.eval_int("before"), 0x304050);
    assert_eq!(env.eval_int("after"), 0x304050);
}

/// Reference: with `face == dfAuto` (128, the reference default
/// `LayerIntf.cpp:371`) the effective face is derived from the layer type
/// (`UpdateDrawFace`, `LayerIntf.cpp:1525-1620`): `ltAlpha` → `dfAlpha`
/// (a `ColorRect` fill writes the alpha plane, `LayerIntf.cpp:4352-4358`)
/// while `ltOpaque` → `dfOpaque` (a fill *holds* the destination alpha,
/// `LayerIntf.cpp:4374-4381`).
#[test]
fn df_auto_face_resolves_from_the_layer_type() {
    let _lock = vm_lock();
    let env = Env::new();
    layer_with_image(&env, 1, 1);
    env.run(
        "l.setMaskPixel(0, 0, 100); \
         l.face = 128; \
         l.type = 2; \
         l.colorRect(0, 0, 1, 1, 0xff102030); \
         var mask_alpha = l.getMaskPixel(0, 0); \
         l.setMaskPixel(0, 0, 100); \
         l.type = 1; \
         l.colorRect(0, 0, 1, 1, 0xff102030); \
         var mask_opaque = l.getMaskPixel(0, 0);",
    );
    // dfAlpha fill writes alpha = 255; dfOpaque holds the old alpha.
    assert_eq!(env.eval_int("mask_alpha"), 255);
    assert_eq!(env.eval_int("mask_opaque"), 100);
}

/// Reference: `gaussianBlur` (`ApplyGaussianBlur`,
/// `reference/cpp/core/visual/LayerIntf.cpp:5749-5760`) and `light`
/// (`ApplyLightContrast`, `LayerIntf.cpp:1006-1025`) log and return
/// **without an error** when the layer has no image to operate on.
#[test]
fn blur_and_light_are_silent_on_an_imageless_layer() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.gaussianBlur(1); \
         l.light(10, 10); \
         var survived = 1;",
    );
    env.assert_script("survived == 1");
}

/// Reference: `doGrayScale` (`LayerIntf.cpp:9616-9624`) and the two flips
/// (`9626-9647`) take no arguments and always succeed on a drawable layer.
#[test]
fn grayscale_and_flips_take_no_arguments() {
    let _lock = vm_lock();
    let env = Env::new();
    layer_with_image(&env, 2, 2);
    env.run(
        "l.setMainPixel(0, 0, 0xff306090); \
         l.doGrayScale(); \
         var gray = l.getMainPixel(0, 0); \
         l.setMainPixel(0, 0, 0xff0000ff); \
         l.flipLR(); \
         var flipped = l.getMainPixel(1, 0);",
    );
    // Grayscale of (0x30, 0x60, 0x90) is not the original colour, and the
    // left-right flip moved the pixel to x = 1.
    assert!(env.eval_int("gray") != 0x306090);
    assert_eq!(env.eval_int("flipped"), 0x0000ff);
}

// ---------------------------------------------------------------------------
// `onPaint` dispatch (CallOnPaint / AffineLayer compositing)
// ---------------------------------------------------------------------------

/// Reference: the per-draw `tTJSNI_BaseLayer::BeforeCompletion` fires the
/// layer's `onPaint` event when `CallOnPaint` is set, clearing the flag
/// **before** posting the event
/// (`reference/cpp/core/visual/LayerIntf.cpp:6401-6402`):
///
/// ```cpp
/// if(CallOnPaint) {
///     CallOnPaint = false;
///     TVPPostEvent(Owner, Owner, TJS_W("onPaint"), 0, TVP_EPT_IMMEDIATE, 0, nullptr);
/// }
/// ```
///
/// `Layer.update()` arms that flag (`LayerIntf.h:990-995` `UpdateByScript`,
/// `LayerIntf.cpp:9667-9671`). The frame body must therefore dispatch
/// `onPaint` through the object before drawing. This is what makes every
/// `Sprite`/`AffineLayer` render: `AffineLayer.onPaint` copies the hidden
/// inner `_image` layer onto the visible outer layer
/// (`system/affinelayer.tjs:142-144`), and nothing else composites it, so a
/// missing dispatch leaves the outer layer an empty 1x1 image (the intro
/// logo white screen).
#[test]
fn layer_update_dispatches_on_paint_before_drawing() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var paints = 0; \
         class PaintLayer extends Layer { \
             function PaintLayer(w, parent) { super.Layer(w, parent); } \
             function onPaint() { paints++; } \
         } \
         var w = new Window(); \
         var p = new PaintLayer(w, null); \
         p.update();",
    );
    assert_eq!(
        env.eval_int("paints"),
        0,
        "update() only arms CallOnPaint; onPaint runs on the next pre-draw step"
    );

    // The frame body's pre-draw step (`project_and_render` ->
    // `compat::advance_transitions` / `compat::paint_poll`) runs the pending
    // `onPaint` before anything is rendered.
    let ctx = env._host.ctx().expect("bound host");
    compat::advance_transitions(&ctx, 0.0).expect("advance transitions");
    assert_eq!(
        env.eval_int("paints"),
        1,
        "a layer whose update() armed CallOnPaint must get one onPaint dispatch"
    );

    // The reference clears `CallOnPaint` before posting, so a frame with no
    // new `update()` dispatches nothing.
    compat::advance_transitions(&ctx, 0.0).expect("advance transitions");
    assert_eq!(
        env.eval_int("paints"),
        1,
        "the flag is cleared before the call, so onPaint fires once per update()"
    );

    // A re-armed flag dispatches again on the following frame.
    env.run("p.update();");
    compat::advance_transitions(&ctx, 0.0).expect("advance transitions");
    assert_eq!(
        env.eval_int("paints"),
        2,
        "a second update() re-arms the next frame's dispatch"
    );
}

/// `with(member){ .visible = true }` writes the **member**, never the
/// enclosing object, and that holds whether the enclosing function runs
/// directly or from a `Timer` action
/// (`reference/tests/test_files/tjs2/test_with.tjs`; the with-object is the
/// write receiver, `tTJSVariantClosure::PropSet` prefers `ObjThis`,
/// `reference/cpp/core/tjs2/tjsVariant.h:261-269`).
///
/// The game's `OnceTimer` shape (`super.Timer(owner, "")` + a script
/// `onTimer` that calls `_owner()`, `system/Utility.tjs`) is reproduced so
/// the timer-invoked path is the one under test.
///
/// The child layer is built through `global.Layer`: inside a method of a
/// `Layer` subclass an unqualified `Layer` is compiled as `this.Layer`
/// (`reference/cpp/core/tjs2/tjsInterCodeGen.cpp:2321-2335`) and the native
/// constructor is a non-static instance member (`reference/cpp/core/tjs2/
/// tjsNative.h:400-408`, `reference/cpp/core/visual/LayerIntf.cpp:8286-
/// 8291`), whose `CreateNew` is `tTJSObject::CreateNew` -> `TJS_E_NOTIMPL`
/// (`reference/cpp/core/tjs2/tjsObject.h:187-190`). `global.Layer` names
/// the class object, which does implement `CreateNew`.
///
/// The enclosing `Holder` is the window's first layer, which `AttachPrimary`
/// forces visible (`reference/cpp/core/visual/LayerManager.cpp:161-172`), so
/// the test hides it before the write under test.
#[test]
fn with_member_property_write_reaches_the_member_from_a_timer() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var fired = 0; \
         class Holder extends Layer { \
             var child = null; \
             function Holder(w, parent) { super.Layer(w, parent); child = new global.Layer(w, this); } \
             function mark() { with(child) { .visible = true; } fired++; } \
         } \
         class OnceTimer extends Timer { \
             var _owner = null; \
             function OnceTimer(owner, time) { \
                 super.Timer(owner, ''); \
                 capacity = 1; \
                 interval = time; \
                 _owner = owner; \
             } \
             function start() { enabled = true; } \
             function cancel() { enabled = false; } \
             function onTimer() { cancel(); if (isvalid _owner) _owner(); } \
         } \
         var w = new Window(); \
         var h = new Holder(w, null); \
         h.visible = false; \
         h.mark();",
    );
    assert_eq!(
        env.eval_int("h.child.visible"),
        1,
        "the direct with(member) write must reach the member"
    );
    assert_eq!(
        env.eval_int("h.visible"),
        0,
        "the direct with(member) write must not touch the enclosing object"
    );

    env.run(
        "h.child.visible = false; \
         h.visible = false; \
         fired = 0; \
         var ot = new OnceTimer(h.mark, 1); \
         ot.start();",
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while env.eval_int("fired") == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
        let mut ctx = env._host.ctx().expect("bound host");
        compat::timer_poll(&mut ctx).expect("timer poll");
    }
    assert_eq!(env.eval_int("fired"), 1, "the timer action must run");
    assert_eq!(
        env.eval_int("h.child.visible"),
        1,
        "the timer-invoked with(member) write must reach the member"
    );
    assert_eq!(
        env.eval_int("h.visible"),
        0,
        "the timer-invoked with(member) write must not touch the enclosing object"
    );
}

/// Reference: `Layer.assignImages(srcLayer)` resolves the argument through
/// the Layer native instance, raising `TVPSpecifyLayer` for a non-object,
/// `null`, or a non-Layer object (`LayerIntf.cpp:9860-9882`), then
/// `tTJSNI_BaseLayer::AssignImages` copies the source's MainImage and
/// ProvinceImage into ours — assigning a present image, deallocating ours
/// when the source has none (`LayerIntf.cpp:2394-2432`).
#[test]
fn assign_images_copies_a_layer_and_deallocates_on_an_imageless_source() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); \
         var dst = new Layer(w, null); \
         src.setImageSize(2, 2); \
         src.setMainPixel(0, 0, 0x123456); \
         dst.setImageSize(3, 3); \
         dst.assignImages(src);",
    );
    // `AssignImages` gives the target the source's MainImage.
    assert_eq!(
        env.eval_int("dst.hasImage"),
        1,
        "assignImages must give the target an image"
    );
    assert_eq!(
        env.eval_int("dst.getMainPixel(0, 0) == src.getMainPixel(0, 0)"),
        1,
        "assignImages must copy the source MainImage pixels"
    );
    assert_eq!(
        env.eval_int("dst.getMainPixel(0, 0)"),
        0x123456,
        "the copied pixel is the source pixel"
    );

    // A source with no MainImage deallocates the target's image
    // (`DeallocateImage`, LayerIntf.cpp:2349-2356).
    env.run("var empty = new Layer(w, null); dst.assignImages(empty);");
    assert_eq!(
        env.eval_int("dst.hasImage"),
        0,
        "an imageless source must deallocate the target image"
    );

    // The argument is a Layer, not a Bitmap: a non-Layer object raises
    // (`LayerIntf.cpp:9863-9875`).
    assert!(
        env.try_run("dst.assignImages(new Bitmap(1, 1));").is_err(),
        "a Bitmap argument must raise TVPSpecifyLayer"
    );
    assert!(
        env.try_run("dst.assignImages(1);").is_err(),
        "a non-object argument must raise TVPSpecifyLayer"
    );
    assert!(
        env.try_run("dst.assignImages(null);").is_err(),
        "null must raise TVPSpecifyLayer"
    );
    // Missing argument is TJS_E_BADPARAMCOUNT (`LayerIntf.cpp:9860-9861`).
    assert!(
        env.try_run("dst.assignImages();").is_err(),
        "a missing argument must raise BADPARAMCOUNT"
    );
}
