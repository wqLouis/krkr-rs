//! Integration tests for the `psd_file` plugin's `PSD` class.
//!
//! A small RGB PSD fixture is synthesized in memory: a 2x2 document with one
//! RLE (PackBits) compressed layer, a raw merged composite, a grid/guide
//! resource and a v6 slice resource. The test loads it through the attached
//! L2 storage and asserts the header, layer metadata, resource dictionaries
//! and the decoded pixels placed on a script `Layer`.

mod common;

use api::fs::{FsPath, OpenMode};
use common::vm_lock;
use compat::CompatHost;
use engine::{Scene, Storage};
use fs::FileSystem;
use tjs::{Tjs2Engine, TjsValue};

// ---------------------------------------------------------------------------
// Env
// ---------------------------------------------------------------------------

/// A bound VM + host + attached scene and storage.
///
/// Field order matters: `_host` drops first (removing the process binding and
/// releasing retained objects) before the scene, storage and engine.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    scene: Box<Scene>,
    _storage: Box<Storage>,
}

impl Env {
    fn new(psd_bytes: &[u8]) -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut storage = Box::new(memory_storage(&[("test.psd", psd_bytes)]));
        let mut host = CompatHost::new();
        // SAFETY: the boxed scene and storage are declared after `_host` and
        // therefore outlive the binding.
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_storage(storage.as_mut() as *mut Storage);
        }
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
            scene,
            _storage: storage,
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "psd-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "psd-test")
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

    fn eval_str(&self, expression: &str) -> String {
        match self.eval_ok(expression) {
            TjsValue::String(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected a string"),
        }
    }
}

// ---------------------------------------------------------------------------
// In-memory storage
// ---------------------------------------------------------------------------

fn memory_storage(files: &[(&str, &[u8])]) -> Storage {
    let file_system = FileSystem::memory();
    for (path, data) in files {
        let path_obj = std::path::Path::new(path);
        if let Some(parent) = path_obj.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(&file_system, &FsPath::host(parent)).expect("create parent dirs");
        }
        let file =
            fs::open(&file_system, &FsPath::host(path), OpenMode::Write).expect("open memory file");
        fs::write_at(&file_system, &file, 0, data).expect("write memory file");
        fs::flush(&file_system, &file).expect("flush memory file");
    }
    Storage::mount_archives(file_system, "/", [] as [&str; 0]).expect("mount storage")
}

// ---------------------------------------------------------------------------
// PSD fixture builder
// ---------------------------------------------------------------------------

fn be16(value: u16) -> Vec<u8> {
    value.to_be_bytes().to_vec()
}

fn be32(value: u32) -> Vec<u8> {
    value.to_be_bytes().to_vec()
}

fn bei32(value: i32) -> Vec<u8> {
    value.to_be_bytes().to_vec()
}

/// A Photoshop unicode string: a big-endian code-unit count then UTF-16BE.
fn ustr(text: &str) -> Vec<u8> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut out = bei32(units.len() as i32);
    for unit in units {
        out.extend_from_slice(&unit.to_be_bytes());
    }
    out
}

/// A layer-name Pascal string padded to a 4-byte boundary.
fn pstring4(text: &str) -> Vec<u8> {
    let mut out = vec![text.len() as u8];
    out.extend_from_slice(text.as_bytes());
    let pad = (4 - ((text.len() + 1) & 3)) & 3;
    out.extend(std::iter::repeat_n(0u8, pad));
    out
}

/// PackBits-encode two 2-byte rows and prefix the row-count table.
fn rle_channel(rows: &[[u8; 2]]) -> Vec<u8> {
    let mut counts = Vec::new();
    let mut packed = Vec::new();
    for row in rows {
        let mut body = Vec::new();
        if row[0] == row[1] {
            body.push(0xff);
            body.push(row[0]);
        } else {
            body.push(0x01);
            body.push(row[0]);
            body.push(row[1]);
        }
        counts.extend_from_slice(&(body.len() as u16).to_be_bytes());
        packed.extend_from_slice(&body);
    }
    let mut out = counts;
    out.extend_from_slice(&packed);
    out
}

/// Append one image resource block (signature, id, empty name, size, data).
fn push_resource(out: &mut Vec<u8>, id: u16, data: &[u8]) {
    out.extend_from_slice(b"8BIM");
    out.extend_from_slice(&be16(id));
    out.push(0); // name length
    out.push(0); // name padding
    out.extend_from_slice(&be32(data.len() as u32));
    out.extend_from_slice(data);
    if data.len() % 2 == 1 {
        out.push(0);
    }
}

/// Build the synthetic PSD document.
fn build_fixture() -> Vec<u8> {
    const WIDTH: i32 = 2;
    const HEIGHT: i32 = 2;

    // Layer channels (RLE), rows of two bytes.
    let channel_bodies = [
        rle_channel(&[[255, 0], [0, 255]]),
        rle_channel(&[[0, 255], [0, 255]]),
        rle_channel(&[[0, 0], [255, 255]]),
    ];

    // Layer extra data.
    let mut extra = Vec::new();
    extra.extend_from_slice(&bei32(0)); // layer mask data length
    extra.extend_from_slice(&bei32(0)); // layer blending ranges length
    extra.extend_from_slice(&pstring4("Layr"));
    // `luni` unicode name.
    let unicode_name = ustr("LayerOne");
    extra.extend_from_slice(b"8BIM");
    extra.extend_from_slice(b"luni");
    extra.extend_from_slice(&be32(unicode_name.len() as u32));
    extra.extend_from_slice(&unicode_name);
    // `lyid` layer id.
    extra.extend_from_slice(b"8BIM");
    extra.extend_from_slice(b"lyid");
    extra.extend_from_slice(&be32(4));
    extra.extend_from_slice(&bei32(7));
    // `iOpa` fill opacity.
    extra.extend_from_slice(b"8BIM");
    extra.extend_from_slice(b"iOpa");
    extra.extend_from_slice(&be32(1));
    extra.push(200);

    // Layer record.
    let mut record = Vec::new();
    record.extend_from_slice(&bei32(0)); // top
    record.extend_from_slice(&bei32(0)); // left
    record.extend_from_slice(&bei32(HEIGHT)); // bottom
    record.extend_from_slice(&bei32(WIDTH)); // right
    record.extend_from_slice(&be16(3)); // channel count
    for (index, body) in channel_bodies.iter().enumerate() {
        record.extend_from_slice(&(index as i16).to_be_bytes());
        record.extend_from_slice(&bei32(body.len() as i32 + 2));
    }
    record.extend_from_slice(b"8BIM");
    record.extend_from_slice(b"norm");
    record.push(255); // opacity
    record.push(0); // clipping
    record.push(0); // flags
    record.push(0); // filler
    record.extend_from_slice(&be32(extra.len() as u32));
    record.extend_from_slice(&extra);

    // Layer info: count, record, channel image data.
    let mut layer_info = Vec::new();
    layer_info.extend_from_slice(&1i16.to_be_bytes());
    layer_info.extend_from_slice(&record);
    for body in &channel_bodies {
        layer_info.extend_from_slice(&be16(1)); // RLE
        layer_info.extend_from_slice(body);
    }

    // Layer and mask section.
    let mut layer_mask = Vec::new();
    layer_mask.extend_from_slice(&be32(layer_info.len() as u32));
    layer_mask.extend_from_slice(&layer_info);
    layer_mask.extend_from_slice(&be32(0)); // global layer mask info size

    // Merged composite (raw RGB): every pixel (10, 20, 30).
    let mut image_data = Vec::new();
    image_data.extend_from_slice(&be16(0)); // raw
    image_data.extend_from_slice(&[10u8, 10, 10, 10]);
    image_data.extend_from_slice(&[20u8, 20, 20, 20]);
    image_data.extend_from_slice(&[30u8, 30, 30, 30]);

    // Image resources: grid/guides (1032) and slices (1050).
    let mut resources = Vec::new();
    {
        let mut data = Vec::new();
        data.extend_from_slice(&bei32(1)); // version
        data.extend_from_slice(&bei32(100)); // horizontal grid
        data.extend_from_slice(&bei32(50)); // vertical grid
        data.extend_from_slice(&bei32(2)); // guide count
        data.extend_from_slice(&bei32(12)); // vertical guide
        data.push(0);
        data.extend_from_slice(&bei32(34)); // horizontal guide
        data.push(1);
        push_resource(&mut resources, 1032, &data);
    }
    {
        let mut data = Vec::new();
        data.extend_from_slice(&bei32(6)); // version
        data.extend_from_slice(&bei32(0)); // bounding left
        data.extend_from_slice(&bei32(0)); // top
        data.extend_from_slice(&bei32(WIDTH)); // right
        data.extend_from_slice(&bei32(HEIGHT)); // bottom
        data.extend_from_slice(&ustr("grp"));
        data.extend_from_slice(&bei32(1)); // slice count
        data.extend_from_slice(&bei32(1)); // id
        data.extend_from_slice(&bei32(0)); // group id
        data.extend_from_slice(&bei32(0)); // origin
        data.extend_from_slice(&ustr("s"));
        data.extend_from_slice(&bei32(0)); // type
        data.extend_from_slice(&bei32(0)); // left
        data.extend_from_slice(&bei32(0)); // top
        data.extend_from_slice(&bei32(WIDTH)); // right
        data.extend_from_slice(&bei32(HEIGHT)); // bottom
        data.extend_from_slice(&ustr("")); // url
        data.extend_from_slice(&ustr("")); // target
        data.extend_from_slice(&ustr("")); // message
        data.extend_from_slice(&ustr("")); // alt tag
        data.push(0); // cell text is html
        data.extend_from_slice(&ustr("")); // cell text
        data.extend_from_slice(&bei32(0)); // horizontal align
        data.extend_from_slice(&bei32(0)); // vertical align
        data.push(255); // A
        data.push(10); // R
        data.push(20); // G
        data.push(30); // B
        push_resource(&mut resources, 1050, &data);
    }

    // Full file.
    let mut psd = Vec::new();
    psd.extend_from_slice(b"8BPS");
    psd.extend_from_slice(&be16(1)); // version
    psd.extend_from_slice(&[0u8; 6]); // reserved
    psd.extend_from_slice(&be16(3)); // channels
    psd.extend_from_slice(&be32(HEIGHT as u32));
    psd.extend_from_slice(&be32(WIDTH as u32));
    psd.extend_from_slice(&be16(8)); // depth
    psd.extend_from_slice(&be16(3)); // RGB
    psd.extend_from_slice(&be32(0)); // colour mode data length
    psd.extend_from_slice(&be32(resources.len() as u32));
    psd.extend_from_slice(&resources);
    psd.extend_from_slice(&be32(layer_mask.len() as u32));
    psd.extend_from_slice(&layer_mask);
    psd.extend_from_slice(&image_data);
    psd
}

/// The expected RGBA pixels of the RLE layer.
const LAYER_PIXELS: [u8; 16] = [
    255, 0, 0, 255, // (0,0) red
    0, 255, 0, 255, // (1,0) green
    0, 0, 255, 255, // (0,1) blue
    255, 255, 255, 255, // (1,1) white
];

/// The expected RGBA pixels of the raw merged composite.
const MERGED_PIXELS: [u8; 16] = [
    10, 20, 30, 255, 10, 20, 30, 255, 10, 20, 30, 255, 10, 20, 30, 255,
];

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn parses_header_layers_and_resources() {
    let _lock = vm_lock();
    let env = Env::new(&build_fixture());
    env.run("var p = new PSD(); var ok = p.load('test.psd');");

    assert_eq!(env.eval_int("ok"), 1, "load should succeed");
    assert_eq!(env.eval_int("p.width"), 2);
    assert_eq!(env.eval_int("p.height"), 2);
    assert_eq!(env.eval_int("p.channels"), 3);
    assert_eq!(env.eval_int("p.depth"), 8);
    assert_eq!(env.eval_int("p.color_mode"), 3);
    assert_eq!(env.eval_int("p.layer_count"), 1);

    assert_eq!(env.eval_int("p.getLayerType(0)"), 0);
    assert_eq!(env.eval_str("p.getLayerName(0)"), "LayerOne");

    env.run("var info = p.getLayerInfo(0);");
    assert_eq!(env.eval_int("info.width"), 2);
    assert_eq!(env.eval_int("info.height"), 2);
    assert_eq!(env.eval_int("info.layer_id"), 7);
    assert_eq!(env.eval_int("info.fill_opacity"), 200);
    assert_eq!(env.eval_int("info.type"), 13); // ltPsNormal
    assert_eq!(env.eval_int("info.blend_mode"), 0);
    assert_eq!(env.eval_int("info.visible"), 1);
    assert_eq!(env.eval_int("info.mask"), 0);
    assert_eq!(env.eval_str("info.name"), "LayerOne");

    // Grid/guide resource.
    env.run("var guides = p.getGuides();");
    assert_eq!(env.eval_int("guides.horz_grid"), 100);
    assert_eq!(env.eval_int("guides.vert_grid"), 50);
    assert_eq!(env.eval_int("guides.vertical[0]"), 12);
    assert_eq!(env.eval_int("guides.horizontal[0]"), 34);

    // Slice resource.
    env.run("var slices = p.getSlices(); var first = slices.slices[0];");
    assert_eq!(env.eval_int("slices.left"), 0);
    assert_eq!(env.eval_int("slices.right"), 2);
    assert_eq!(env.eval_int("first.id"), 1);
    assert_eq!(env.eval_str("first.name"), "s");
    assert_eq!(
        env.eval_int("first.color"),
        (255i64 << 24) | (10 << 16) | (20 << 8) | 30
    );

    // No layer comps.
    assert_eq!(env.eval_ok("p.getLayerComp()"), TjsValue::Void);

    // Class constants.
    assert_eq!(env.eval_int("PSD.color_mode_rgb"), 3);
    assert_eq!(env.eval_int("PSD.blend_mode_multiply"), 3);
    assert_eq!(env.eval_int("PSD.layer_type_folder"), 2);
}

#[test]
fn get_layer_data_decodes_rle_pixels() {
    let _lock = vm_lock();
    let env = Env::new(&build_fixture());
    env.run(
        "var p = new PSD(); p.load('test.psd'); \
         var w = new Window(); w.setSize(8, 8); \
         var l = new Layer(w, null); \
         var ret = p.getLayerData(l, 0);",
    );

    assert_eq!(env.eval_int("l.width"), 2);
    assert_eq!(env.eval_int("l.height"), 2);
    assert_eq!(env.eval_int("l.imageWidth"), 2);
    assert_eq!(env.eval_int("l.name == 'LayerOne'"), 1);

    // The reference `Layer` member list registers no `id`
    // (`reference/cpp/core/visual/LayerIntf.cpp:8283`): reading it raises,
    // and the id comes from the Rust-side scene (the only layer created).
    assert!(env.eval("l.id").is_err(), "Layer has no `id` member");
    let layer_id = env.scene.layers[0].id;
    let pixels = env
        .scene
        .with_main_image_pixels(layer_id, |pixels| pixels.to_vec())
        .expect("the layer has an image");
    assert_eq!(pixels, LAYER_PIXELS);
}

/// Reference: `_getLayerData` accepts only a `Layer` instance
/// (`layer.AsObjectNoAddRef()->IsInstanceOf(0, 0, 0, "Layer", nullptr)`) and
/// throws "not layer" otherwise
/// (`reference/cpp/plugins/psdfile/psdclass.cpp:309-313`). A plain object
/// carrying a `nativeId` is not a Layer.
#[test]
fn get_layer_data_rejects_a_plain_object() {
    let _lock = vm_lock();
    let env = Env::new(&build_fixture());
    env.run("var p = new PSD(); p.load('test.psd');");
    let error = env
        .eval("p.getLayerData(%[nativeId: 0], 0)")
        .expect_err("a plain object must not be accepted as a Layer");
    assert!(error.contains("not a layer"), "unexpected message: {error}");
}

#[test]
fn get_layer_data_raw_and_mask_variants() {
    let _lock = vm_lock();
    let env = Env::new(&build_fixture());
    env.run(
        "var p = new PSD(); p.load('test.psd'); \
         var w = new Window(); w.setSize(8, 8); \
         var raw = new Layer(w, null); \
         p.getLayerDataRaw(raw, 0); \
         var masked = new Layer(w, null); \
         p.getLayerDataMask(masked, 0);",
    );
    // Raw image (no mask) matches the masked image for a maskless layer.
    // The reference surface has no `Layer.id` member; the ids come from the
    // Rust-side scene (creation order: raw, then masked).
    let raw_id = env.scene.layers[0].id;
    let raw_pixels = env
        .scene
        .with_main_image_pixels(raw_id, |pixels| pixels.to_vec())
        .expect("the raw layer has an image");
    assert_eq!(raw_pixels, LAYER_PIXELS);

    // A maskless layer's mask is the 1x1 default-colour dummy.
    let masked_id = env.scene.layers[1].id;
    assert_eq!(env.eval_int("masked.width"), 1);
    assert_eq!(env.eval_int("masked.height"), 1);
    let mask_pixels = env
        .scene
        .with_main_image_pixels(masked_id, |pixels| pixels.to_vec())
        .expect("the mask layer has an image");
    assert_eq!(mask_pixels, vec![0, 0, 0, 255]);
}

#[test]
fn get_blend_decodes_the_merged_composite() {
    let _lock = vm_lock();
    let env = Env::new(&build_fixture());
    env.run(
        "var p = new PSD(); p.load('test.psd'); \
         var w = new Window(); w.setSize(8, 8); \
         var l = new Layer(w, null); \
         var ok = p.getBlend(l);",
    );

    assert_eq!(env.eval_int("ok"), 1);
    assert_eq!(env.eval_int("l.width"), 2);
    assert_eq!(env.eval_int("l.height"), 2);

    // The reference surface has no `Layer.id` member; the id comes from the
    // Rust-side scene (the only layer created).
    let layer_id = env.scene.layers[0].id;
    let pixels = env
        .scene
        .with_main_image_pixels(layer_id, |pixels| pixels.to_vec())
        .expect("the layer has an image");
    assert_eq!(pixels, MERGED_PIXELS);
}

#[test]
fn missing_file_loads_false() {
    let _lock = vm_lock();
    let env = Env::new(&build_fixture());
    env.run("var p = new PSD(); var ok = p.load('does-not-exist.psd');");
    assert_eq!(env.eval_int("ok"), 0);
    assert_eq!(env.eval_int("p.width"), -1);
    assert_eq!(env.eval_int("p.layer_count"), -1);
}
