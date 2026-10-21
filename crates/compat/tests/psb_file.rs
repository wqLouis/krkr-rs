//! Integration tests for the `PSBFile` native class.
//!
//! The fixture is a synthetic PSB v2 container built by the helpers below:
//! a charset/names table, a string table, one resource chunk and a root
//! object dictionary, following the reference header layout
//! (`reference/cpp/plugins/psbfile/PSBHeader.h:8-40`,
//! `parsePSBHeader` at `:54-85`). The tests drive the class through TJS
//! against a bound [`CompatHost`] with a host-backed storage (so the
//! embedded-image bridge can stage its temporary file) and a synthetic
//! `media` codec installed through the `Bitmap` seam.

mod common;

use std::path::{Path, PathBuf};

use common::vm_lock;
use compat::CompatHost;
use engine::{Scene, Storage};
use fs::FileSystem;
use tjs::{Tjs2Engine, TjsValue};

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

/// A unique temporary game directory, removed on drop.
struct GameDir(PathBuf);

impl GameDir {
    fn new() -> Self {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "krkr-compat-psb-{}-{}",
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

/// The number of bytes needed to encode `value` as a little-endian count.
fn count_bytes(value: usize) -> usize {
    let mut bytes = 1;
    let mut remaining = value;
    while remaining > 0xff {
        remaining >>= 8;
        bytes += 1;
    }
    bytes.min(8)
}

/// Encode a PSB array/list body with its leading `ArrayN*` type byte.
fn psb_list(values: &[u32], entry_width: usize) -> Vec<u8> {
    let count = values.len();
    let width = count_bytes(count);
    let mut out = Vec::new();
    out.push((0x0D + (width - 1)) as u8);
    for index in 0..width {
        out.push(((count >> (8 * index)) & 0xff) as u8);
    }
    out.push((0x0C + entry_width) as u8);
    for value in values {
        for index in 0..entry_width {
            out.push(((value >> (8 * index)) & 0xff) as u8);
        }
    }
    out
}

/// Build a reference name-table encoding for a set of names.
///
/// The reference stores each name as a path through a byte trie: a node's
/// parent is `names_data[node]`, the byte on the edge is
/// `node - charset[parent]`, and `name_indexes[i]` is a slot whose
/// `names_data` entry is the terminal node. `decode_names` walks the chain
/// back to the root and reverses it.
fn encode_names(names: &[&str]) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
    type Children = std::collections::BTreeMap<u8, usize>;

    fn assign(
        node: usize,
        index: u32,
        children: &[Children],
        node_index: &mut [u32],
        charset: &mut Vec<u32>,
        names_data: &mut Vec<u32>,
        next_base: &mut u32,
    ) {
        if children[node].is_empty() {
            return;
        }
        // Reserve a 256-wide window so every child byte maps to a unique
        // node index that no other parent can reuse.
        let base = *next_base;
        *next_base += 256;
        if charset.len() <= index as usize {
            charset.resize(index as usize + 1, 0);
        }
        charset[index as usize] = base;
        let edges: Vec<(usize, u32)> = children[node]
            .iter()
            .map(|(&byte, &child)| {
                let child_index = base + u32::from(byte);
                if names_data.len() <= child_index as usize {
                    names_data.resize(child_index as usize + 1, 0);
                }
                names_data[child_index as usize] = index;
                node_index[child] = child_index;
                (child, child_index)
            })
            .collect();
        for (child, child_index) in edges {
            assign(
                child,
                child_index,
                children,
                node_index,
                charset,
                names_data,
                next_base,
            );
        }
    }

    let mut children: Vec<Children> = vec![Children::new()];
    let mut terminals = Vec::with_capacity(names.len());
    for name in names {
        let mut node = 0usize;
        for byte in name.bytes() {
            node = if let Some(&child) = children[node].get(&byte) {
                child
            } else {
                children.push(Children::new());
                let child = children.len() - 1;
                children[node].insert(byte, child);
                child
            };
        }
        terminals.push(node);
    }

    let mut charset = vec![0u32; 1];
    let mut names_data = vec![0u32; 1];
    let mut node_index = vec![0u32; children.len()];
    let mut next_base = 1u32;
    assign(
        0,
        0,
        &children,
        &mut node_index,
        &mut charset,
        &mut names_data,
        &mut next_base,
    );
    let indexes: Vec<u32> = terminals
        .into_iter()
        .map(|terminal| {
            let slot = next_base;
            next_base += 1;
            if names_data.len() <= slot as usize {
                names_data.resize(slot as usize + 1, 0);
            }
            names_data[slot as usize] = node_index[terminal];
            slot
        })
        .collect();
    (charset, names_data, indexes)
}

fn num4(value: i32) -> Vec<u8> {
    let mut out = vec![0x08];
    out.extend_from_slice(&value.to_le_bytes());
    out
}

fn num1(value: u8) -> Vec<u8> {
    vec![0x05, value]
}

/// A `PSBList` value: offsets table followed by the child values.
fn list_value(children: &[Vec<u8>]) -> Vec<u8> {
    let mut offsets = Vec::new();
    let mut position = 0u32;
    for child in children {
        offsets.push(position);
        position += child.len() as u32;
    }
    let mut out = vec![0x20];
    out.extend(psb_list(&offsets, 4));
    for child in children {
        out.extend(child);
    }
    out
}

/// A v2 `Objects` value: name-index table, offset table, then the entries.
fn objects_v2(entries: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut offsets = Vec::new();
    let mut position = 0u32;
    for (_, value) in entries {
        offsets.push(position);
        position += value.len() as u32;
    }
    let name_indexes: Vec<u32> = entries.iter().map(|(index, _)| *index).collect();
    let mut out = vec![0x21];
    out.extend(psb_list(&name_indexes, 2));
    out.extend(psb_list(&offsets, 4));
    for (_, value) in entries {
        out.extend(value);
    }
    out
}

/// Build the synthetic PSB v2 fixture used by every test.
///
/// Layout mirrors `PSBHeader.h:54-85`: a 40-byte v2 header (magic, version,
/// encrypt, then the little-endian offset table), the name trie, the string
/// offset table, the string data, the chunk offset/length tables, the chunk
/// data and finally the root `Objects` value
/// (`reference/cpp/plugins/psbfile/PSBFile.cpp:328-527`).
fn build_fixture() -> Vec<u8> {
    let names = [
        "id", "num", "str", "arr", "res", "flag", "nothing", "spec", "a", "b", "sub", "realv",
        "big", "nested",
    ];
    let (charset, names_data, name_indexes) = encode_names(&names);
    // The names-section `nameIndexes` table holds trie slot indices, but an
    // object's name table holds ordinals into the decoded `names` list.
    let positions: Vec<u32> = (0..name_indexes.len() as u32).collect();
    let names_section = [
        psb_list(&charset, 4),
        psb_list(&names_data, 4),
        psb_list(&name_indexes, 2),
    ]
    .concat();

    let strings_data = b"hello\0world\0motion\0krkr\0".to_vec();
    let string_offsets = vec![0u32, 6, 12, 19];
    let strings_section = psb_list(&string_offsets, 4);

    let resource = b"FAKEIMAGE".to_vec();
    let chunk_offsets = vec![0u32];
    let chunk_lengths = vec![resource.len() as u32];

    let nested = objects_v2(&[(positions[8], num1(9)), (positions[9], vec![0x15, 1])]);
    // A resource nested in a dictionary and one nested in a list, so the
    // `psb://` registration walk is exercised below the root.
    let resource_nest = objects_v2(&[
        (positions[8], vec![0x19, 0]),
        (positions[9], list_value(&[vec![0x19, 0]])),
    ]);
    let entries: Vec<(u32, Vec<u8>)> = vec![
        (positions[0], vec![0x15, 2]),                         // id = "motion"
        (positions[1], num4(1234)),                            // num
        (positions[2], vec![0x15, 0]),                         // str = "hello"
        (positions[3], list_value(&[num1(7), vec![0x15, 1]])), // arr
        (positions[4], vec![0x19, 0]),                         // res
        (positions[5], vec![0x03]),                            // flag = true
        (positions[6], vec![0x01]),                            // nothing = null
        (positions[7], vec![0x15, 3]),                         // spec = "krkr"
        (positions[10], nested),                               // sub
        (positions[13], resource_nest),                        // nested
        (positions[11], {
            // A `Float` value: type byte `0x1E` + little-endian f32
            // (`reference/cpp/plugins/psbfile/PSBFile.cpp:244-247`).
            let mut out = vec![0x1E];
            out.extend_from_slice(&3.5f32.to_le_bytes());
            out
        }), // realv = 3.5
        (positions[12], {
            // A `PSBArray` of one `u32` entry: `ArrayN1` count-size byte,
            // count, entry-width byte, four little-endian bytes
            // (`reference/cpp/plugins/psbfile/PSBFile.cpp:248-256`,
            // `reference/cpp/plugins/psbfile/PSBValue.h:202-210`).
            psb_list(&[0xFFFF_FFFF], 4)
        }), // big
    ];
    let root = objects_v2(&entries);

    let mut out = vec![0u8; 40];
    let offset_names = out.len() as u32;
    out.extend_from_slice(&names_section);
    let offset_strings = out.len() as u32;
    out.extend_from_slice(&strings_section);
    let offset_strings_data = out.len() as u32;
    out.extend_from_slice(&strings_data);
    let offset_chunk_offsets = out.len() as u32;
    out.extend_from_slice(&psb_list(&chunk_offsets, 4));
    let offset_chunk_lengths = out.len() as u32;
    out.extend_from_slice(&psb_list(&chunk_lengths, 4));
    let offset_chunk_data = out.len() as u32;
    out.extend_from_slice(&resource);
    let offset_entries = out.len() as u32;
    out.extend_from_slice(&root);

    out[0..4].copy_from_slice(b"PSB\0");
    out[4..6].copy_from_slice(&2u16.to_le_bytes());
    out[6..8].copy_from_slice(&0u16.to_le_bytes());
    out[8..12].copy_from_slice(&offset_names.to_le_bytes());
    out[12..16].copy_from_slice(&offset_names.to_le_bytes());
    out[16..20].copy_from_slice(&offset_strings.to_le_bytes());
    out[20..24].copy_from_slice(&offset_strings_data.to_le_bytes());
    out[24..28].copy_from_slice(&offset_chunk_offsets.to_le_bytes());
    out[28..32].copy_from_slice(&offset_chunk_lengths.to_le_bytes());
    out[32..36].copy_from_slice(&offset_chunk_data.to_le_bytes());
    out[36..40].copy_from_slice(&offset_entries.to_le_bytes());
    out
}

// ---------------------------------------------------------------------------
// Environment
// ---------------------------------------------------------------------------

/// A bound VM + host + scene + host-backed storage.
struct Env {
    // Drop order matters: the host removes the process-wide binding before
    // the boxed engine/scene/storage it points at are freed.
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    _scene: Box<Scene>,
    _storage: Box<Storage>,
    _dir: GameDir,
}

impl Env {
    /// Build an environment.
    fn new() -> Env {
        let dir = GameDir::new();
        let fs = FileSystem::host(dir.path());
        let storage =
            Storage::mount_archives(fs, dir.path().to_path_buf(), std::iter::empty::<&str>())
                .expect("mount storage");
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut storage = Box::new(storage);
        let mut host = CompatHost::new();
        // SAFETY: the boxed scene and storage outlive the binding, which
        // `_host` removes on drop.
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_storage(storage.as_mut() as *mut Storage);
        }
        host.install(&engine).expect("install compat surface");
        common::link_all_builtins(&engine);
        Env {
            _host: host,
            engine,
            _scene: scene,
            _storage: storage,
            _dir: dir,
        }
    }

    fn write_fixture(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self._dir.path().join(name), bytes).expect("write fixture");
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "psb-file-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval_ok(&self, expression: &str) -> TjsValue {
        self.engine
            .eval(expression, "psb-file-test")
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
        match self.engine.eval(expression, "psb-file-test") {
            Ok(value) => panic!("{expression:?} should have raised, got {value:?}"),
            Err(error) => error.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// `root` converts every root entry through `toTJSVal`
// (`reference/cpp/plugins/psbfile/main.cpp:33-41`): strings become strings,
// numbers keep their integer/real type (`PSBValue.cpp:166-199`) and
// `PSBNull` becomes **void** (`:166`), not `null`.
#[test]
fn load_exposes_the_root_tree() {
    let _lock = vm_lock();
    let env = Env::new();
    env.write_fixture("fixture.psb", &build_fixture());

    env.run(
        "var f = new PSBFile(); \
         var ok = f.load('fixture.psb'); \
         var root = f.root; \
         var str = root.str; \
         var arr0 = root.arr[0]; \
         var arr1 = root.arr[1]; \
         var flag = root.flag; \
         var isVoid = (root.nothing === void); \
         var isNull = (root.nothing === null);",
    );

    assert_eq!(env.int("ok"), 1);
    assert_eq!(env.int("root.num"), 1234);
    assert_eq!(env.string("str"), "hello");
    assert_eq!(env.int("arr0"), 7);
    assert_eq!(env.string("arr1"), "world");
    assert_eq!(env.int("flag"), 1);
    assert_eq!(env.int("isVoid"), 1);
    assert_eq!(env.int("isNull"), 0);
}

// `PSBFile.load` registers each top-level `PSBResource` into the `psb://`
// storage media (`reference/cpp/plugins/psbfile/main.cpp:60-68`,
// `PSBMedia.cpp:24-27`), so a caller can open
// `psb://<normalized container>/<resource key>` afterwards. `PSDInfo.loadPIMG`
// relies on this: it builds `psb://<container>/<layer_id>.tlg`
// (`data.xp3:system/psdlayer.tjs`).
#[test]
fn load_registers_psb_resources_in_the_psb_media() {
    let _lock = vm_lock();
    let env = Env::new();
    env.write_fixture("fixture.psb", &build_fixture());

    env.run(
        "var f = new PSBFile(); \
         var ok = f.load('fixture.psb'); \
         var exists = Storages.isExistentStorage('psb://fixture.psb/res'); \
         var missing = Storages.isExistentStorage('psb://fixture.psb/nope');",
    );

    assert_eq!(env.int("ok"), 1);
    assert_eq!(env.int("exists"), 1, "the registered resource must resolve");
    assert_eq!(
        env.int("missing"),
        0,
        "an unregistered key must not resolve"
    );
}

// `PSB::registerRootResources` walks the whole value tree, so a resource
// nested below the root is registered under its `/`-joined member path and a
// list entry contributes its index as a path segment
// (`reference/cpp/plugins/psbfile/PSBMediaRegistry.cpp:16-77`).
#[test]
fn nested_psb_resources_are_registered_under_their_member_path() {
    let _lock = vm_lock();
    let env = Env::new();
    env.write_fixture("fixture.psb", &build_fixture());

    env.run("var f = new PSBFile(); var ok = f.load('fixture.psb');");

    assert_eq!(env.int("ok"), 1);
    assert_eq!(
        env._storage
            .read("psb://fixture.psb/nested/a")
            .expect("the dictionary-nested chunk resolves"),
        b"FAKEIMAGE".to_vec()
    );
    assert_eq!(
        env._storage
            .read("psb://fixture.psb/nested/b/0")
            .expect("the list-nested chunk resolves"),
        b"FAKEIMAGE".to_vec()
    );
    // The top-level resource stays reachable too.
    assert_eq!(
        env._storage
            .read("psb://fixture.psb/res")
            .expect("the root chunk resolves"),
        b"FAKEIMAGE".to_vec()
    );
}

// A missing storage raises: the reference's `loadPSBFile` calls
// `TVPCreateStream`, which throws out of the call
// (`reference/cpp/plugins/psbfile/PSBFile.cpp:529-534`,
// `reference/cpp/core/base/StorageIntf.cpp:1189-1219`), and the constructor
// factory lets that exception propagate (`reference/cpp/plugins/psbfile/main.cpp:127`).
#[test]
fn load_failure_is_reported() {
    let _lock = vm_lock();
    let env = Env::new();
    assert!(matches!(env.eval_ok("new PSBFile()"), TjsValue::Object));
    let message = env.error("new PSBFile('missing.psb')");
    assert!(message.contains("PSBFile"), "unexpected error: {message}");
    let message = env.error("(new PSBFile()).load('missing.psb')");
    assert!(message.contains("load"), "unexpected error: {message}");
}

// The fixture follows the reference header layout exactly: `PSB\0` magic +
// version/encrypt u16 + little-endian offset table
// (`reference/cpp/plugins/psbfile/PSBHeader.h:8-40`, parsed by
// `parsePSBHeader` at `:54-85`), a v2 header is 40 bytes (`:49-55`), and
// `offsetEncrypt == offsetNames` is precisely the "not encrypted" case of
// `isEncrypted` (`:42-47`). The chunk bytes live at `offsetChunkData` +
// `chunkOffsets[i]` with `chunkLengths[i]` length
// (`reference/cpp/plugins/psbfile/PSBFile.cpp:557-570`).
#[test]
fn fixture_follows_the_reference_header_layout() {
    let fixture = build_fixture();

    assert_eq!(&fixture[0..4], b"PSB\0");
    assert_eq!(u16::from_le_bytes([fixture[4], fixture[5]]), 2);
    assert_eq!(u16::from_le_bytes([fixture[6], fixture[7]]), 0);
    let offset_encrypt = u32::from_le_bytes(fixture[8..12].try_into().unwrap());
    let offset_names = u32::from_le_bytes(fixture[12..16].try_into().unwrap());
    let offset_strings = u32::from_le_bytes(fixture[16..20].try_into().unwrap());
    let offset_chunk_data = u32::from_le_bytes(fixture[32..36].try_into().unwrap());
    let offset_entries = u32::from_le_bytes(fixture[36..40].try_into().unwrap());
    // v2: header length 40, encrypt == names (unencrypted).
    assert_eq!(offset_encrypt, offset_names);
    assert!(offset_names >= 40 && offset_strings > offset_names);
    assert!(offset_entries > offset_chunk_data);
    // The single chunk's bytes sit at `offsetChunkData + chunkOffsets[0]`.
    let chunk_start = offset_chunk_data as usize;
    assert_eq!(&fixture[chunk_start..chunk_start + 9], b"FAKEIMAGE");
}

// `getRoot` only ever returns a dictionary: when the container's root value
// is not a dictionary, `getObjects()` null-casts
// (`reference/cpp/plugins/psbfile/PSBFile.h:97-99`) and the reference still
// returns the (empty) custom object (`reference/cpp/plugins/psbfile/main.cpp:32-42`)
// — never an error and never the scalar itself.
#[test]
fn root_of_a_non_dictionary_container_is_an_empty_dictionary() {
    let _lock = vm_lock();
    let env = Env::new();
    let mut fixture = build_fixture();
    // Rewrite the root value type byte from `Objects` (0x21) to `Null`
    // (0x01) — `reference/cpp/plugins/psbfile/PSBFile.cpp:219-224`.
    let entries_offset = u32::from_le_bytes(fixture[36..40].try_into().unwrap()) as usize;
    fixture[entries_offset] = 0x01;
    env.write_fixture("scalar_root.psb", &fixture);

    env.run(
        "var f = new PSBFile(); \
         var ok = f.load('scalar_root.psb'); \
         var root = f.root; \
         var kind = typeof root; \
         var missing = typeof root.anything;",
    );

    assert_eq!(env.int("ok"), 1);
    assert_eq!(env.string("kind"), "Object");
    assert_eq!(env.string("missing"), "undefined");
}

// `root` builds a fresh dictionary on every read (the reference calls
// `TJSCreateCustomObject()` per `getRoot`, `reference/cpp/plugins/psbfile/main.cpp:32`),
// and before any load the dictionary is empty rather than an error.
#[test]
fn root_is_a_fresh_dictionary_even_before_a_load() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var f = new PSBFile(); \
         var r1 = f.root; \
         var r2 = f.root; \
         var distinct = (r1 !== r2); \
         var kind = typeof r1; \
         var missing = typeof r1.foo;",
    );

    assert_eq!(env.int("distinct"), 1);
    assert_eq!(env.string("kind"), "Object");
    assert_eq!(env.string("missing"), "undefined");
}

// Argument surface of the factory and `load`
// (`reference/cpp/plugins/psbfile/main.cpp:46-79`, `:119-131`): the factory
// accepts 0 args or exactly 1 string (else `TJS_E_INVALIDPARAM`, `:129`),
// `load` demands exactly 1 argument (`TJS_E_BADPARAMCOUNT`, `:51`), rejects
// non-string/non-octet types (`:74`) and answers an octet argument with
// `false` (`:70-72`). The reference factory has no seed parameter, so a
// second constructor argument must raise.
#[test]
fn argument_surface_matches_the_reference() {
    let _lock = vm_lock();
    let env = Env::new();
    env.write_fixture("fixture.psb", &build_fixture());

    // Factory: only 0 args or 1 string.
    env.run("var ok0 = new PSBFile();");
    assert!(matches!(env.eval_ok("ok0 !== null"), TjsValue::Integer(_)));
    for call in [
        "new PSBFile(5)".to_string(),
        "new PSBFile('fixture.psb', 0)".to_string(),
        "new PSBFile('fixture.psb', 'seed')".to_string(),
    ] {
        let error = env
            .engine
            .exec_script(&format!("{call};"), "psb-file-test")
            .err()
            .unwrap_or_else(|| panic!("{call} should have raised"));
        assert!(!error.to_string().is_empty(), "{call}: empty error");
    }

    // `load`: arity and type checks.
    for call in [
        "(new PSBFile()).load()".to_string(),
        "(new PSBFile()).load('fixture.psb', 0)".to_string(),
        "(new PSBFile()).load(5)".to_string(),
    ] {
        let error = env
            .engine
            .exec_script(&format!("{call};"), "psb-file-test")
            .err()
            .unwrap_or_else(|| panic!("{call} should have raised"));
        assert!(!error.to_string().is_empty(), "{call}: empty error");
    }

    // An octet argument answers `false` without touching the document.
    env.run(
        "var f = new PSBFile(); \
         var loaded = f.load('fixture.psb'); \
         var octetResult = f.load(<% 00 01 %>); \
         var stillThere = f.root.num;",
    );
    assert_eq!(env.int("loaded"), 1);
    assert_eq!(env.int("octetResult"), 0);
    assert_eq!(env.int("stillThere"), 1234);
}

// A structurally broken container makes `load` return `false`, exactly like
// the reference `loadPSBFile` → `loadSuccess = false` path
// (`reference/cpp/plugins/psbfile/main.cpp:56-58`,
// `reference/cpp/plugins/psbfile/PSBFile.cpp:546-551`): no exception, and
// an earlier successful document stays intact (`_root` is only replaced on
// success, `PSBFile.cpp:524`).
#[test]
fn parse_failure_returns_false_and_keeps_the_previous_document() {
    let _lock = vm_lock();
    let env = Env::new();
    env.write_fixture("fixture.psb", &build_fixture());
    // `PSB\0` + a v2 header whose tables are all zeros: the string-offset
    // table read fails structurally (`reference/cpp/plugins/psbfile/PSBHeader.h:54-85`).
    let mut garbage = vec![0u8; 40];
    garbage[0..4].copy_from_slice(b"PSB\0");
    garbage[4..6].copy_from_slice(&2u16.to_le_bytes());
    env.write_fixture("garbage.psb", &garbage);

    env.run(
        "var f = new PSBFile(); \
         var first = f.load('fixture.psb'); \
         var second = f.load('garbage.psb'); \
         var num = f.root.num;",
    );

    assert_eq!(env.int("first"), 1);
    assert_eq!(env.int("second"), 0);
    assert_eq!(env.int("num"), 1234);
}

// A version > 3 container is rejected with `false`
// (`reference/cpp/plugins/psbfile/PSBFile.cpp:446-450`:
// "not support psb file format version > 3" → return false).
#[test]
fn version_above_three_returns_false() {
    let _lock = vm_lock();
    let env = Env::new();

    // A v4-sized header (56 bytes) with `offsetEncrypt == offsetNames`
    // (so it does not read as encrypted) and version 4.
    let mut v4 = vec![0u8; 56];
    v4[0..4].copy_from_slice(b"PSB\0");
    v4[4..6].copy_from_slice(&4u16.to_le_bytes());
    v4[8..12].copy_from_slice(&40u32.to_le_bytes());
    v4[12..16].copy_from_slice(&40u32.to_le_bytes());
    env.write_fixture("v4.psb", &v4);

    env.run("var f = new PSBFile(); var ok = f.load('v4.psb');");
    assert_eq!(env.int("ok"), 0);
}

// A bad signature raises: the reference `parsePSBHeader` throws
// "Not a valid PSB file" (`reference/cpp/plugins/psbfile/PSBHeader.h:67-75`)
// and nothing in `loadPSBFile` catches it
// (`reference/cpp/plugins/psbfile/PSBFile.cpp:546`). An `MDF`/`MFL` stream is
// intercepted earlier by `openDecompressedStream`
// (`reference/cpp/plugins/psbfile/PSBStreamDecompress.cpp:37-74`); a
// malformed one makes it return null and `loadPSBFile` reports `false`
// (`PSBFile.cpp:535-543`) — the `"Maybe a MDF file"` header check is never
// reached on the load path.
#[test]
fn bad_signature_raises() {
    let _lock = vm_lock();
    let env = Env::new();
    env.write_fixture("garbage.dat", b"GARBAGE DATA, NOT A PSB");
    env.write_fixture("archive.mdf", b"MDF\0not really an archive....");

    let error = env.error("(new PSBFile()).load('garbage.dat')");
    assert!(error.contains("PSB"), "unexpected error: {error}");
    // An MDF-magic stream goes through `openDecompressedStream`; when its
    // zlib payload cannot be inflated the failure is swallowed and `load`
    // answers `false`, not an exception (`PSBFile.cpp:535-543`).
    assert_eq!(env.int("(new PSBFile()).load('archive.mdf')"), 0);
    // The constructor factory lets the same exception propagate.
    let error = env.error("new PSBFile('garbage.dat')");
    assert!(error.contains("PSB"), "unexpected error: {error}");
}

// Value shapes through `root` (`reference/cpp/plugins/psbfile/PSBValue.cpp:166-199`):
// integers stay `Integer` (`:170-177`), floats become `Real` (`:178-182`),
// and a `PSBArray` becomes a TJS `Array` whose entries narrow to `tjs_int32`
// (`:185-197`), so `root.big[0]` for `0xFFFFFFFF` reads back as `-1`, while
// `root.big` itself is an object.
#[test]
fn value_types_follow_to_tjs_val_conversion() {
    let _lock = vm_lock();
    let env = Env::new();
    env.write_fixture("fixture.psb", &build_fixture());

    env.run(
        "var f = new PSBFile(); \
         f.load('fixture.psb'); \
         var root = f.root; \
         var numKind = typeof root.num; \
         var realKind = typeof root.realv; \
         var strKind = typeof root.str; \
         var arrKind = typeof root.arr; \
         var subKind = typeof root.sub; \
         var realValue = root.realv; \
         var bigValue = root.big[0]; \
         var bigKind = typeof root.big[0];",
    );

    assert_eq!(env.string("numKind"), "Integer");
    assert_eq!(env.string("realKind"), "Real");
    assert_eq!(env.string("strKind"), "String");
    assert_eq!(env.string("arrKind"), "Object");
    assert_eq!(env.string("subKind"), "Object");
    assert_eq!(env.eval_ok("realValue"), TjsValue::Real(3.5));
    assert_eq!(env.int("bigValue"), -1);
    assert_eq!(env.string("bigKind"), "Integer");
}

// Resource values are TJS octets (`reference/cpp/plugins/psbfile/PSBValue.cpp:201-204`):
// `typeof` reports `Octet`, `length` is the chunk size and indexing yields
// the raw bytes (octet member access,
// `reference/cpp/core/tjs2/tjsInterCodeExec.cpp:135-151`).
#[test]
fn resource_members_are_octets() {
    let _lock = vm_lock();
    let env = Env::new();
    env.write_fixture("fixture.psb", &build_fixture());

    env.run(
        "var f = new PSBFile(); \
         f.load('fixture.psb'); \
         var res = f.root.res; \
         var kind = typeof res; \
         var len = res.length; \
         var first = res[0];",
    );

    assert_eq!(env.string("kind"), "Octet");
    assert_eq!(env.int("len"), 9);
    assert_eq!(env.int("first"), i64::from(b'F'));
    // Every byte of the chunk reads back unchanged.
    for (index, expected) in b"FAKEIMAGE".iter().enumerate() {
        assert_eq!(
            env.int(&format!("res[{index}]")),
            i64::from(*expected),
            "byte {index} of the chunk must match"
        );
    }
}

// The octet member is installed natively on the parent object, so the
// parse path never runs a script. The temporary globals that carried the
// parent object and the member name are therefore never created, and an
// absent member reads as `undefined` (the `typeof` of a missing member,
// `reference/cpp/core/tjs2/tjsInterCodeExec.cpp:1898-1904`).
#[test]
fn resource_members_are_installed_without_a_script_round_trip() {
    let _lock = vm_lock();
    let env = Env::new();
    env.write_fixture("fixture.psb", &build_fixture());

    env.run(
        "var f = new PSBFile(); \
         f.load('fixture.psb'); \
         var bufKind = typeof global.__krkr_psb_buf; \
         var keyKind = typeof global.__krkr_psb_key;",
    );

    assert_eq!(env.string("bufKind"), "undefined");
    assert_eq!(env.string("keyKind"), "undefined");
}
