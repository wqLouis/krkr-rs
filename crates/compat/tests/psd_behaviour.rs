//! Reference-behaviour tests for the `PSD` native class (the `psd.dll`
//! plugin surface) against synthetic PSD fixtures built here byte-by-byte.
//!
//! Every test carries a `reference/cpp/...` citation above it. The fixtures
//! follow the reference grammar in
//! `reference/cpp/plugins/psdfile/psdparse/psdparse.h` (`HeaderParser`,
//! `LayerInfoParser`, `LayerAndMaskParser`) so the *documented* format
//! behaviours — signature, big-endian header, size-prefixed sections, layer
//! records and channel image data — are pinned together with the
//! script-visible surface registered by `psd_file/mod.rs`
//! (`reference/cpp/plugins/psdfile/psdclass.cpp:801-893`).
//!
//! Every test serializes on the shared VM lock, since the process-wide
//! engine → context binding is single.

mod common;

use std::path::{Path, PathBuf};

use common::vm_lock;
use compat::CompatHost;
use engine::Storage;
use fs::FileSystem;
use tjs::{Tjs2Engine, TjsValue};

// ---------------------------------------------------------------------------
// Fixture builders
// ---------------------------------------------------------------------------

/// A unique temporary game directory, removed on drop.
struct GameDir(PathBuf);

impl GameDir {
    fn new() -> Self {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "krkr-compat-psdbehaviour-{}-{}",
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
}

impl Drop for GameDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A bound VM with a mounted storage attached to the compat services.
struct Env {
    // Drop order matters: the host removes the process-wide binding before
    // the boxed engine/storage it points at are freed.
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    _storage: Box<Storage>,
    _dir: GameDir,
}

impl Env {
    fn new() -> Env {
        let dir = GameDir::new();
        let mut storage = Box::new(
            Storage::mount_archives(
                FileSystem::host(dir.path()),
                dir.path().to_path_buf(),
                std::iter::empty::<&str>(),
            )
            .expect("mount storage"),
        );
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut host = CompatHost::new();
        // SAFETY: `storage` is boxed and outlives the binding, which `_host`
        // removes on drop.
        unsafe { host.services_mut().attach_storage(&mut *storage) };
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
            _storage: storage,
            _dir: dir,
        }
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self._dir.path().join(name), bytes).expect("write fixture");
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "psd-behaviour-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval_ok(&self, expression: &str) -> TjsValue {
        self.engine
            .eval(expression, "psd-behaviour-test")
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn int(&self, expression: &str) -> i64 {
        match self.eval_ok(expression) {
            TjsValue::Integer(value) => value,
            TjsValue::Real(value) => value as i64,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    fn string(&self, expression: &str) -> String {
        match self.eval_ok(expression) {
            TjsValue::String(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected a string"),
        }
    }

    fn error(&self, expression: &str) -> String {
        match self.engine.eval(expression, "psd-behaviour-test") {
            Ok(value) => panic!("{expression:?} should have raised, got {value:?}"),
            Err(error) => error.to_string(),
        }
    }
}

fn be16(value: u16) -> [u8; 2] {
    value.to_be_bytes()
}

fn be32(value: u32) -> [u8; 4] {
    value.to_be_bytes()
}

/// The reference PSD header grammar (`psdparse/psdparse.h:215-219`):
/// `"8BPS"`, big-endian version word, 6 reserved bytes, big-endian channels,
/// height, width, depth and mode.
fn psd_header(channels: u16, height: u32, width: u32, depth: u16, mode: u16) -> Vec<u8> {
    let mut out = b"8BPS".to_vec();
    out.extend_from_slice(&be16(1)); // version 1 (PSD, not PSB)
    out.extend_from_slice(&[0u8; 6]); // reserved, skipped by `advance(6)`
    out.extend_from_slice(&be16(channels));
    out.extend_from_slice(&be32(height));
    out.extend_from_slice(&be32(width));
    out.extend_from_slice(&be16(depth));
    out.extend_from_slice(&be16(mode));
    out
}

/// One image-resource block: `8BIM`, id word, Pascal name padded to even,
/// size dword, data padded to even (`psdparse/psdparse.h:251-260`).
fn image_resource(id: u16, data: &[u8]) -> Vec<u8> {
    let mut out = b"8BIM".to_vec();
    out.extend_from_slice(&be16(id));
    out.push(0); // name length 0
    out.push(0); // (0 + 1) % 2 padding bytes
    out.extend_from_slice(&be32(data.len() as u32));
    out.extend_from_slice(data);
    if !data.len().is_multiple_of(2) {
        out.push(0); // data padding
    }
    out
}

/// A Pascal layer name padded to a 4-byte boundary
/// (`psdparse/psdparse.h:380-385`).
fn pascal_name(name: &str) -> Vec<u8> {
    let bytes = name.as_bytes();
    let mut out = vec![bytes.len() as u8];
    out.extend_from_slice(bytes);
    let pad = (4 - ((bytes.len() + 1) & 3)) & 3;
    out.extend(std::iter::repeat_n(0u8, pad));
    out
}

/// A minimal, flattened PSD: header, three empty sections and a raw merged
/// image (`compression == 0`) — the full top-level grammar
/// `start = headerParser >> colorMode >> imageResource >> layerAndMask >>
/// -imageData >> eoi` (`psdparse/psdparse.h:668-669`).
fn minimal_psd() -> Vec<u8> {
    let mut out = psd_header(3, 2, 3, 8, 3);
    out.extend_from_slice(&be32(0)); // colour mode data: empty
    let resource = image_resource(1000, b"PSDTEST!"); // 8 bytes, even
    out.extend_from_slice(&be32(resource.len() as u32));
    out.extend_from_slice(&resource);
    out.extend_from_slice(&be32(0)); // layer & mask section: empty
    out.extend_from_slice(&be16(0)); // compression: raw
    out.extend_from_slice(&[
        1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18,
    ]);
    out
}

/// A PSD with two layer records and channel image data, pinning the layer
/// record layout (`psdparse/psdparse.h:468-492`) and the extra-data block
/// (`:362-398`).
///
/// Layer 0: rect 0,0-3,2, channels `0` + `-2` (mask), opacity 200, blend
/// `norm`, flag 0 (visible), Pascal name `L1`, `lyid` 42 and a `luni`
/// unicode name `Hi`.
/// Layer 1: rect 0,0-1,1, one channel, opacity 100, blend `mul `, flag 0x02
/// (hidden), Pascal name `L2`.
fn layered_psd() -> Vec<u8> {
    let mut layer_info = Vec::new();
    layer_info.extend_from_slice(&be16(2)); // layer count

    // --- layer record 0 ---
    layer_info.extend_from_slice(&be32(0)); // top
    layer_info.extend_from_slice(&be32(0)); // left
    layer_info.extend_from_slice(&be32(2)); // bottom
    layer_info.extend_from_slice(&be32(3)); // right
    layer_info.extend_from_slice(&be16(2)); // channel count
    layer_info.extend_from_slice(&be16(0u16)); // channel id 0
    layer_info.extend_from_slice(&be32(8)); // 2-byte compression + 6 bytes
    layer_info.extend_from_slice(&be16(0xFFFE)); // channel id -2 (user mask)
    layer_info.extend_from_slice(&be32(8));
    layer_info.extend_from_slice(b"8BIM");
    layer_info.extend_from_slice(b"norm"); // blend key (big_dword)
    layer_info.push(200); // opacity
    layer_info.push(0); // clipping
    layer_info.push(0); // flag: visible (`psddata.h:453`)
    layer_info.push(0); // filler
    // Extra data: 20-byte mask record, empty blending range, Pascal name,
    // `lyid` and `luni` additional blocks.
    let mut extra = Vec::new();
    extra.extend_from_slice(&be32(20)); // mask data size
    extra.extend_from_slice(&be32(0)); // mask top
    extra.extend_from_slice(&be32(0)); // mask left
    extra.extend_from_slice(&be32(2)); // mask bottom
    extra.extend_from_slice(&be32(3)); // mask right
    extra.push(0x7F); // default colour
    extra.push(0); // flags
    extra.extend_from_slice(&[0, 0]); // padding to 20 bytes
    extra.extend_from_slice(&be32(0)); // blending range: empty
    extra.extend_from_slice(&pascal_name("L1"));
    extra.extend_from_slice(b"8BIM");
    extra.extend_from_slice(b"lyid");
    extra.extend_from_slice(&be32(4));
    extra.extend_from_slice(&be32(42));
    extra.extend_from_slice(b"8BIM");
    extra.extend_from_slice(b"luni");
    extra.extend_from_slice(&be32(8));
    extra.extend_from_slice(&be32(2)); // unicode length: 2 code units
    extra.extend_from_slice(&be16(0x0048)); // 'H'
    extra.extend_from_slice(&be16(0x0069)); // 'i'
    layer_info.extend_from_slice(&be32(extra.len() as u32));
    layer_info.extend_from_slice(&extra);

    // --- layer record 1 ---
    layer_info.extend_from_slice(&be32(0)); // top
    layer_info.extend_from_slice(&be32(0)); // left
    layer_info.extend_from_slice(&be32(1)); // bottom
    layer_info.extend_from_slice(&be32(1)); // right
    layer_info.extend_from_slice(&be16(1)); // channel count
    layer_info.extend_from_slice(&be16(0u16)); // channel id 0
    layer_info.extend_from_slice(&be32(3)); // 2-byte compression + 1 byte
    layer_info.extend_from_slice(b"8BIM");
    layer_info.extend_from_slice(b"mul "); // blend key
    layer_info.push(100); // opacity
    layer_info.push(0); // clipping
    layer_info.push(0x02); // flag: bit 1 set → hidden
    layer_info.push(0); // filler
    let mut extra = Vec::new();
    extra.extend_from_slice(&be32(0)); // no mask
    extra.extend_from_slice(&be32(0)); // no blending range
    extra.extend_from_slice(&pascal_name("L2"));
    layer_info.extend_from_slice(&be32(extra.len() as u32));
    layer_info.extend_from_slice(&extra);

    // --- channel image data (same order as the records/channels) ---
    // Layer 0, channel 0: raw compression + 6 bytes.
    layer_info.extend_from_slice(&be16(0));
    layer_info.extend_from_slice(&[10, 11, 12, 13, 14, 15]);
    // Layer 0, mask channel: raw compression + 6 bytes.
    layer_info.extend_from_slice(&be16(0));
    layer_info.extend_from_slice(&[20, 21, 22, 23, 24, 25]);
    // Layer 1, channel 0: raw compression + 1 byte.
    layer_info.extend_from_slice(&be16(0));
    layer_info.push(7);

    let layer_info_len = layer_info.len();
    let mut layer_section = Vec::new();
    layer_section.extend_from_slice(&be32(layer_info_len as u32));
    layer_section.extend_from_slice(&layer_info);

    let mut out = psd_header(3, 2, 3, 8, 3);
    out.extend_from_slice(&be32(0)); // colour mode data: empty
    out.extend_from_slice(&be32(0)); // image resources: empty
    out.extend_from_slice(&be32(layer_section.len() as u32));
    out.extend_from_slice(&layer_section);
    out.extend_from_slice(&be16(0)); // merged image: raw
    out.extend_from_slice(&[0u8; 18]); // 3 channels × 3 × 2 bytes
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// The header grammar is `\"8BPS\" >> big_word >> advance(6) >> big_word >>
// big_dword >> big_dword >> big_word >> big_word`
// (`reference/cpp/plugins/psdfile/psdparse/psdparse.h:215-219`) — all
// integers big-endian — followed by the size-prefixed colour-mode,
// resource, layer/mask sections and the optional image data
// (`:648-669`). The fixture mirrors those offsets exactly.
#[test]
fn fixture_follows_the_reference_header_and_section_layout() {
    let psd = layered_psd();

    assert_eq!(&psd[0..4], b"8BPS");
    assert_eq!(u16::from_be_bytes([psd[4], psd[5]]), 1); // version
    assert_eq!(&psd[6..12], &[0u8; 6]); // reserved
    assert_eq!(u16::from_be_bytes([psd[12], psd[13]]), 3); // channels
    assert_eq!(u32::from_be_bytes(psd[14..18].try_into().unwrap()), 2); // height
    assert_eq!(u32::from_be_bytes(psd[18..22].try_into().unwrap()), 3); // width
    assert_eq!(u16::from_be_bytes([psd[22], psd[23]]), 8); // depth
    assert_eq!(u16::from_be_bytes([psd[24], psd[25]]), 3); // RGB mode
    let color_mode_len = u32::from_be_bytes(psd[26..30].try_into().unwrap());
    assert_eq!(color_mode_len, 0);
    let resource_len = u32::from_be_bytes(psd[30..34].try_into().unwrap());
    assert_eq!(resource_len, 0);
    let layer_len = u32::from_be_bytes(psd[34..38].try_into().unwrap());
    assert!(layer_len > 0, "the layered fixture must carry layer data");
}

// `load` parses the header through the grammar
// (`reference/cpp/plugins/psdfile/psdparse/psdparse.h:215-219`, values
// stored by `setHeader` at `:222-230`) and the properties read them back as
// plain integers (`reference/cpp/plugins/psdfile/psdclass.h:56-63`) —
// `Integer`, never `Real`.
#[test]
fn load_reads_header_properties_as_integers() {
    let _lock = vm_lock();
    let env = Env::new();
    env.write("ok.psd", &minimal_psd());

    env.run(
        "var psd = new PSD(); \
         var ok = psd.load('ok.psd'); \
         var wKind = typeof psd.width; \
         var hKind = typeof psd.height; \
         var cKind = typeof psd.channels; \
         var dKind = typeof psd.depth; \
         var mKind = typeof psd.color_mode; \
         var lKind = typeof psd.layer_count;",
    );

    assert_eq!(env.int("ok"), 1);
    assert_eq!(env.int("psd.width"), 3);
    assert_eq!(env.int("psd.height"), 2);
    assert_eq!(env.int("psd.channels"), 3);
    assert_eq!(env.int("psd.depth"), 8);
    assert_eq!(env.int("psd.color_mode"), 3);
    assert_eq!(env.int("psd.layer_count"), 0);
    for kind in ["wKind", "hKind", "cKind", "dKind", "mKind", "lKind"] {
        assert_eq!(env.string(kind), "Integer", "{kind} is not an Integer");
    }
}

// Before a load — and after a failed one — every header property answers
// `-1`: `get_##tag() { return isLoaded ? header.tag : -1; }`
// (`reference/cpp/plugins/psdfile/psdclass.h:56`), `get_color_mode` /
// `get_layer_count` at `:62-63`, and `PSDFile::load` resets `isLoaded`
// before parsing (`reference/cpp/plugins/psdfile/psdparse/psdfile.cpp:17-24`).
// A failed load therefore never leaks a partial document.
#[test]
fn header_properties_are_minus_one_until_loaded() {
    let _lock = vm_lock();
    let env = Env::new();
    env.write("garbage.psd", b"this is not a psd file at all, just text");

    env.run(
        "var psd = new PSD(); \
         var w0 = psd.width; \
         var l0 = psd.layer_count; \
         var ok = psd.load('garbage.psd'); \
         var w1 = psd.width; \
         var l1 = psd.layer_count; \
         var kind = typeof psd.width;",
    );

    assert_eq!(env.int("w0"), -1);
    assert_eq!(env.int("l0"), -1);
    assert_eq!(env.int("ok"), 0);
    assert_eq!(env.int("w1"), -1);
    assert_eq!(env.int("l1"), -1);
    assert_eq!(env.string("kind"), "Integer");
}

// `load` failure modes are typed `false`, never an exception and never a
// partial result: a missing file (`psdparse/psdfile.cpp:20-27`), a bad
// `8BPS` signature (`psdparse/psdparse.h:215` + `psdfile.cpp:42-47`), a
// truncated header and a header whose sections are missing all return
// `false` (`psdfile.cpp:43-47` requires the whole file to parse), and the
// layer accessors then raise `no data`
// (`reference/cpp/plugins/psdfile/psdclass.cpp:168-171`).
#[test]
fn load_failures_are_typed_false_without_partial_state() {
    let _lock = vm_lock();
    let env = Env::new();
    env.write("text.psd", b"definitely not 8BPS");
    env.write("truncated.psd", &psd_header(3, 2, 3, 8, 3)[..10]);
    env.write("sections_missing.psd", &{
        let mut bytes = psd_header(3, 2, 3, 8, 3);
        bytes.extend_from_slice(&be32(0)); // colour mode only: stops early
        bytes
    });

    env.run(
        "var a = new PSD(); var rMissing = a.load('nope.psd'); \
         var b = new PSD(); var rText = b.load('text.psd'); \
         var c = new PSD(); var rTrunc = c.load('truncated.psd'); \
         var rSections = b.load('sections_missing.psd');",
    );

    assert_eq!(env.int("rMissing"), 0);
    assert_eq!(env.int("rText"), 0);
    assert_eq!(env.int("rTrunc"), 0);
    assert_eq!(env.int("rSections"), 0);

    // No layer is reachable after a failed load (`checkLayerNo` → "no data").
    let message = env.error("b.getLayerName(0)");
    assert!(message.contains("no data"), "unexpected error: {message}");
    let message = env.error("b.getLayerInfo(0)");
    assert!(message.contains("no data"), "unexpected error: {message}");
}

// The ncbind method wrapper enforces the declared argument count —
// `if (_numparams < SelectorT::ArgsCount) return TJS_E_BADPARAMCOUNT;`
// (`reference/cpp/core/plugin/ncbind.hpp:1178`) — so `load()` with no
// filename raises.
#[test]
fn load_requires_exactly_one_argument() {
    let _lock = vm_lock();
    let env = Env::new();

    let message = env.error("(new PSD()).load()");
    assert!(!message.is_empty());
}

// The header properties are declared getter-only —
// `#define INTPROP(name) Property(TJS_W(#name), &Class::get_##name, 0)`
// (`reference/cpp/plugins/psdfile/psdclass.cpp:872-879`) — so a script
// write raises (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:1444-1464`
// turns the missing setter into an error).
#[test]
fn header_properties_are_read_only() {
    let _lock = vm_lock();
    let env = Env::new();

    for statement in [
        "(new PSD()).width = 5;".to_string(),
        "(new PSD()).layer_count = 9;".to_string(),
    ] {
        let error = env
            .engine
            .exec_script(&statement, "psd-behaviour-test")
            .err()
            .unwrap_or_else(|| panic!("{statement} should have raised"));
        assert!(!error.to_string().is_empty(), "{statement}: empty error");
    }
}

// The class constants are registered from the reference enumerations:
// `color_mode_rgb` (`reference/cpp/plugins/psdfile/psdclass.cpp:809`,
// value `3` at `psdparse/psddata.h:47`), `blend_mode_normal`
// (`psdclass.cpp:816`, `0` at `psddata.h:80`) and `layer_type_normal`
// (`psdclass.cpp:864`, `0` at `psddata.h:12`), multiply = `3`
// (`psddata.h:83`), folder = `2` (`psddata.h:14`), lab = `9`
// (`psddata.h:51`).
#[test]
fn class_constants_match_the_reference_enumerations() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var rgb = PSD.color_mode_rgb; \
         var lab = PSD.color_mode_lab; \
         var normal = PSD.blend_mode_normal; \
         var multiply = PSD.blend_mode_multiply; \
         var layerNormal = PSD.layer_type_normal; \
         var layerFolder = PSD.layer_type_folder; \
         var kind = typeof PSD.color_mode_rgb;",
    );

    assert_eq!(env.int("rgb"), 3);
    assert_eq!(env.int("lab"), 9);
    assert_eq!(env.int("normal"), 0);
    assert_eq!(env.int("multiply"), 3);
    assert_eq!(env.int("layerNormal"), 0);
    assert_eq!(env.int("layerFolder"), 2);
    assert_eq!(env.string("kind"), "Integer");
}

// Layer records round-trip through the reference layout: `getLayerInfo`
// exposes the record fields (`reference/cpp/plugins/psdfile/psdclass.cpp:220-253`),
// `getLayerName` prefers the `luni` unicode name over the Pascal name
// (`psdclass.cpp:181-190`, `psdparse/psdparse.cpp:416-418`) and falls back
// to the Pascal name (`psdparse/psdparse.h:380-385`), `blend_mode` is the
// raw `psd::BlendMode` value (`psddata.h:78-109`, `mul ` → 3) while `type`
// is the KiriKiri mapping (`psdclass.cpp:16-17`, `ltPsNormal = 13` at
// `reference/cpp/core/visual/drawable.h:35`), `visible` flips on flag bit 1
// (`psddata.h:453`) and `layer_id` comes from the `lyid` block
// (`psdparse/psdparse.cpp:420-422`).
#[test]
fn layer_records_round_trip_names_types_and_flags() {
    let _lock = vm_lock();
    let env = Env::new();
    env.write("layered.psd", &layered_psd());

    env.run(
        "var psd = new PSD(); \
         var ok = psd.load('layered.psd'); \
         var count = psd.layer_count; \
         var name0 = psd.getLayerName(0); \
         var name1 = psd.getLayerName(1); \
         var type0 = psd.getLayerType(0); \
         var type1 = psd.getLayerType(1); \
         var info0 = psd.getLayerInfo(0); \
         var info1 = psd.getLayerInfo(1);",
    );

    assert_eq!(env.int("ok"), 1);
    assert_eq!(env.int("count"), 2);
    assert_eq!(env.string("name0"), "Hi"); // unicode name wins
    assert_eq!(env.string("name1"), "L2"); // Pascal fallback
    assert_eq!(env.int("type0"), 0);
    assert_eq!(env.int("type1"), 0);
    assert_eq!(env.int("type0 == PSD.layer_type_normal"), 1);

    // Layer 0: full record fields.
    assert_eq!(env.string("typeof info0"), "Object");
    assert_eq!(env.int("info0.top"), 0);
    assert_eq!(env.int("info0.left"), 0);
    assert_eq!(env.int("info0.bottom"), 2);
    assert_eq!(env.int("info0.right"), 3);
    assert_eq!(env.int("info0.width"), 3);
    assert_eq!(env.int("info0.height"), 2);
    assert_eq!(env.int("info0.opacity"), 200);
    assert_eq!(env.int("info0.fill_opacity"), 255);
    assert_eq!(env.int("info0.mask"), 1); // the -2 channel sets the flag
    assert_eq!(env.int("info0.type"), 13); // convBlendMode(norm) = ltPsNormal
    assert_eq!(env.int("info0.blend_mode"), 0);
    assert_eq!(env.int("info0.visible"), 1); // flag bit 1 clear
    assert_eq!(env.string("info0.name"), "Hi");
    assert_eq!(env.int("info0.clipping"), 0);
    assert_eq!(env.int("info0.layer_id"), 42);
    assert_eq!(env.int("info0.obsolete"), 0);
    assert_eq!(env.int("info0.transparency_protected"), 0);
    assert_eq!(env.int("info0.pixel_data_irrelevant"), 0);

    // Layer 1: multiply blend and the hidden flag.
    assert_eq!(env.int("info1.opacity"), 100);
    assert_eq!(env.int("info1.blend_mode"), 3); // BLEND_MODE_MULTIPLY
    assert_eq!(env.int("info1.visible"), 0); // flag bit 1 set
    assert_eq!(env.int("info1.mask"), 0);
    assert_eq!(env.int("info1.layer_id"), -1); // no `lyid` block
    assert_eq!(env.string("info1.name"), "L2");
}

// Layer indices are validated by `checkLayerNo`
// (`reference/cpp/plugins/psdfile/psdclass.cpp:168-177`): unloaded →
// "no data", out of range → "not such layer" — both raise.
#[test]
fn layer_index_checks_raise_typed_errors() {
    let _lock = vm_lock();
    let env = Env::new();
    env.write("layered.psd", &layered_psd());

    env.run("var fresh = new PSD(); var psd = new PSD(); psd.load('layered.psd');");

    let message = env.error("fresh.getLayerName(0)");
    assert!(message.contains("no data"), "unexpected error: {message}");
    let message = env.error("psd.getLayerName(2)");
    assert!(
        message.contains("not such layer"),
        "unexpected error: {message}"
    );
    let message = env.error("psd.getLayerInfo(-1)");
    assert!(
        message.contains("not such layer"),
        "unexpected error: {message}"
    );
    let message = env.error("psd.getLayerType(99)");
    assert!(
        message.contains("not such layer"),
        "unexpected error: {message}"
    );
}

// `getSlices` / `getGuides` return `void` when the document carries no
// slice (`reference/cpp/plugins/psdfile/psdclass.cpp:419-477`, guard at
// `:425`) or guide (`:484-508`, guard at `:490`) resource, and raise
// `no data` before a load (`:420-421`, `:485-486`).
#[test]
fn slices_and_guides_are_void_without_the_resources() {
    let _lock = vm_lock();
    let env = Env::new();
    env.write("ok.psd", &minimal_psd());

    env.run(
        "var psd = new PSD(); psd.load('ok.psd'); \
         var slices = psd.getSlices(); \
         var guides = psd.getGuides(); \
         var sliceKind = typeof slices; \
         var guideKind = typeof guides;",
    );

    // `typeof` renders void as lowercase "void"
    // (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2603-2612`).
    assert_eq!(env.string("sliceKind"), "void");
    assert_eq!(env.string("guideKind"), "void");

    let message = env.error("(new PSD()).getSlices()");
    assert!(message.contains("no data"), "unexpected error: {message}");
    let message = env.error("(new PSD()).getGuides()");
    assert!(message.contains("no data"), "unexpected error: {message}");
}

// The image-resource block layout — `8BIM`, id word, Pascal name padded to
// even, size dword, data padded to even
// (`reference/cpp/plugins/psdfile/psdparse/psdparse.h:251-260`) — is
// exercised by `minimal_psd` (an *odd*-length resource forces the trailing
// pad byte): if the padding were wrong the section would misalign and the
// whole parse would fail, so a successful `load` pins the layout.
#[test]
fn odd_length_image_resources_are_padded_to_even() {
    let _lock = vm_lock();
    let env = Env::new();
    // A 9-byte resource payload forces the trailing pad byte.
    let resource = image_resource(1000, b"odd9byteX");
    let mut out = psd_header(1, 1, 1, 8, 1);
    out.extend_from_slice(&be32(0)); // colour mode data: empty
    out.extend_from_slice(&be32(resource.len() as u32));
    out.extend_from_slice(&resource);
    out.extend_from_slice(&be32(0)); // layer & mask section: empty
    out.extend_from_slice(&be16(0)); // merged image: raw
    out.push(0x80); // one channel × 1 × 1 byte
    env.write("odd.psd", &out);

    env.run("var psd = new PSD(); var ok = psd.load('odd.psd');");
    assert_eq!(env.int("ok"), 1);
}
