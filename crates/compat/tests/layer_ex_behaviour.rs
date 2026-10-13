//! Reference-behaviour parity tests for the `layerExDraw` plugin family:
//! the GDI+ `GdiPlus` namespace value types
//! (`GdiPlus.PointF`/`GdiPlus.RectF`/`GdiPlus.Font`/`GdiPlus.Appearance`/
//! `GdiPlus.Path`) and the `Layer.perspectiveCopy` / movie members.
//!
//! `LayerExBase` / `LayerExMovie` / `LayerExPerspective` are not TJS classes
//! in the reference (`reference/cpp/plugins/layerExPerspective.cpp:255-296`),
//! so they are absent from the script surface.
//!
//! Every test states the **KiriKiri (reference C++) expectation** with a
//! `reference/cpp/**` citation above the assertion it pins, exactly like
//! `crates/compat/tests/layer_behaviour.rs`. The tests attach a
//! caller-owned L2 [`Scene`] to a [`CompatHost`] and drive the TJS surface —
//! the same seam the app uses — so write side effects are verified against
//! the L2 state the natives actually mutate.
//!
//! The movie *playback* path needs the crate-private decoded-frame source
//! and the per-frame poll, so it is covered by the unit tests in
//! `src/natives/plugins/layer_ex/movie.rs`; here the movie surface, its
//! arities and its callback members are pinned through the VM.
//!
//! Known reference divergences that **cannot** be fixed inside the audited
//! files (documented here so the exclusions are visible):
//! * `Layer.loadRecord` always returns `false` in the reference
//!   (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:1890-1901`
//!   computes the redraw result and then falls through to `return false`),
//!   while `draw.rs` returns `true` on a successful load. The recorded
//!   content still round-trips (pinned below); only the boolean is not
//!   reference-exact.
//! * The reference `RectF.Union` is the *static* `static bool
//!   Union(RectF &ret, const RectF &a, const RectF &b)`
//!   (`reference/cpp/plugins/layerex_draw/windows/main.cpp:210`), registered
//!   with `TJS_STATICMEMBER`, so it is reachable only as
//!   `GdiPlus.RectF.Union`.

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use api::fs::{FsPath, OpenMode};
use api::media::{DecodedImage, ImageInfo};
use common::vm_lock;
use compat::CompatHost;
use engine::{Scene, Storage};
use fs::FileSystem;
use testkit::build_xp3;
use tjs::{Tjs2Engine, TjsValue};

/// Script label for diagnostics.
const SCRIPT: &str = "layer-ex-behaviour";

/// A bound VM + host + attached scene and optional storage.
///
/// Fields drop in declaration order: the host first (releasing every
/// retained script object), then the engine, the scene and the storage.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    scene: Box<Scene>,
    /// Kept alive for the host's raw pointer; `None` means no storage.
    _storage: Option<Box<Storage>>,
}

impl Env {
    fn new() -> Env {
        Env::build(None, false)
    }

    /// Build an env with `storage` attached and, when `codec` is true, the
    /// synthetic image codec the `GdiPlus.Image` tests decode through.
    fn build(storage: Option<Storage>, codec: bool) -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut storage = storage.map(Box::new);
        let mut host = CompatHost::new();
        if codec {
            host.set_image_codec(
                |_bytes, _hint| Ok(DecodedImage::new(2, 2, DECODED_RGBA.to_vec())),
                |_bytes, _hint| {
                    Ok(ImageInfo {
                        width: 2,
                        height: 2,
                        has_alpha: true,
                        format: Some("img".to_string()),
                    })
                },
                |_name, _type, _width, _height, _rgba| Ok(()),
            );
        }
        // SAFETY: the boxed scene and storage outlive the host (they are
        // declared after `_host`, so they drop later).
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
            scene,
            _storage: storage,
        }
    }

    /// Replace the installed image codec with one whose decode closure records
    /// each resolved storage name and returns the shared 2x2 test image.
    fn record_decodes(&self, hints: Arc<Mutex<Vec<String>>>) {
        self._host.set_image_codec(
            move |_bytes, hint| {
                hints
                    .lock()
                    .expect("decode lock")
                    .push(hint.unwrap_or_default().to_string());
                Ok(DecodedImage::new(2, 2, DECODED_RGBA.to_vec()))
            },
            |_bytes, _hint| {
                Ok(ImageInfo {
                    width: 2,
                    height: 2,
                    has_alpha: true,
                    format: Some("img".to_string()),
                })
            },
            |_name, _type, _width, _height, _rgba| Ok(()),
        );
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, SCRIPT)
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    /// Run a script and keep a failure observable instead of panicking (for
    /// cases where the assertion is that the reference *raises*).
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

    fn eval_real(&self, expression: &str) -> f64 {
        match self.eval_ok(expression) {
            TjsValue::Real(value) => value,
            TjsValue::Integer(value) => value as f64,
            other => panic!("{expression:?} returned {other:?}, expected a real"),
        }
    }

    /// The id of the scene layer whose `name` member is `name`.
    fn layer_id(&self, name: &str) -> u32 {
        self.scene
            .layers
            .iter()
            .find(|layer| layer.name == name)
            .unwrap_or_else(|| panic!("no scene layer named {name:?}"))
            .id
    }

    /// The straight-alpha RGBA pixel at `(x, y)` of a layer's MainImage.
    fn pixel(&self, layer_id: u32, x: u32, y: u32) -> [u8; 4] {
        let bitmap_id = self
            .scene
            .layer(layer_id)
            .and_then(|layer| layer.bitmap)
            .expect("layer bitmap");
        let bitmap = self.scene.bitmap(bitmap_id).expect("bitmap");
        let offset = bitmap.pixel_offset(x, y).expect("pixel in range");
        [
            bitmap.rgba[offset],
            bitmap.rgba[offset + 1],
            bitmap.rgba[offset + 2],
            bitmap.rgba[offset + 3],
        ]
    }
}

/// Create a window with two 4x4 layers `src` (filled opaque red) and `dst`
/// (transparent).
fn two_layers(env: &Env) {
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); \
         var dst = new Layer(w, null); \
         src.name = 'src'; \
         dst.name = 'dst'; \
         src.setImageSize(4, 4); \
         dst.setImageSize(4, 4); \
         src.fillRect(0, 0, 4, 4, 0xFFFF0000);",
    );
}

// ---------------------------------------------------------------------------
// GdiPlus.Image / addPrivateFont / getFontList fixtures
// ---------------------------------------------------------------------------

/// The pixels the synthetic image decoder returns: a 2x2 straight-alpha RGBA
/// image.
const DECODED_RGBA: [u8; 16] = [1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255];

/// Write `data` to `path` in an in-memory file system, creating parents.
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

/// An in-memory storage holding `files` (written before the mount).
fn memory_storage(files: &[(&str, &[u8])]) -> Storage {
    let fs = FileSystem::memory();
    for &(path, bytes) in files {
        write_memory_file(&fs, path, bytes);
    }
    Storage::mount_archives(fs, "/", std::iter::empty::<&str>()).expect("mount storage")
}

/// An in-memory storage holding `images/test.img`.
fn image_storage() -> Storage {
    memory_storage(&[("/images/test.img", b"not-a-real-image")])
}

/// An in-memory storage mounting `data.xp3` with one entry, `entry`, whose
/// index entry is valid but whose stored zlib segment is corrupt. Reading the
/// entry is an archive error, not a missing entry, so the in-memory file
/// system alone cannot produce this case.
fn corrupt_archive_storage(entry: &str) -> Storage {
    let mut xp3 = build_xp3(&[(entry, &[0x5a_u8; 256], true)]);
    // The stored segment starts after the 19-byte XP3 header (11-byte magic
    // plus the 8-byte index offset); flipping its zlib header makes the
    // decoder reject the stream.
    for byte in &mut xp3[19..23] {
        *byte ^= 0xff;
    }
    let fs = FileSystem::memory();
    write_memory_file(&fs, "/data.xp3", &xp3);
    Storage::mount_archives(fs, "/", ["data.xp3"]).expect("mount storage")
}

/// System fonts tried by the private-font tests; a missing or unusable font
/// skips the case rather than failing.
const TEST_FONT_PATHS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
];

/// The number of faces the L2 text engine can load from `bytes`.
fn face_count(bytes: &[u8]) -> i64 {
    let mut engine = engine::text::TextEngine::new();
    let mut count = 0u32;
    while engine.add_font(bytes, count).is_ok() {
        count += 1;
    }
    i64::from(count)
}

/// Find a system font the L2 text engine can actually shape and return its
/// file name plus bytes.
fn test_font() -> Option<(String, Vec<u8>)> {
    for candidate in TEST_FONT_PATHS {
        let path = Path::new(candidate);
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        if engine::text::TextEngine::new().add_font(&bytes, 0).is_err() {
            continue;
        }
        let name = path.file_name()?.to_string_lossy().into_owned();
        return Some((name, bytes));
    }
    None
}

/// The font-family name `bytes` reports to the L2 text engine.
fn family_name(bytes: &[u8]) -> Option<String> {
    let mut engine = engine::text::TextEngine::new();
    let id = engine.add_font(bytes, 0).ok()?;
    engine.family_name(id).map(str::to_owned)
}

/// Rewrite the `name` table's family records of `bytes` to `family`.
///
/// Every family record must already hold `old`, and `family` must be ASCII of
/// the same length, so the string-storage offsets stay valid. Returns `None`
/// when the name table cannot be rewritten.
fn rename_font_family(bytes: &[u8], old: &str, family: &str) -> Option<Vec<u8>> {
    if !old.is_ascii() || !family.is_ascii() || family.len() != old.len() {
        return None;
    }
    let read_u16 = |at: usize| -> Option<u16> {
        bytes
            .get(at..at + 2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]))
    };
    let read_u32 = |at: usize| -> Option<u32> {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    let table_count = read_u16(4)? as usize;
    let mut name_offset = None;
    for index in 0..table_count {
        let record = 12 + index * 16;
        if bytes.get(record..record + 4) == Some(&b"name"[..]) {
            name_offset = Some(read_u32(record + 8)? as usize);
            break;
        }
    }
    let name_offset = name_offset?;
    let name_count = read_u16(name_offset + 2)? as usize;
    let storage_offset = read_u16(name_offset + 4)? as usize;
    let encode = |text: &str, platform: u16, encoding: u16| -> Option<Vec<u8>> {
        match (platform, encoding) {
            (1, 0) => Some(text.as_bytes().to_vec()),
            (0, _) | (3, 1) | (3, 10) => {
                let mut encoded = Vec::with_capacity(text.len() * 2);
                for unit in text.encode_utf16() {
                    encoded.extend_from_slice(&unit.to_be_bytes());
                }
                Some(encoded)
            }
            _ => None,
        }
    };
    let mut patched = bytes.to_vec();
    let mut replaced = 0usize;
    for index in 0..name_count {
        let record = name_offset + 6 + index * 12;
        let platform = read_u16(record)?;
        let encoding = read_u16(record + 2)?;
        let name_id = read_u16(record + 6)?;
        if name_id != 1 && name_id != 16 {
            continue;
        }
        let length = read_u16(record + 8)? as usize;
        let offset = read_u16(record + 10)? as usize;
        let start = name_offset
            .checked_add(storage_offset)?
            .checked_add(offset)?;
        let end = start.checked_add(length)?;
        let Some(expected) = encode(old, platform, encoding) else {
            continue;
        };
        let Some(replacement) = encode(family, platform, encoding) else {
            continue;
        };
        if expected.len() != length
            || replacement.len() != length
            || bytes.get(start..end) != Some(expected.as_slice())
        {
            return None;
        }
        patched[start..end].copy_from_slice(&replacement);
        replaced += 1;
    }
    (replaced > 0).then_some(patched)
}

/// A test font whose family is renamed to a value no host has installed, so
/// `GdiPlus.addPrivateFont` is what first makes the family resolvable.
///
/// Returns `(storage file name, patched bytes, family)`; `None` when the name
/// table cannot be rewritten or the patch is not observed.
fn private_family_font() -> Option<(String, Vec<u8>, String)> {
    let (name, bytes) = test_font()?;
    let old = family_name(&bytes)?;
    let custom = "K".repeat(old.len());
    let patched = rename_font_family(&bytes, &old, &custom)?;
    if family_name(&patched).as_deref() != Some(custom.as_str()) {
        return None;
    }
    Some((name, patched, custom))
}

/// A unique temporary game directory, removed on drop.
struct GameDir(PathBuf);

impl GameDir {
    fn new() -> Self {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "krkr-compat-layer-ex-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        );
        path.push(unique);
        std::fs::create_dir_all(&path).expect("create temp game dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.0.join(name), bytes).expect("write fixture");
    }
}

impl Drop for GameDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A storage mounting a copy of `name` plus a file that is not a font.
fn font_storage(name: &str, bytes: &[u8]) -> (Storage, GameDir) {
    let dir = GameDir::new();
    dir.write(name, bytes);
    dir.write("garbage.ttf", b"this is definitely not a font");
    let storage = Storage::mount_archives(
        FileSystem::host(dir.path()),
        dir.path().to_path_buf(),
        std::iter::empty::<&str>(),
    )
    .expect("mount font directory");
    (storage, dir)
}

// ---------------------------------------------------------------------------
// GdiPlus namespace shape (gdiplus.rs)
// ---------------------------------------------------------------------------

#[test]
fn gdiplus_namespace_matches_the_reference_shape() {
    let _lock = vm_lock();
    let env = Env::new();
    // `NCB_REGISTER_CLASS(GdiPlus)` publishes exactly one global class object
    // (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:3463`).
    assert_eq!(env.eval_int("typeof GdiPlus == 'Object'"), 1);
    // `NCB_SUBCLASS` attaches a class as a member of the enclosing class
    // object and never publishes it globally
    // (`reference/cpp/core/plugin/ncbind.hpp:1792-1799,1963-1990`).
    for name in [
        "PointF",
        "RectF",
        "Image",
        "Matrix",
        "Path",
        "Appearance",
        "Font",
    ] {
        let expression = format!("typeof GdiPlus.{name} == 'Object'");
        assert_eq!(env.eval_int(&expression), 1, "GdiPlus.{name} must exist");
    }
    // C++ `FontInfo` is script-visible as `GdiPlus.Font`
    // (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:3648`);
    // the bare registration globals must be gone.
    for name in [
        "PointF",
        "RectF",
        "Image",
        "Matrix",
        "Path",
        "Appearance",
        "FontInfo",
    ] {
        // A bare `typeof <absent>` raises in this VM; a `global.` member read
        // reports absence as `"undefined"`.
        let expression = format!("typeof global.{name} == 'undefined'");
        assert_eq!(env.eval_int(&expression), 1, "{name} must not be global");
    }
    // `LayerExBase` / `LayerExMovie` / `LayerExPerspective` are internal
    // native-instance bases, not TJS classes
    // (`reference/cpp/plugins/layerExPerspective.cpp:255-296`).
    for name in ["LayerExBase", "LayerExMovie", "LayerExPerspective"] {
        let expression = format!("typeof global.{name} == 'undefined'");
        assert_eq!(env.eval_int(&expression), 1, "{name} must not exist");
    }
}

#[test]
fn gdiplus_publishes_the_reference_enum_constants() {
    let _lock = vm_lock();
    let env = Env::new();
    // `ENUM(n)` is `Variant(#n, (int)n)`
    // (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:3461`);
    // the values come from the blend2d enum definitions
    // (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:65-262`).
    assert_eq!(env.eval_int("GdiPlus.Ok"), 0);
    assert_eq!(env.eval_int("GdiPlus.ProfileNotFound"), 21);
    assert_eq!(env.eval_int("GdiPlus.BrushTypeHatchFill"), 1);
    assert_eq!(env.eval_int("GdiPlus.HatchStyleDiagonalBrick"), 38);
    assert_eq!(env.eval_int("GdiPlus.SmoothingModeInvalid"), -1);
    assert_eq!(env.eval_int("GdiPlus.LineCapDiamondAnchor"), 19);
    assert_eq!(env.eval_int("GdiPlus.TextRenderingHintClearTypeGridFit"), 5);
}

// ---------------------------------------------------------------------------
// PointF (geom.rs)
// ---------------------------------------------------------------------------

#[test]
fn pointf_constructor_requires_two_arguments() {
    let _lock = vm_lock();
    let env = Env::new();
    // reference/cpp/plugins/layerex_draw/windows/main.cpp:133-137
    // registers `NCB_CONSTRUCTOR((REAL, REAL))`; NCB rejects a call below
    // the declared parameter count (`reference/cpp/core/plugin/ncbind.hpp:1178`).
    assert!(
        env.try_run("new GdiPlus.PointF(1);").is_err(),
        "`new GdiPlus.PointF(1)` must raise"
    );
    env.run("var p = new GdiPlus.PointF(1, 2);");
    assert_eq!(env.eval_real("p.x"), 1.0);
    assert_eq!(env.eval_real("p.y"), 2.0);
}

#[test]
fn pointf_components_are_settable_reals() {
    let _lock = vm_lock();
    let env = Env::new();
    // reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:341-342
    // (`PointF(tjs_real _x, tjs_real _y)`) and main.cpp:135-136 register
    // `x`/`y` as `REAL` member properties.
    env.run("var p = new GdiPlus.PointF(1.5, 2.5); p.x = 3; p.y = 4.5;");
    assert_eq!(env.eval_real("p.x"), 3.0);
    assert_eq!(env.eval_real("p.y"), 4.5);
}

#[test]
fn pointf_equals_compares_components_exactly() {
    let _lock = vm_lock();
    let env = Env::new();
    // reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:344
    // (`bool Equals(PointF tgt) { return x == tgt.x && y == tgt.y; }`).
    assert_eq!(
        env.eval_int("(new GdiPlus.PointF(1, 2)).Equals(new GdiPlus.PointF(1, 2))"),
        1
    );
    assert_eq!(
        env.eval_int("(new GdiPlus.PointF(1, 2)).Equals(new GdiPlus.PointF(1, 3))"),
        0
    );
    // A non-object argument converts to the default `PointF()` (0, 0)
    // (`reference/cpp/plugins/layerex_draw/windows/main.cpp:98-100`).
    assert_eq!(env.eval_int("(new GdiPlus.PointF(0, 0)).Equals(5)"), 1);
    // NCB requires the one parameter (`reference/cpp/core/plugin/ncbind.hpp:1178`).
    assert!(env.try_run("(new GdiPlus.PointF(1, 2)).Equals();").is_err());
}

// ---------------------------------------------------------------------------
// RectF (geom.rs)
// ---------------------------------------------------------------------------

#[test]
fn rectf_constructor_requires_four_arguments() {
    let _lock = vm_lock();
    let env = Env::new();
    // reference/cpp/plugins/layerex_draw/windows/main.cpp:181-182
    // (`NCB_CONSTRUCTOR((REAL, REAL, REAL, REAL))`) with the NCB minimum
    // check (`reference/cpp/core/plugin/ncbind.hpp:1178`).
    assert!(env.try_run("new GdiPlus.RectF(1, 2, 3);").is_err());
    env.run("var r = new GdiPlus.RectF(1, 2, 3, 4);");
    assert_eq!(env.eval_real("r.x"), 1.0);
    assert_eq!(env.eval_real("r.y"), 2.0);
    assert_eq!(env.eval_real("r.width"), 3.0);
    assert_eq!(env.eval_real("r.height"), 4.0);
}

#[test]
fn rectf_derived_edges_and_location() {
    let _lock = vm_lock();
    let env = Env::new();
    // reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:363-367:
    // `GetLeft/GetTop/GetRight/GetBottom` and `GetLocation` are read-only
    // members (`windows/main.cpp:189-192`).
    env.run("var r = new GdiPlus.RectF(1, 2, 3, 4);");
    assert_eq!(env.eval_real("r.left"), 1.0);
    assert_eq!(env.eval_real("r.top"), 2.0);
    assert_eq!(env.eval_real("r.right"), 4.0);
    assert_eq!(env.eval_real("r.bottom"), 6.0);
    assert_eq!(env.eval_real("r.location.x"), 1.0);
    assert_eq!(env.eval_real("r.location.y"), 2.0);
}

#[test]
fn rectf_clone_and_equals_copy_the_components() {
    let _lock = vm_lock();
    let env = Env::new();
    // reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:369-373
    // (`Clone`/`Equals`), registered at `windows/main.cpp:193,199`.
    env.run("var r = new GdiPlus.RectF(1, 2, 3, 4); var c = r.Clone();");
    assert_eq!(
        env.eval_int("c.x == 1 && c.y == 2 && c.width == 3 && c.height == 4"),
        1
    );
    assert_eq!(env.eval_int("r.Equals(c)"), 1);
    assert_eq!(env.eval_int("r.Equals(new GdiPlus.RectF(0, 0, 0, 0))"), 0);
    // A non-object argument converts to the default `RectF()`
    // (`reference/cpp/plugins/layerex_draw/windows/main.cpp:171-173`).
    assert_eq!(env.eval_int("(new GdiPlus.RectF(0, 0, 0, 0)).Equals(5)"), 1);
}

#[test]
fn rectf_inflate_and_offset_mutate_in_place() {
    let _lock = vm_lock();
    let env = Env::new();
    // reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:373-388:
    // `Inflate` grows by `(width, height)` on every side and `Offset` shifts
    // the origin.
    env.run("var r = new GdiPlus.RectF(1, 2, 3, 4); r.Inflate(1, 1);");
    assert_eq!(env.eval_real("r.x"), 0.0);
    assert_eq!(env.eval_real("r.y"), 1.0);
    assert_eq!(env.eval_real("r.width"), 5.0);
    assert_eq!(env.eval_real("r.height"), 6.0);
    env.run("r.Offset(2, 3);");
    assert_eq!(env.eval_real("r.x"), 2.0);
    assert_eq!(env.eval_real("r.y"), 4.0);
    // `InflatePoint(point)` is `Inflate(point.x, point.y)`
    // (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:379`,
    // registered at `windows/main.cpp:201-202`).
    env.run("var q = new GdiPlus.RectF(1, 1, 2, 2); q.InflatePoint(new GdiPlus.PointF(1, 2));");
    assert_eq!(env.eval_real("q.x"), 0.0);
    assert_eq!(env.eval_real("q.y"), -1.0);
    assert_eq!(env.eval_real("q.width"), 4.0);
    assert_eq!(env.eval_real("q.height"), 6.0);
}

#[test]
fn rectf_intersection_and_empty_predicates() {
    let _lock = vm_lock();
    let env = Env::new();
    // reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:380-384
    // (`IntersectsWith` / `IsEmptyArea`), registered at `windows/main.cpp:203-204`.
    env.run("var r = new GdiPlus.RectF(0, 0, 2, 2);");
    assert_eq!(
        env.eval_int("r.IntersectsWith(new GdiPlus.RectF(1, 1, 2, 2))"),
        1
    );
    assert_eq!(
        env.eval_int("r.IntersectsWith(new GdiPlus.RectF(5, 5, 1, 1))"),
        0
    );
    assert_eq!(
        env.eval_int("(new GdiPlus.RectF(0, 0, 0, 1)).IsEmptyArea()"),
        1
    );
    assert_eq!(
        env.eval_int("(new GdiPlus.RectF(0, 0, 1, 1)).IsEmptyArea()"),
        0
    );
}

#[test]
fn rectf_static_union_writes_the_target_and_returns_true() {
    let _lock = vm_lock();
    let env = Env::new();
    // reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:389-397
    // (`static bool Union(RectF &ret, const RectF &a, const RectF &b)`),
    // registered with `NCB_METHOD(Union)` at `windows/main.cpp:210`; the
    // first argument is overwritten with the bounding rectangle and the call
    // returns `true` (TJS integer 1).
    env.run(
        "var ret = new GdiPlus.RectF(0, 0, 0, 0); \
         var a = new GdiPlus.RectF(1, 2, 3, 4); \
         var b = new GdiPlus.RectF(5, 6, 7, 8); \
         var ok = GdiPlus.RectF.Union(ret, a, b);",
    );
    assert_eq!(env.eval_int("ok"), 1);
    assert_eq!(env.eval_real("ret.x"), 1.0);
    assert_eq!(env.eval_real("ret.y"), 2.0);
    assert_eq!(env.eval_real("ret.width"), 11.0);
    assert_eq!(env.eval_real("ret.height"), 12.0);
}

// ---------------------------------------------------------------------------
// FontInfo (font.rs)
// ---------------------------------------------------------------------------

#[test]
fn fontinfo_constructor_requires_three_arguments() {
    let _lock = vm_lock();
    let env = Env::new();
    // reference/cpp/plugins/layerex_draw/windows/main.cpp:583-584 registers
    // `NCB_CONSTRUCTOR((const tjs_char *, REAL, INT))`; NCB rejects a
    // shorter call (`reference/cpp/core/plugin/ncbind.hpp:1178`).
    assert!(env.try_run("new GdiPlus.Font('A', 12);").is_err());
    env.run("var f = new GdiPlus.Font('A', 12, 1);");
    assert_eq!(env.eval_real("f.emSize"), 12.0);
    assert_eq!(env.eval_int("f.style"), 1);
}

#[test]
fn fontinfo_property_types() {
    let _lock = vm_lock();
    let env = Env::new();
    // `getStyle` returns `INT` and `getEmSize` returns `REAL`
    // (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.hpp:77-84`,
    // registered at `windows/main.cpp:585-587`).
    env.run("var f = new GdiPlus.Font('A', 12.5, 3);");
    assert!(matches!(env.eval_ok("f.style"), TjsValue::Integer(3)));
    assert!(matches!(env.eval_ok("f.emSize"), TjsValue::Real(_)));
    // `forceSelfPathDraw` defaults to `false`
    // (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:222-224`).
    assert_eq!(env.eval_int("f.forceSelfPathDraw"), 0);
    env.run("f.forceSelfPathDraw = true;");
    assert_eq!(env.eval_int("f.forceSelfPathDraw"), 1);
    // `familyName` is the script-visible family
    // (`reference/cpp/plugins/layerex_draw/windows/main.cpp:585`).
    assert!(matches!(env.eval_ok("f.familyName"), TjsValue::String(_)));
}

#[test]
fn fontinfo_metrics_are_reals() {
    let _lock = vm_lock();
    let env = Env::new();
    // The five metric getters return `REAL`
    // (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:384-405`,
    // registered as read-only at `windows/main.cpp:588-592`). With no face
    // registered they read as 0 rather than fabricating a metric.
    env.run("var f = new GdiPlus.Font('GhostFamily', 16, 0);");
    for member in [
        "ascent",
        "descent",
        "ascentLeading",
        "descentLeading",
        "lineSpacing",
    ] {
        let expression = format!("f.{member}");
        assert!(
            matches!(env.eval_ok(&expression), TjsValue::Real(_)),
            "FontInfo.{member} must be a real"
        );
    }
}

// ---------------------------------------------------------------------------
// Appearance (appearance.rs)
// ---------------------------------------------------------------------------

#[test]
fn appearance_mutators_require_the_full_reference_arity() {
    let _lock = vm_lock();
    let env = Env::new();
    // `void addBrush(tTJSVariant, REAL ox = 0, REAL oy = 0)` and
    // `void addPen(tTJSVariant, tTJSVariant, REAL ox = 0, REAL oy = 0)`
    // (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:921,930`).
    // A default argument is not part of a C++ member-pointer type, so ncbind's
    // `ArgsCount` is the full parameter count and `doInvoke` rejects a short
    // call before the void-default path
    // (`reference/cpp/core/plugin/ncbind.hpp:921,1178`).
    env.run(
        "var app = new GdiPlus.Appearance(); \
         app.addBrush(0xFFFF0000, 0, 0); \
         app.addPen(0xFF00FF00, 1, 0, 0);",
    );
    for short in [
        "(new GdiPlus.Appearance()).addBrush();",
        "(new GdiPlus.Appearance()).addBrush(0xFFFF0000);",
        "(new GdiPlus.Appearance()).addBrush(0xFFFF0000, 1);",
        "(new GdiPlus.Appearance()).addPen(0xFFFF0000);",
        "(new GdiPlus.Appearance()).addPen(0xFFFF0000, 1);",
        "(new GdiPlus.Appearance()).addPen(0xFFFF0000, 1, 2);",
    ] {
        assert!(env.try_run(short).is_err(), "{short} should raise");
    }
}

#[test]
fn appearance_clear_empties_the_draw_list() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    let dst = env.layer_id("dst");
    // `Appearance::clear` drops every brush/pen
    // (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:422-426`).
    env.run(
        "var app = new GdiPlus.Appearance(); \
         app.addBrush(0xFFFF0000, 0, 0); \
         app.clear(); \
         dst.clear(0); \
         dst.drawRectangle(app, 0, 0, 4, 4);",
    );
    assert_eq!(env.pixel(dst, 1, 1), [0, 0, 0, 0]);
}

#[test]
fn appearance_color_arguments_follow_create_dictionary_defaults() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    let dst = env.layer_id("dst");

    // A non-object colour is cast with `Color((tjs_int)colorOrBrush)`
    // (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:626-637`).
    env.run("dst.clear(0); var app = new GdiPlus.Appearance(); app.addBrush(0xFFFF0000, 0, 0); dst.drawRectangle(app, 0, 0, 4, 4);");
    assert_eq!(env.pixel(dst, 1, 1), [255, 0, 0, 255]);

    // A string goes through `TJSStringToInteger`, so a hex spelling parses
    // (`reference/cpp/core/tjs2/tjsVariant.cpp:301-307`).
    env.run("dst.clear(0); var app2 = new GdiPlus.Appearance(); app2.addBrush('0xFFFF0000', 0, 0); dst.drawRectangle(app2, 0, 0, 4, 4);");
    assert_eq!(env.pixel(dst, 1, 1), [255, 0, 0, 255]);

    // A dictionary/array without a `type` member reads the defaults: solid
    // white (`type` defaults to `BrushTypeSolidColor`, `color` to
    // `0xFFFFFFFF`, `reference/cpp/plugins/layerex_draw/windows/
    // LayerExDraw.cpp:633-638`).
    env.run("dst.clear(0); var app3 = new GdiPlus.Appearance(); app3.addBrush([], 0, 0); dst.drawRectangle(app3, 0, 0, 4, 4);");
    assert_eq!(env.pixel(dst, 1, 1), [255, 255, 255, 255]);
}

// ---------------------------------------------------------------------------
// Path (path.rs)
// ---------------------------------------------------------------------------

#[test]
fn path_primitives_require_their_declared_arity() {
    let _lock = vm_lock();
    let env = Env::new();
    // Each `Path` primitive is registered with `NCB_METHOD`
    // (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:3438-3456`)
    // against its declared signature (`blend2d/LayerExDraw.hpp:944-977`),
    // so NCB requires the full parameter count
    // (`reference/cpp/core/plugin/ncbind.hpp:1178`).
    env.run("var p = new GdiPlus.Path();");
    for script in [
        "p.drawLine(1, 2, 3);",
        "p.drawRectangle(1, 2, 3);",
        "p.drawEllipse(1, 2, 3);",
        "p.drawArc(1, 2, 3, 4, 5);",
        "p.drawPie(1, 2, 3, 4, 5);",
        "p.drawBezier(1, 2, 3, 4, 5, 6, 7);",
        "p.drawCurve2([[0, 0], [1, 1]]);",
        "p.drawCurve3([[0, 0], [1, 1]], 0, 1);",
        "p.drawClosedCurve2([[0, 0], [1, 1]]);",
    ] {
        assert!(env.try_run(script).is_err(), "{script} must raise");
    }
}

#[test]
fn path_rectangle_and_polygon_fill_the_closed_area() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    let dst = env.layer_id("dst");
    // `Path::drawRectangle` adds a closed figure
    // (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1429-1437`)
    // and `Layer.drawPath` fills it with the appearance.
    env.run(
        "var p = new GdiPlus.Path(); \
         p.drawRectangle(1, 1, 2, 2); \
         var app = new GdiPlus.Appearance(); \
         app.addBrush(0xFF00FF00, 0, 0); \
         dst.drawPath(app, p);",
    );
    assert_eq!(env.pixel(dst, 2, 2), [0, 255, 0, 255]);
    // The corner was never drawn on: it holds the reference's default image
    // (transparent white, `reference/cpp/core/visual/LayerIntf.cpp:104-110,400`).
    assert_eq!(env.pixel(dst, 0, 0), [255, 255, 255, 0]);

    // `Path::drawPolygon` also closes its own figure
    // (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1405-1425`).
    env.run(
        "dst.clear(0); \
         var q = new GdiPlus.Path(); \
         q.drawPolygon([[0, 0], [4, 0], [2, 4]]); \
         var app2 = new GdiPlus.Appearance(); \
         app2.addBrush(0xFFFF0000, 0, 0); \
         dst.drawPath(app2, q);",
    );
    assert_eq!(env.pixel(dst, 2, 1), [255, 0, 0, 255]);
}

#[test]
fn path_invalid_bezier_chain_adds_nothing() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    let dst = env.layer_id("dst");
    // `Path::drawBeziers` silently ignores a point count below 4 or not of
    // the form `3n + 1`
    // (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1190-1191`);
    // the call still succeeds but draws nothing.
    env.run(
        "dst.clear(0); \
         var p = new GdiPlus.Path(); \
         p.drawBeziers([[0, 0], [1, 1], [2, 0]]); \
         var app = new GdiPlus.Appearance(); \
         app.addBrush(0xFFFF0000, 0, 0); \
         dst.drawPath(app, p);",
    );
    assert_eq!(env.pixel(dst, 1, 1), [0, 0, 0, 0]);
}

// ---------------------------------------------------------------------------
// Layer.perspectiveCopy (perspective.rs)
// ---------------------------------------------------------------------------

#[test]
fn perspective_copy_requires_a_layer_source() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    // The reference resolves `param[0]` with
    // `NativeInstanceSupport(TJS_NIS_GETINSTANCE, tTJSNC_Layer::ClassID, …)`
    // and throws `TVPSpecifyLayer` otherwise
    // (`reference/cpp/plugins/layerExPerspective.cpp:171-176`).
    assert!(
        env.try_run("dst.perspectiveCopy({}, 0, 0, 4, 4, 0, 0, 4, 0, 0, 4, 4, 4);")
            .is_err()
    );
}

#[test]
fn perspective_copy_rejects_non_numeric_coordinates() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    // Every coordinate is read with `param[i]->AsReal()`
    // (`reference/cpp/plugins/layerExPerspective.cpp:181-199`), which raises
    // a conversion error for an object argument rather than coercing to 0
    // (`reference/cpp/core/tjs2/tjsVariant.h:947-966`).
    assert!(
        env.try_run("dst.perspectiveCopy(src, {}, 0, 4, 4, 0, 0, 4, 0, 0, 4, 4, 4);")
            .is_err()
    );
}

#[test]
fn perspective_copy_maps_the_source_quad() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    let dst = env.layer_id("dst");
    // Argument order and quad mapping from `PerspectiveCopy_GL`
    // (`reference/cpp/plugins/layerExPerspective.cpp:166-199`): the source
    // rectangle is `(g_x1, g_y1, g_x1 + g_x3, g_y1 + g_y4)` and the four
    // destination corners are `(p5,p6) (p7,p8) (p9,p10) (p11,p12)`.
    env.run("dst.perspectiveCopy(src, 0, 0, 4, 4, 1, 1, 3, 1, 1, 3, 3, 3);");
    assert_eq!(env.pixel(dst, 1, 1), [255, 0, 0, 255]);
    assert_eq!(env.pixel(dst, 2, 2), [255, 0, 0, 255]);
    // Outside the destination quad stays at the reference's default image
    // (transparent white, `reference/cpp/core/visual/LayerIntf.cpp:104-110,400`).
    assert_eq!(env.pixel(dst, 0, 0), [255, 255, 255, 0]);
}

// ---------------------------------------------------------------------------
// Movie surface (movie.rs)
// ---------------------------------------------------------------------------

#[test]
fn movie_surface_arities() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    // `void openMovie(const tjs_char *filename, bool alpha)` and
    // `void startMovie(bool loop)` are registered with `NCB_METHOD`
    // (`reference/cpp/plugins/layerExMovie.cpp:72-74,338-339`), so NCB
    // requires 2 and 1 parameters respectively
    // (`reference/cpp/core/plugin/ncbind.hpp:1178`).
    assert!(env.try_run("dst.openMovie('movie.mpg');").is_err());
    assert!(env.try_run("dst.startMovie();").is_err());
    // `stopMovie()` and `isPlayingMovie()` take no parameters.
    env.run("dst.stopMovie();");
    assert_eq!(env.eval_int("dst.isPlayingMovie()"), 0);
}

#[test]
fn movie_callbacks_are_layer_members() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    // The reference reads `onStartMovie`/`onStopMovie`/`onUpdateMovie` from
    // the layer object (`reference/cpp/plugins/layerExMovie.cpp:205,222,274`),
    // so a script override is what the native dispatch reaches.
    env.run(
        "dst.onStartMovie = function() { return 7; }; \
         dst.onStopMovie = function() { return 8; }; \
         dst.onUpdateMovie = function() { return 9; };",
    );
    assert_eq!(env.eval_int("dst.onStartMovie()"), 7);
    assert_eq!(env.eval_int("dst.onStopMovie()"), 8);
    assert_eq!(env.eval_int("dst.onUpdateMovie()"), 9);
}

// ---------------------------------------------------------------------------
// Record lifecycle (record.rs, driven through the Layer members)
// ---------------------------------------------------------------------------

#[test]
fn record_lifecycle_round_trips_through_a_file() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    let dst = env.layer_id("dst");
    let record_path = std::env::temp_dir().join("krkr_layer_ex_behaviour_record.kexr");
    let record_path_str = record_path.to_string_lossy().replace('\\', "\\\\");

    // `setRecord(true)` creates the recording; `getRecord()` reports it
    // (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:1770-1781`).
    env.run(
        "var app = new GdiPlus.Appearance(); \
         app.addBrush(0xFFFF0000, 0, 0); \
         dst.record = true; \
         dst.drawRectangle(app, 0, 0, 2, 2);",
    );
    assert_eq!(env.eval_int("dst.record"), 1);

    // `getRecordImage()` replays the recording into a fresh image
    // (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:1808-1839`).
    env.run("var rec = dst.getRecordImage();");
    let rec_id = env.scene.bitmaps.last().expect("record bitmap").id;
    let bitmap = env.scene.bitmap(rec_id).expect("bitmap");
    let offset = bitmap.pixel_offset(1, 1).expect("pixel");
    assert_eq!(
        [
            bitmap.rgba[offset],
            bitmap.rgba[offset + 1],
            bitmap.rgba[offset + 2],
            bitmap.rgba[offset + 3],
        ],
        [255, 0, 0, 255]
    );

    // `saveRecord(filename)` returns `true` on success
    // (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:1856-1888`).
    assert_eq!(
        env.eval_int(&format!("dst.saveRecord('{record_path_str}')")),
        1
    );
    assert!(record_path.exists(), "record file should be written");

    // `loadRecord(filename)` restores the recording and redraws it
    // (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:1890-1901`;
    // the reference's `return false` is the documented divergence above).
    env.run("dst.clear(0); dst.record = false;");
    assert_eq!(env.pixel(dst, 1, 1), [0, 0, 0, 0]);
    env.run(&format!("dst.loadRecord('{record_path_str}');"));
    assert_eq!(env.pixel(dst, 1, 1), [255, 0, 0, 255]);

    // `redrawRecord()` re-renders the current recording
    // (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:1841-1854`).
    env.run("dst.clear(0);");
    assert_eq!(env.eval_int("dst.redrawRecord()"), 1);

    let _ = std::fs::remove_file(&record_path);
}

// ---------------------------------------------------------------------------
// Hand-off: the removed `nativeId`/`id` fallback
// ---------------------------------------------------------------------------

#[test]
fn copy_to_bitmap_reports_a_member_specific_error() {
    let _lock = vm_lock();
    let env = Env::new();
    two_layers(&env);
    // `Layer.copyToBitmapFromMainImage` resolves its argument with the
    // native-instance lookup for `Bitmap` and rejects anything else with
    // `TJS_E_INVALIDPARAM` (`reference/cpp/core/visual/LayerIntf.cpp:9893-9913`);
    // a plain object must raise the member's own error, not the generic
    // object-resolution message.
    // `%[]` is the TJS empty dictionary literal; `{}` is a block and is a
    // syntax error as an expression, so it never reached the member.
    let message = env
        .try_run("dst.copyToBitmapFromMainImage(%[]);")
        .expect_err("a plain object is not a Bitmap");
    assert!(
        message.contains("copyToBitmapFromMainImage"),
        "error must name the member: {message}"
    );
    assert!(
        message.contains("not a Bitmap"),
        "error must say it is not a Bitmap: {message}"
    );
}

// ---------------------------------------------------------------------------
// GdiPlus.Matrix (matrix.rs)
// ---------------------------------------------------------------------------

#[test]
fn gdiplus_matrix_constructor_arity() {
    let _lock = vm_lock();
    let env = Env::new();
    // `MatrixFactory` (`reference/cpp/plugins/layerex_draw/blend2d/
    // LayerExDraw.cpp:3206-3285`): no arguments is the identity, six
    // arguments are the elements, and any other arity returns
    // `TJS_E_INVALIDPARAM`.
    env.run("var m = new GdiPlus.Matrix();");
    assert_eq!(env.eval_int("m.IsIdentity()"), 1);
    assert_eq!(env.eval_real("m.OffsetX()"), 0.0);
    assert_eq!(env.eval_real("m.OffsetY()"), 0.0);
    assert_eq!(env.eval_int("m.GetLastStatus()"), 0);
    env.run("var six = new GdiPlus.Matrix(1, 2, 3, 4, 5, 6);");
    for script in [
        "new GdiPlus.Matrix(1);",
        "new GdiPlus.Matrix(1, 2);",
        "new GdiPlus.Matrix(1, 2, 3);",
        "new GdiPlus.Matrix(1, 2, 3, 4);",
        "new GdiPlus.Matrix(1, 2, 3, 4, 5);",
        "new GdiPlus.Matrix(1, 2, 3, 4, 5, 6, 7);",
    ] {
        assert!(env.try_run(script).is_err(), "{script} must raise");
    }
}

#[test]
fn gdiplus_matrix_set_elements_and_invert() {
    let _lock = vm_lock();
    let env = Env::new();
    // `SetElements` replaces the core and refreshes the cached values
    // (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:443-455`).
    env.run("var m = new GdiPlus.Matrix(); var status = m.SetElements(2, 0, 0, 2, 5, 6);");
    assert_eq!(env.eval_int("status"), 0);
    assert_eq!(env.eval_real("m.OffsetX()"), 5.0);
    assert_eq!(env.eval_real("m.OffsetY()"), 6.0);
    assert_eq!(env.eval_int("m.IsIdentity()"), 0);
    assert_eq!(env.eval_int("m.IsInvertible()"), 1);
    // `Invert` (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:
    // 459-466`) returns `Ok` (0) and replaces the core with its inverse.
    assert_eq!(env.eval_int("m.Invert()"), 0);
    assert_eq!(
        env.eval_int("m.Equals(new GdiPlus.Matrix(0.5, 0, 0, 0.5, -2.5, -3))"),
        1
    );
}

#[test]
fn gdiplus_matrix_singular_invert_reports_invalid_parameter() {
    let _lock = vm_lock();
    let env = Env::new();
    // A zero-determinant matrix is not invertible and `Invert` returns
    // `InvalidParameter` (2) (`reference/cpp/plugins/layerex_draw/blend2d/
    // LayerExDraw.hpp:459-466`).
    env.run("var m = new GdiPlus.Matrix(0, 0, 0, 0, 0, 0);");
    assert_eq!(env.eval_int("m.IsInvertible()"), 0);
    assert_eq!(env.eval_int("m.Invert()"), 2);
}

#[test]
fn gdiplus_matrix_multiply_and_translate() {
    let _lock = vm_lock();
    let env = Env::new();
    // `Multiply(matrix, order)` (`reference/cpp/plugins/layerex_draw/blend2d/
    // LayerExDraw.hpp:476-490`) with `MatrixOrderPrepend` applies the
    // argument first.
    env.run(
        "var m = new GdiPlus.Matrix(1, 0, 0, 1, 1, 2); \
         var other = new GdiPlus.Matrix(1, 0, 0, 1, 3, 4);",
    );
    assert_eq!(env.eval_int("m.Multiply(other, 0)"), 0);
    assert_eq!(env.eval_real("m.OffsetX()"), 4.0);
    assert_eq!(env.eval_real("m.OffsetY()"), 6.0);
    // A non-object is the reference's null matrix; `Multiply` returns
    // `InvalidParameter` (2) (`LayerExDraw.hpp:478-479`).
    assert_eq!(env.eval_int("m.Multiply(5, 0)"), 2);
    // `Translate` accumulates the cached offset
    // (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:572-584`)
    // and `Reset` clears it.
    env.run("var t = new GdiPlus.Matrix(); t.Translate(7, 8, 0);");
    assert_eq!(env.eval_real("t.OffsetX()"), 7.0);
    assert_eq!(env.eval_real("t.OffsetY()"), 8.0);
    env.run("t.Reset();");
    assert_eq!(env.eval_real("t.OffsetX()"), 0.0);
    assert_eq!(env.eval_int("t.IsIdentity()"), 1);
}

#[test]
fn gdiplus_matrix_transform_arities() {
    let _lock = vm_lock();
    let env = Env::new();
    // Every `NCB_GDIP_METHOD` keeps its declared parameter list, so the
    // defaulted `MatrixOrder` arguments are still required
    // (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:476-584`,
    // `reference/cpp/core/plugin/ncbind.hpp:1178`).
    env.run(
        "var m = new GdiPlus.Matrix(); \
         var other = new GdiPlus.Matrix(); \
         var p = new GdiPlus.PointF(0, 0);",
    );
    for script in [
        "m.Multiply(other);",
        "m.Rotate(90);",
        "m.RotateAt(90, p);",
        "m.Scale(2);",
        "m.Shear(1);",
        "m.Translate(1);",
        "m.SetElements(1, 2, 3, 4, 5);",
    ] {
        assert!(env.try_run(script).is_err(), "{script} must raise");
    }
    env.run(
        "m.Rotate(90, 0); \
         m.RotateAt(90, p, 0); \
         m.Scale(2, 2, 0); \
         m.Shear(1, 1, 0); \
         m.Translate(1, 2, 0); \
         m.SetElements(1, 2, 3, 4, 5, 6);",
    );
}

#[test]
fn gdiplus_matrix_equals_converts_arrays_and_objects() {
    let _lock = vm_lock();
    let env = Env::new();
    // `MatrixConvertor` (`reference/cpp/plugins/layerex_draw/blend2d/
    // LayerExDraw.cpp:3124-3157`) accepts a native `Matrix`, a six-element
    // array, or an object with the `m11`/`m12`/`m21`/`m22`/`dx`/`dy`
    // members.
    env.run("var m = new GdiPlus.Matrix(1, 2, 3, 4, 5, 6);");
    assert_eq!(env.eval_int("m.Equals([1, 2, 3, 4, 5, 6])"), 1);
    assert_eq!(
        env.eval_int("m.Equals(%[m11:1, m12:2, m21:3, m22:4, dx:5, dy:6])"),
        1
    );
    assert_eq!(
        env.eval_int("m.Equals(new GdiPlus.Matrix(1, 2, 3, 4, 5, 6))"),
        1
    );
    assert_eq!(
        env.eval_int("m.Equals(new GdiPlus.Matrix(6, 5, 4, 3, 2, 1))"),
        0
    );
    // A non-object is the reference's null pointer; the port raises instead
    // of dereferencing it.
    assert!(env.try_run("m.Equals(5);").is_err());
}

// ---------------------------------------------------------------------------
// GdiPlus.Image (image.rs)
// ---------------------------------------------------------------------------

#[test]
fn gdiplus_image_factory_and_accessors() {
    let _lock = vm_lock();
    let env = Env::build(Some(image_storage()), true);
    // `ImageFactory` (`reference/cpp/plugins/layerex_draw/blend2d/
    // LayerExDraw.cpp:3338-3354`): no arguments is an empty image, one
    // string loads it.
    env.run(
        "var empty = new GdiPlus.Image(); \
         var img = new GdiPlus.Image('images/test.img');",
    );
    assert_eq!(env.eval_int("empty.GetWidth()"), 0);
    assert_eq!(env.eval_int("empty.GetHeight()"), 0);
    assert_eq!(env.eval_int("empty.GetType()"), 0);
    assert_eq!(env.eval_int("empty.GetLastStatus()"), 1);
    assert_eq!(env.eval_int("empty.GetFlags()"), 0);
    assert_eq!(env.eval_int("empty.GetPixelFormat()"), 0);

    // `GdipImage::GetWidth`/`GetHeight`/`GetType`/`GetLastStatus`
    // (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:708-731`)
    // and `GetFlags` (`:696-706`) describe the loaded 2x2 bitmap.
    assert_eq!(env.eval_int("img.GetWidth()"), 2);
    assert_eq!(env.eval_int("img.GetHeight()"), 2);
    assert_eq!(env.eval_int("img.GetType()"), 1);
    assert_eq!(env.eval_int("img.GetLastStatus()"), 0);
    assert_eq!(env.eval_int("img.GetFlags()"), 18);
    // The source had an alpha channel, so `GetPixelFormat` reports
    // `PixelFormat32bppARGB` (`reference/cpp/plugins/layerex_draw/blend2d/
    // LayerExDraw.hpp:712-723` and `:33-35`).
    assert_eq!(env.eval_int("img.GetPixelFormat()"), 2_498_570);
    // Both resolutions are the reference's 96 DPI default (`:716,733`).
    assert_eq!(env.eval_real("img.GetHorizontalResolution()"), 96.0);
    assert_eq!(env.eval_real("img.GetVerticalResolution()"), 96.0);

    // `GetBounds` returns `(0, 0, width, height)` (`:668-694`).
    env.run("var bounds = img.GetBounds();");
    assert_eq!(env.eval_real("bounds.x"), 0.0);
    assert_eq!(env.eval_real("bounds.y"), 0.0);
    assert_eq!(env.eval_real("bounds.width"), 2.0);
    assert_eq!(env.eval_real("bounds.height"), 2.0);

    // `load` (`LayerExDraw.cpp:3356-3363`) replaces the image; a missing
    // file leaves an empty image without raising.
    env.run("var other = new GdiPlus.Image(); other.load('missing.png');");
    assert_eq!(env.eval_int("other.GetWidth()"), 0);
    assert_eq!(env.eval_int("other.GetLastStatus()"), 1);
    env.run("other.load('images/test.img');");
    assert_eq!(env.eval_int("other.GetWidth()"), 2);
}

#[test]
fn gdiplus_image_factory_rejects_non_strings_and_missing_files() {
    let _lock = vm_lock();
    let env = Env::build(Some(image_storage()), true);
    // `ImageFactory` returns `TJS_E_INVALIDPARAM` for a non-string first
    // argument and throws `cannot open:%1` when a string load yields nothing
    // (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:3338-3354`).
    assert!(env.try_run("new GdiPlus.Image(5);").is_err());
    let message = env
        .try_run("new GdiPlus.Image('missing.png');")
        .expect_err("a missing image must raise");
    assert!(message.contains("cannot open"), "{message}");
}

#[test]
fn gdiplus_image_clone_and_rotate_flip() {
    let _lock = vm_lock();
    let env = Env::build(Some(image_storage()), true);
    env.run("var img = new GdiPlus.Image('images/test.img'); var copy = img.Clone();");
    assert_eq!(env.eval_int("copy.GetWidth()"), 2);
    assert_eq!(env.eval_int("copy.GetHeight()"), 2);
    // `RotateFlip` returns `Ok` (0) for the handled transforms and
    // `NotImplemented` (6) otherwise
    // (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:736-773`).
    assert_eq!(env.eval_int("copy.RotateFlip(0)"), 0);
    assert_eq!(env.eval_int("copy.RotateFlip(6)"), 0);
    assert_eq!(env.eval_int("copy.RotateFlip(7)"), 6);
}

/// The literal name resolves before any extension is appended: with both
/// `images/plain` and `images/plain.png` present the decoder is handed
/// `images/plain`.
///
/// reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1591-1610: when the name
/// has no registered extension the reference appends its registered
/// extensions until one exists; the port probes the literal name first.
#[test]
fn gdiplus_image_resolution_tries_the_plain_name_before_any_extension() {
    let _lock = vm_lock();
    let files: &[(&str, &[u8])] = &[
        ("/images/plain", b"plain"),
        ("/images/plain.png", b"extended"),
    ];
    let env = Env::build(Some(memory_storage(files)), true);
    let hints = Arc::new(Mutex::new(Vec::new()));
    env.record_decodes(Arc::clone(&hints));

    env.run("var img = new GdiPlus.Image('images/plain');");

    assert_eq!(env.eval_int("img.GetWidth()"), 2);
    assert_eq!(
        hints.lock().expect("decode lock").clone(),
        vec!["images/plain".to_string()],
        "the literal name is resolved without probing an extension"
    );
}

/// A name that only resolves with an appended extension is decoded under the
/// resolved name.
///
/// reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1591-1610.
#[test]
fn gdiplus_image_resolution_probes_extensions_and_uses_the_resolved_name() {
    let _lock = vm_lock();
    let files: &[(&str, &[u8])] = &[("/images/only.png", b"extended")];
    let env = Env::build(Some(memory_storage(files)), true);
    let hints = Arc::new(Mutex::new(Vec::new()));
    env.record_decodes(Arc::clone(&hints));

    env.run("var img = new GdiPlus.Image('images/only');");

    assert_eq!(env.eval_int("img.GetWidth()"), 2);
    assert_eq!(
        hints.lock().expect("decode lock").clone(),
        vec!["images/only.png".to_string()],
        "the `.png` candidate is resolved and handed to the decoder"
    );
}

/// A name that resolves to no candidate still fails the way the reference
/// fails: `cannot open:%1`.
///
/// reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1605-1610: when no
/// extension candidate exists the reference raises; reference/cpp/plugins/
/// layerex_draw/blend2d/LayerExDraw.cpp:3338-3354: `ImageFactory` throws
/// `cannot open:%1` when the load yields nothing.
#[test]
fn gdiplus_image_name_that_resolves_to_nothing_raises_cannot_open() {
    let _lock = vm_lock();
    let files: &[(&str, &[u8])] = &[];
    let env = Env::build(Some(memory_storage(files)), true);

    let error = env
        .try_run("new GdiPlus.Image('images/missing.png');")
        .expect_err("a name that resolves to nothing must raise");
    assert!(error.contains("cannot open:images/missing.png"), "{error}");
    assert!(!error.contains("archive error"), "{error}");
}

/// A storage read that fails for a reason other than "not found" is surfaced
/// as itself instead of being retried as the next extension and ultimately
/// reported as a missing entry.
///
/// reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1591-1610: the reference
/// probes each candidate by existence alone, so a real read failure is not a
/// missing entry. The in-memory file system cannot fail a read for an entry it
/// holds, so the failure comes from a mounted XP3 whose index parses but whose
/// zlib segment is corrupt.
#[test]
fn gdiplus_image_storage_read_failure_surfaces_as_itself() {
    let _lock = vm_lock();
    let env = Env::build(Some(corrupt_archive_storage("images/broken.png")), true);

    let error = env
        .try_run("new GdiPlus.Image('images/broken.png');")
        .expect_err("a genuine read failure must raise");
    assert!(error.contains("archive error"), "{error}");
    assert!(
        !error.contains("cannot open") && !error.contains("storage not found"),
        "the real cause must not be collapsed into not-found: {error}"
    );

    // `load` reaches the same read and raises instead of silently leaving an
    // empty image.
    env.run("var img = new GdiPlus.Image();");
    let load_error = env
        .try_run("img.load('images/broken.png');")
        .expect_err("load must surface the read failure");
    assert!(load_error.contains("archive error"), "{load_error}");
}

// ---------------------------------------------------------------------------
// GdiPlus.addPrivateFont / getFontList (font.rs)
// ---------------------------------------------------------------------------

#[test]
fn gdiplus_private_font_registration_and_listing() {
    let _lock = vm_lock();
    let Some((name, bytes)) = test_font() else {
        return;
    };
    let count = face_count(&bytes);
    let (storage, _dir) = font_storage(&name, &bytes);
    let env = Env::build(Some(storage), false);

    // `getFontList` (`reference/cpp/plugins/layerex_draw/blend2d/
    // LayerExDraw.cpp:132-157`) reports no private faces before
    // `addPrivateFont`.
    assert_eq!(env.eval_int("GdiPlus.getFontList(true).length"), 0);

    // `addPrivateFont` (`LayerExDraw.cpp:93-127`) adds every face of the
    // file; `getFontList(true)` then reports the same count, and
    // `getFontList(false)` never reports fewer.
    env.run(&format!("GdiPlus.addPrivateFont('{name}');"));
    assert!(count >= 1, "a test font should expose at least one face");
    assert_eq!(env.eval_int("GdiPlus.getFontList(true).length"), count);
    assert!(env.eval_int("GdiPlus.getFontList(false).length") >= count);

    // The private face is registered with the shared text engine, so a
    // `GdiPlus.Font` for its family reports real metrics
    // (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:375-383`).
    env.run(
        "var names = GdiPlus.getFontList(true); \
         var face = new GdiPlus.Font(names[0], 16, 0);",
    );
    assert_eq!(env.eval_int("face.lineSpacing > 0"), 1);
}

/// Reference: `GdiPlus::addPrivateFont` appends the file's faces to the private
/// face list, and a `FontInfo` re-resolves its family against that list on the
/// next use, so a face added after a failed lookup is used from then on
/// (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:93-126,
/// 215-221,374-383`). A cached default-face fallback must not keep the
/// requested family on the fallback face.
#[test]
fn gdiplus_private_font_invalidates_a_failed_face_lookup() {
    let _lock = vm_lock();
    let Some((name, bytes, family)) = private_family_font() else {
        return;
    };
    let (storage, _dir) = font_storage(&name, &bytes);
    let env = Env::build(Some(storage), false);

    // Before the registration the family is unknown; it resolves to the
    // default face, and so does an unrelated missing family.
    env.run(&format!(
        "var missing = new GdiPlus.Font('{family}', 16, 0); \
         var fallbackSpacing = missing.lineSpacing; \
         var unrelated = new GdiPlus.Font('OtherlyMissing', 16, 0); \
         var unrelatedFallback = unrelated.lineSpacing;"
    ));
    let fallback_spacing = env.eval_real("fallbackSpacing");
    assert!(
        fallback_spacing > 0.0,
        "the default face must shape text, got {fallback_spacing}"
    );

    // `addPrivateFont` (`LayerExDraw.cpp:93-126`) supplies the family.
    env.run(&format!("GdiPlus.addPrivateFont('{name}');"));
    assert!(
        env.eval_int("GdiPlus.getFontList(true).length > 0") == 1,
        "addPrivateFont must list the new family"
    );

    // The next lookup must resolve to the private face, whose metrics equal
    // the same bytes loaded directly from storage. An unrelated family stays
    // on the default face.
    env.run(&format!(
        "var resolved = new GdiPlus.Font('{family}', 16, 0); \
         var resolvedSpacing = resolved.lineSpacing; \
         var direct = new GdiPlus.Font('{name}', 16, 0); \
         var directSpacing = direct.lineSpacing; \
         var unrelatedAfter = new GdiPlus.Font('OtherlyMissing', 16, 0); \
         var unrelatedSpacing = unrelatedAfter.lineSpacing;"
    ));
    let direct_spacing = env.eval_real("directSpacing");
    let resolved_spacing = env.eval_real("resolvedSpacing");
    assert_ne!(
        fallback_spacing, direct_spacing,
        "the default and private faces must differ for this test to be meaningful"
    );
    assert_eq!(
        resolved_spacing, direct_spacing,
        "a face registered by addPrivateFont must be used after a failed lookup"
    );
    assert_eq!(
        env.eval_real("unrelatedSpacing"),
        env.eval_real("unrelatedFallback"),
        "an unrelated fallback must stay on the default face"
    );
}

#[test]
fn gdiplus_private_font_error_paths() {
    let _lock = vm_lock();
    let Some((name, bytes)) = test_font() else {
        return;
    };
    let (storage, _dir) = font_storage(&name, &bytes);
    let env = Env::build(Some(storage), false);
    // A missing file throws `cannot open:%1`
    // (`reference/cpp/plugins/layerex_draw/blend2d/
    // LayerExDraw.cpp:94-95,125`): the empty placed path reaches the final
    // `TVPThrowExceptionMessage` with the requested name.
    let missing = env
        .try_run("GdiPlus.addPrivateFont('does-not-exist.ttf');")
        .expect_err("a missing font must raise");
    assert!(
        missing.contains("cannot open:does-not-exist.ttf"),
        "{missing}"
    );
    // A readable file the font reader rejects throws `blend2d cannot load:%1`.
    let garbage = env
        .try_run("GdiPlus.addPrivateFont('garbage.ttf');")
        .expect_err("a malformed font must raise");
    assert!(garbage.contains("blend2d cannot load"), "{garbage}");
    // Both `NCB_METHOD`s require their one parameter
    // (`reference/cpp/core/plugin/ncbind.hpp:1178`).
    assert!(env.try_run("GdiPlus.addPrivateFont();").is_err());
    assert!(env.try_run("GdiPlus.getFontList();").is_err());
}

/// A storage read that fails for a reason other than "not found" is surfaced
/// as itself instead of being reported as the reference's missing-file
/// `cannot open:%1`.
///
/// reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:94-100: the
/// reference reads the resolved storage through a binary stream, and a read
/// that cannot be satisfied raises the stream error
/// (`reference/cpp/core/tjs2/tjs.cpp:665-669`), so it propagates out of
/// `addPrivateFont` and never becomes `cannot open:%1` (that message is only
/// the empty-placed-path path, LayerExDraw.cpp:125). The in-memory file
/// system cannot fail a read for an entry it holds, so the failure comes from
/// a mounted XP3 whose index parses but whose zlib segment is corrupt.
#[test]
fn gdiplus_private_font_storage_read_failure_surfaces_as_itself() {
    let _lock = vm_lock();
    let env = Env::build(Some(corrupt_archive_storage("fonts/broken.ttf")), false);

    let error = env
        .try_run("GdiPlus.addPrivateFont('fonts/broken.ttf');")
        .expect_err("a genuine read failure must raise");
    assert!(error.contains("archive error"), "{error}");
    assert!(
        !error.contains("cannot open") && !error.contains("storage not found"),
        "the real cause must not be collapsed into not-found: {error}"
    );
}

// ---------------------------------------------------------------------------
// Bitmap(name) constructor
// ---------------------------------------------------------------------------

/// The KAG config/album/messageframe widgets build their checkbox, radio and
/// slider images with `new Bitmap("FRM_...")` and copy the result into a
/// child layer (`patch__configwindow.tjs` `setup()`). The reference
/// `tTJSNI_Bitmap::Construct` loads a string first argument through
/// `Bitmap.Load` (`reference/cpp/core/visual/BitmapIntf.cpp:19-43`), so the
/// destination layer's main image adopts the decoded size; a constructor that
/// ignores the name leaves a 1x1 image and the widget invisible.
#[test]
fn bitmap_name_constructor_loads_the_storage_image() {
    let _lock = vm_lock();
    let env = Env::build(Some(image_storage()), true);

    env.run(
        "var bmp = new Bitmap('images/test.img'); \
         var w = new Window(); \
         var widget = new Layer(w, null); \
         widget.name = 'widget'; \
         widget.copyFromBitmapToMainImage(bmp);",
    );

    // The synthetic decoder returns a 2x2 image (see `DECODED_RGBA`).
    let id = env.layer_id("widget");
    let layer = env.scene.layer(id).expect("widget layer");
    assert_eq!(layer.image_width, 2);
    assert_eq!(layer.image_height, 2);
    assert_eq!(env.pixel(id, 0, 0), [1, 2, 3, 255]);
    assert_eq!(env.pixel(id, 1, 1), [10, 11, 12, 255]);
}
