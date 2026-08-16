//! TVP visual native classes: Window, Layer, Bitmap, Font, Timer.
//!
//! Instance natives backed by the logical [`crate::scene::Scene`] (see
//! WAVE3.md). [`register_visual`] wires the scene + storage into the
//! crate-global context and registers every class; the app's update loop
//! calls [`timer_poll`] to fire due timers.

pub mod bitmap;
pub mod ffi;
pub mod font;
pub mod layer;
pub mod timer;
pub mod window;

use std::sync::{Arc, LazyLock, Mutex, RwLock};

use engine::Storage;
use tjs2_sys::Tjs2Engine;

use std::collections::HashMap;
use std::ffi::c_void;

use crate::scene::{BitmapCache, Scene};

static SCENE: LazyLock<Mutex<Option<Arc<RwLock<Scene>>>>> = LazyLock::new(Default::default);
static STORAGE: LazyLock<Mutex<Option<Arc<Mutex<Storage>>>>> = LazyLock::new(Default::default);
static BITMAP_CACHE: LazyLock<Mutex<BitmapCache>> = LazyLock::new(Default::default);
/// layer id -> the layer's TJS object (objthis captured at construction), so
/// `Window.primaryLayer` can return the real Layer object to scripts.
#[derive(Default)]
struct LayerTjsRegistry(Mutex<HashMap<u32, *mut c_void>>);

// SAFETY: the VM is single-threaded; objthis pointers are only dereferenced
// (retained) on that thread.
unsafe impl Send for LayerTjsRegistry {}
unsafe impl Sync for LayerTjsRegistry {}

static LAYER_TJS_OBJECTS: LazyLock<LayerTjsRegistry> = LazyLock::new(Default::default);
static WINDOW_TJS_OBJECTS: LazyLock<LayerTjsRegistry> = LazyLock::new(Default::default);
/// Address of the real `Tjs2Engine` handed to [`register_visual`] (see
/// [`context_engine`]).
static ENGINE: LazyLock<Mutex<Option<EnginePtr>>> = LazyLock::new(Default::default);

/// A raw `Tjs2Engine` pointer stored in a crate-global. `Tjs2Engine` is
/// `Send + Sync` (tjs2-sys), so the pointer shares that contract; it is
/// only ever dereferenced on the VM thread.
#[derive(Clone, Copy)]
struct EnginePtr(*const Tjs2Engine);
// SAFETY: `Tjs2Engine` is `Send + Sync` (tjs2-sys declares it so because
// krkr-rs confines all VM use to a single thread); the pointer is only
// dereferenced on that thread.
unsafe impl Send for EnginePtr {}
unsafe impl Sync for EnginePtr {}

/// The script engine registered by [`register_visual`].
///
/// Native callbacks need the engine to retain timer callbacks, but the
/// callback ABI hands natives the **raw `tjs2_engine*`** (the engine's
/// `inner` field), not a `Tjs2Engine*`: reinterpreting that pointer as a
/// `Tjs2Engine` and reading `.inner` would re-read the C struct's first
/// field (the inner `tTJS*`) and yield a bogus engine. Storing the address
/// of the *real* `Tjs2Engine` (the `&Tjs2Engine` passed to
/// [`register_visual`]) makes the reconstruction sound.
///
/// # Safety
/// The caller of [`register_visual`] must keep the engine alive at a stable
/// address for the whole VM lifetime (the render crate holds it in an
/// `Arc`; tests use `Arc` too), mirroring the `DetachedValue` contract
/// ("the engine MUST outlive every `DetachedValue` it created"). The VM is
/// single-threaded and confined to one engine at a time.
pub(crate) fn context_engine() -> &'static Tjs2Engine {
    let guard = ENGINE.lock().unwrap_or_else(|p| p.into_inner());
    let EnginePtr(ptr) = guard.expect("register_visual: engine context not set");
    drop(guard);
    // SAFETY: `ptr` is the address of the real `Tjs2Engine` from
    // `register_visual`, which the caller keeps alive (see the contract
    // above), and the VM is single-threaded.
    unsafe { &*ptr }
}

/// Lock the shared scene for writing. Panics if [`register_visual`] was not
/// called first (a native fired without context is a programming error).
pub(crate) fn context_scene() -> std::sync::RwLockWriteGuard<'static, Scene> {
    // Clone the Arc out of the registration slot so the returned guard does
    // not borrow the (temporary) outer lock guard.
    let guard = SCENE.lock().unwrap_or_else(|p| p.into_inner());
    let arc = guard
        .as_ref()
        .expect("register_visual: scene context not set")
        .clone();
    drop(guard);
    // SAFETY: `arc` is cloned from the `static` SCENE slot, so it lives for
    // the whole program; extending the guard's borrow to 'static is sound.
    let guard = arc.write().unwrap_or_else(|p| p.into_inner());
    unsafe {
        std::mem::transmute::<
            std::sync::RwLockWriteGuard<'_, Scene>,
            std::sync::RwLockWriteGuard<'static, Scene>,
        >(guard)
    }
}

/// Lock the shared scene for writing (same as [`context_scene`]; kept as a
/// distinct name for call-site clarity).
pub(crate) fn context_scene_mut() -> std::sync::RwLockWriteGuard<'static, Scene> {
    context_scene()
}

/// Lock the shared scene for READING. Read-only natives (property getters,
/// query methods) must use this: taking the write lock in a getter
/// deadlocks when a caller already holds a read lock.
/// Register a layer's TJS object (objthis) for `primaryLayer`.
pub(crate) fn set_layer_tjs_object(id: u32, objthis: *mut c_void) {
    LAYER_TJS_OBJECTS
        .0
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(id, objthis);
}

/// Look up a layer's TJS object by scene id.
pub(crate) fn layer_tjs_object(id: u32) -> *mut c_void {
    LAYER_TJS_OBJECTS
        .0
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&id)
        .copied()
        .unwrap_or(std::ptr::null_mut())
}

/// Register a window's TJS object (objthis) so `Layer.window` can return it.
pub(crate) fn set_window_tjs_object(id: u32, objthis: *mut c_void) {
    WINDOW_TJS_OBJECTS
        .0
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(id, objthis);
}

/// Look up a window's TJS object by scene id.
pub(crate) fn window_tjs_object(id: u32) -> *mut c_void {
    WINDOW_TJS_OBJECTS
        .0
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&id)
        .copied()
        .unwrap_or(std::ptr::null_mut())
}

pub(crate) fn context_scene_read() -> std::sync::RwLockReadGuard<'static, Scene> {
    let guard = SCENE.lock().unwrap_or_else(|p| p.into_inner());
    let arc = guard
        .as_ref()
        .expect("register_visual: scene context not set")
        .clone();
    drop(guard);
    let guard = arc.read().unwrap_or_else(|p| p.into_inner());
    // SAFETY: `arc` is cloned from the `static` SCENE slot.
    unsafe {
        std::mem::transmute::<
            std::sync::RwLockReadGuard<'_, Scene>,
            std::sync::RwLockReadGuard<'static, Scene>,
        >(guard)
    }
}

/// Lock the shared scene AND the storage (for bitmap loads from game files).
pub(crate) fn context_scene_storage() -> (
    std::sync::RwLockWriteGuard<'static, Scene>,
    std::sync::MutexGuard<'static, Storage>,
) {
    let scene = context_scene();
    let guard = STORAGE.lock().unwrap_or_else(|p| p.into_inner());
    let storage_arc = guard
        .as_ref()
        .expect("register_visual: storage context not set")
        .clone();
    drop(guard);
    let storage = storage_arc.lock().unwrap_or_else(|p| p.into_inner());
    // SAFETY: `scene` and `storage_arc` both come from `static` slots.
    (
        unsafe {
            std::mem::transmute::<
                std::sync::RwLockWriteGuard<'_, Scene>,
                std::sync::RwLockWriteGuard<'static, Scene>,
            >(scene)
        },
        unsafe {
            std::mem::transmute::<
                std::sync::MutexGuard<'_, Storage>,
                std::sync::MutexGuard<'static, Storage>,
            >(storage)
        },
    )
}

/// The bitmap-name cache shared by `Bitmap(name)` loads.
pub(crate) fn bitmap_cache() -> std::sync::MutexGuard<'static, BitmapCache> {
    BITMAP_CACHE.lock().unwrap_or_else(|p| p.into_inner())
}

/// Register every visual native class and bind the scene + storage context.
///
/// Must be called once, on the VM thread, before running `startup.tjs`.
pub fn register_visual(
    engine: &Tjs2Engine,
    scene: Arc<RwLock<Scene>>,
    storage: Arc<Mutex<Storage>>,
) -> Result<(), String> {
    *SCENE.lock().unwrap_or_else(|p| p.into_inner()) = Some(scene);
    *STORAGE.lock().unwrap_or_else(|p| p.into_inner()) = Some(storage);
    *ENGINE.lock().unwrap_or_else(|p| p.into_inner()) =
        Some(EnginePtr(engine as *const Tjs2Engine));
    window::register_window(engine)?;
    layer::register_layer(engine)?;
    bitmap::register_bitmap(engine)?;
    font::register_font(engine)?;
    timer::register_timer(engine)?;
    Ok(())
}

/// Fire due timers. The app's update loop calls this with a monotonic
/// millisecond clock. Timer callbacks run synchronously on the VM thread.
pub fn timer_poll(engine: &Tjs2Engine, now_ms: u64) {
    timer::timer_poll(engine, now_ms);
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::{Arc, Mutex, RwLock};

    use tempfile::TempDir;
    use tjs2_sys::{Tjs2Engine, TjsValue};

    use crate::scene::Scene;

    use super::{BITMAP_CACHE, register_visual};

    /// A test harness: fresh engine + scene + a temp game dir mounted as
    /// storage, with all visual natives registered.
    pub struct TestEnv {
        /// The script engine. Held in an `Arc` (like the render crate's
        /// `VmRuntime`) so its address is stable: `register_visual` records
        /// the engine's address and the timer natives call back into it from
        /// native callbacks.
        pub engine: Arc<Tjs2Engine>,
        pub scene: Arc<RwLock<Scene>>,
        /// Holds the Arc registered into the crate-global storage slot so
        /// the mounted dir stays alive for the test's lifetime; reads go
        /// through `write_fixture`'s disk writes + the natives' lookups.
        #[allow(dead_code)]
        pub storage: Arc<Mutex<engine::Storage>>,
        pub _dir: TempDir,
    }

    impl TestEnv {
        pub fn new(_name: &str) -> Self {
            // Tests share the crate-global bitmap cache; reset it so stale
            // ids from a previous test's scene can't be returned.
            BITMAP_CACHE
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .by_name
                .clear();
            let dir = tempfile::tempdir().expect("temp dir");
            let storage =
                engine::Storage::mount(dir.path().to_str().unwrap()).expect("mount temp game dir");
            let storage = Arc::new(Mutex::new(storage));
            let scene = Arc::new(RwLock::new(Scene::default()));
            // Arc so the engine's address is stable while the natives hold
            // a pointer to it (see `TestEnv`'s docs).
            let engine = Arc::new(Tjs2Engine::new().expect("engine"));
            register_visual(&engine, scene.clone(), storage.clone()).expect("register visual");
            TestEnv {
                engine,
                scene,
                storage,
                _dir: dir,
            }
        }

        /// Execute a script (throws on TJS error).
        /// Evaluate an expression (any TJS value).
        pub fn eval(&self, expr: &str, name: &str) -> Result<TjsValue, tjs2_sys::TjsError> {
            self.engine.eval(expr, name)
        }

        pub fn run(&self, script: &str) -> Result<TjsValue, tjs2_sys::TjsError> {
            self.engine.exec_script(script, "test")
        }

        /// Evaluate an expression as an int.
        pub fn eval_int(&self, expr: &str) -> i64 {
            match self.engine.eval(expr, "test") {
                Ok(TjsValue::Integer(i)) => i,
                other => panic!("eval {expr:?} -> {other:?}"),
            }
        }

        /// Evaluate an expression as a string (panics on non-String results).
        pub fn eval_string(&self, expr: &str) -> String {
            match self.engine.eval(expr, "test") {
                Ok(TjsValue::String(s)) => s,
                other => panic!("eval {expr:?} -> {other:?}"),
            }
        }

        pub fn scene(&self) -> std::sync::RwLockReadGuard<'_, Scene> {
            self.scene.read().unwrap_or_else(|p| p.into_inner())
        }
    }

    /// Write a solid-color RGBA image into the env's temp game dir so
    /// storage-backed loads (`new Bitmap(name)`, `layer.loadImages`) find
    /// it. The storage is mounted on `env._dir` and reads lazily from disk,
    /// so files written after mount are visible.
    pub fn write_fixture(env: &TestEnv, name: &str, rgba: &[u8], w: u32, h: u32) {
        let mut bytes = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut bytes)
            .encode(rgba, w, h, image::ExtendedColorType::Rgba8)
            .expect("webp encode");
        std::fs::write(env._dir.path().join(name), bytes).expect("write fixture");
    }
}
