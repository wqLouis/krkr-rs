//! Integration tests for the global / individual config managers.

use std::path::{Path, PathBuf};

use tvp_config::{ConfigError, GlobalConfig, IndividualConfig, defaults};

fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(rel)
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tvp-config-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

// --- built-in default table (extracted from the C++ call sites) ---

#[test]
fn defaults_table() {
    assert_eq!(defaults::default_integer("fps_limit"), Some(60));
    assert_eq!(
        defaults::default_string("renderer"),
        Some("software".to_owned())
    );
    assert_eq!(defaults::default_boolean("outputlog"), Some(true));
    assert_eq!(defaults::default_boolean("showfps"), Some(false));
    assert_eq!(defaults::default_real("menu_handler_opa"), Some(0.15));
    assert_eq!(defaults::default_real("vcursor_scale"), Some(0.5));
    assert_eq!(
        defaults::default_string("memusage"),
        Some("unlimited".to_owned())
    );
    assert_eq!(
        defaults::default_string("user_language"),
        Some(String::new())
    );
    assert_eq!(defaults::default_integer("no_such_key"), None);
    assert_eq!(defaults::default_for("no_such_key"), None);
}

// --- global config: defaults with no file ---

#[test]
fn global_defaults_no_file() {
    let cfg = GlobalConfig::new();
    assert!(cfg.is_empty());
    assert_eq!(cfg.get_integer("fps_limit"), 60);
    assert!(cfg.get_boolean("outputlog"));
    assert!(!cfg.get_boolean("showfps"));
    assert_eq!(cfg.get_string("renderer"), "software");
    assert_eq!(cfg.get_real("menu_handler_opa"), 0.15);
    assert_eq!(cfg.get_string("user_language"), "");
    // unknown keys → type zero-values (note: each read records the default,
    // so assert the string form before the numeric reads rewrite the key)
    assert_eq!(cfg.get_string("no_such_key"), "");
    assert_eq!(cfg.get_integer("no_such_key"), 0);
    assert_eq!(cfg.get_real("no_such_key"), 0.0);
    assert!(!cfg.get_boolean("no_such_key"));
}

// --- global config: load + typed accessors ---

#[test]
fn global_load_typed_accessors() {
    let cfg = GlobalConfig::load(fixture("global/config.json")).unwrap();
    assert!(!cfg.get_boolean("outputlog"));
    assert!(cfg.get_boolean("showfps"));
    assert_eq!(cfg.get_integer("fps_limit"), 30); // string "30" parsed like atoi
    assert_eq!(cfg.get_real("menu_handler_opa"), 0.3);
    assert_eq!(cfg.get_string("user_language"), "zh_cn");
    assert_eq!(cfg.get_integer("custom_int"), 42);
    assert_eq!(cfg.get_real("custom_float"), 1.5);
    assert!(!cfg.get_boolean("custom_bool"));
    assert_eq!(cfg.get_string("custom_string"), "hello");
    assert!(cfg.get_boolean("string_bool")); // "1" → true
    assert_eq!(cfg.get_integer("string_int"), 42);
    // absent keys fall back to the built-in table
    assert!(cfg.is_value_exist("fps_limit"));
    assert!(!cfg.is_value_exist("renderer")); // checked before any read records it
    assert_eq!(cfg.get_string("renderer"), "software");
}

// --- boolean / int / real / string accessor edge cases ---

#[test]
fn accessor_edge_cases() {
    let cfg = GlobalConfig::load(fixture("edge.json")).unwrap();
    assert!(cfg.get_boolean("bool_true"));
    assert!(!cfg.get_boolean("bool_false"));
    assert!(cfg.get_boolean("bool_num1"));
    assert!(!cfg.get_boolean("bool_num0"));
    assert!(cfg.get_boolean("bool_str1"));
    assert!(!cfg.get_boolean("bool_str0"));
    assert!(cfg.get_boolean("bool_str_true"));
    assert!(!cfg.get_boolean("bool_str_false"));
    assert_eq!(cfg.get_integer("int_str"), 42);
    assert_eq!(cfg.get_integer("int_float"), 3); // floats truncate toward zero
    assert_eq!(cfg.get_integer("int_neg"), -7);
    assert_eq!(cfg.get_real("real_str"), 2.5);
    assert_eq!(cfg.get_real("real_int"), 3.0);
    assert_eq!(cfg.get_string("num_as_string"), "42"); // numbers stringify
    assert_eq!(cfg.get_string("flt_as_string"), "0.5");
    // present-but-garbage values behave like atoi/atof: 0, 0.0, false
    assert_eq!(cfg.get_integer("garbage"), 0);
    assert_eq!(cfg.get_real("garbage"), 0.0);
    assert!(!cfg.get_boolean("garbage"));
    assert_eq!(cfg.get_string("garbage"), "abc"); // strings pass through raw
    assert_eq!(cfg.get_integer("empty_str"), 0);
}

// --- individual config: layered lookup ---

#[test]
fn individual_overrides_global() {
    let global = GlobalConfig::load(fixture("global/config.json")).unwrap();
    let ind = IndividualConfig::use_preference_at(fixture("game"), &global).unwrap();
    // individual wins
    assert_eq!(ind.get_string("renderer"), "opengl");
    assert_eq!(ind.get_integer("software_draw_thread"), 4);
    assert_eq!(ind.get_real("vcursor_scale"), 0.8);
    // else global
    assert_eq!(ind.get_integer("fps_limit"), 30);
    assert_eq!(ind.get_string("user_language"), "zh_cn");
    assert!(!ind.get_boolean("outputlog"));
    // else built-in default
    assert!(ind.get_boolean("keep_screen_alive"));
    assert_eq!(ind.get_string("memusage"), "unlimited");
    assert!(ind.is_value_exist("renderer"));
}

#[test]
fn individual_with_caller_default() {
    let global = GlobalConfig::new();
    let ind = IndividualConfig::create_preference_at(temp_dir("ind-default"), &global);
    assert_eq!(ind.get_integer_with_default("fps_limit", 15), 15);
    assert_eq!(ind.get_string_with_default("renderer", "opengl"), "opengl");
    assert_eq!(ind.get_real_with_default("vcursor_scale", 0.25), 0.25);
    assert!(!ind.get_boolean_with_default("outputlog", false));
}

// --- save / reload round trips ---

#[test]
fn global_save_reload_round_trip() {
    let dir = temp_dir("global-roundtrip");
    let path = dir.join("config.json");
    let cfg = GlobalConfig::new();
    cfg.set_string("renderer", "opengl");
    cfg.set_integer("fps_limit", 30);
    cfg.set_real("menu_handler_opa", 0.35);
    cfg.set_boolean("outputlog", true);
    cfg.set_string("custom_key", "value");
    cfg.save_to(&path).unwrap();
    assert!(path.exists());
    let reloaded = GlobalConfig::load(&path).unwrap();
    assert_eq!(reloaded.get_string("renderer"), "opengl");
    assert_eq!(reloaded.get_integer("fps_limit"), 30);
    assert_eq!(reloaded.get_real("menu_handler_opa"), 0.35);
    assert!(reloaded.get_boolean("outputlog"));
    assert_eq!(reloaded.get_string("custom_key"), "value");
}

#[test]
fn individual_save_reload_round_trip() {
    let dir = temp_dir("ind-roundtrip");
    let global = GlobalConfig::new();
    let ind = IndividualConfig::create_preference_at(&dir, &global);
    ind.set_string("renderer", "opengl");
    ind.set_integer("software_draw_thread", 2);
    ind.save().unwrap();
    assert!(dir.join("config.json").exists());
    let reloaded = IndividualConfig::use_preference_at(&dir, &global).unwrap();
    assert_eq!(reloaded.get_string("renderer"), "opengl");
    assert_eq!(reloaded.get_integer("software_draw_thread"), 2);
}

#[test]
fn load_save_preserves_values_and_clean_save_is_noop() {
    let dir = temp_dir("preserve");
    let dst = dir.join("config.json");
    std::fs::copy(fixture("global/config.json"), &dst).unwrap();
    let cfg = GlobalConfig::load(&dst).unwrap();
    // nothing changed → save is a no-op (C++ ConfigUpdated semantics)
    cfg.save_to(&dst).unwrap();
    let orig: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixture("global/config.json")).unwrap())
            .unwrap();
    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&dst).unwrap()).unwrap();
    assert_eq!(orig, after);
    assert!(!cfg.is_dirty());
    // a modification marks the config dirty and saves
    cfg.set_integer("custom_int", 99);
    assert!(cfg.is_dirty());
    cfg.save_to(&dst).unwrap();
    let reloaded = GlobalConfig::load(&dst).unwrap();
    assert_eq!(reloaded.get_integer("custom_int"), 99);
    assert_eq!(reloaded.get_string("custom_string"), "hello");
}

#[test]
fn missing_read_records_default_then_saves() {
    let dir = temp_dir("record");
    let path = dir.join("config.json");
    let cfg = GlobalConfig::new();
    assert!(!cfg.is_value_exist("fps_limit"));
    let _ = cfg.get_integer("fps_limit"); // records the default, marks dirty
    assert!(cfg.is_value_exist("fps_limit"));
    assert!(cfg.is_dirty());
    cfg.save_to(&path).unwrap();
    let reloaded = GlobalConfig::load(&path).unwrap();
    assert_eq!(reloaded.get_integer("fps_limit"), 60);
}

// --- missing / broken files ---

#[test]
fn missing_file_defaults_no_panic() {
    let dir = temp_dir("missing");
    let cfg = GlobalConfig::load(dir.join("no/such/config.json")).unwrap();
    assert!(cfg.is_empty());
    assert_eq!(cfg.get_integer("fps_limit"), 60);
    assert_eq!(cfg.get_string("renderer"), "software");
    assert!(cfg.get_boolean("outputlog"));

    let global = GlobalConfig::new();
    let ind = IndividualConfig::use_preference_at(dir.join("no-such-game"), &global).unwrap();
    assert!(ind.is_empty());
    assert_eq!(ind.get_string("renderer"), "software");
    assert!(!IndividualConfig::check_exist_at(dir.join("no-such-game")));
}

#[test]
fn malformed_or_non_object_file_is_error() {
    let dir = temp_dir("malformed");
    let bad = dir.join("bad.json");
    std::fs::write(&bad, "{ not json").unwrap();
    let err = GlobalConfig::load(&bad).unwrap_err();
    assert!(matches!(err, ConfigError::Parse { .. }));

    let arr = dir.join("array.json");
    std::fs::write(&arr, "[1, 2, 3]").unwrap();
    let err = GlobalConfig::load(&arr).unwrap_err();
    assert!(matches!(err, ConfigError::NotAnObject { .. }));

    // an empty file loads as an empty config (C++ LoadFile on an empty file
    // leaves AllConfig empty)
    let empty = dir.join("empty.json");
    std::fs::write(&empty, "   ").unwrap();
    let cfg = GlobalConfig::load(&empty).unwrap();
    assert!(cfg.is_empty());
    assert_eq!(cfg.get_integer("fps_limit"), 60);
}

#[test]
fn save_without_path_is_error() {
    let cfg = GlobalConfig::new();
    cfg.set_string("key", "value"); // mark dirty — C++ checks ConfigUpdated first
    let err = cfg.save().unwrap_err();
    assert!(matches!(err, ConfigError::NoPath));
}

// --- round trip of a hand-written file keeps unknown keys ---

#[test]
fn unknown_keys_round_trip() {
    let dir = temp_dir("unknown");
    let path = dir.join("config.json");
    let cfg = GlobalConfig::new();
    cfg.set_integer("custom_int", 42);
    cfg.set_string("custom_string", "hello");
    cfg.save_to(&path).unwrap();
    let reloaded = GlobalConfig::load(&path).unwrap();
    let values = reloaded.values();
    assert_eq!(
        values.get("custom_int").and_then(serde_json::Value::as_i64),
        Some(42)
    );
    assert_eq!(
        values
            .get("custom_string")
            .and_then(serde_json::Value::as_str),
        Some("hello")
    );
}

// --- atoi/atof prefix parsing (C semantics) ---

#[test]
fn atoi_atof_parse_leading_numeric_prefix() {
    let cfg = GlobalConfig::new();
    // C atoi/atof stop at the first non-numeric byte; the JSON port must too.
    cfg.set_string("trailing_int", "42abc");
    cfg.set_string("trailing_real", "1.5x");
    cfg.set_string("leading_ws", "  -12 ");
    cfg.set_string("plus", "+7");
    cfg.set_string("no_digits", "abc");
    cfg.set_string("exp", "1e3zzz");
    cfg.set_string("float_as_int", "1.9");
    assert_eq!(cfg.get_integer("trailing_int"), 42);
    assert_eq!(cfg.get_real("trailing_real"), 1.5);
    assert_eq!(cfg.get_integer("leading_ws"), -12);
    assert_eq!(cfg.get_integer("plus"), 7);
    assert_eq!(cfg.get_integer("no_digits"), 0);
    assert_eq!(cfg.get_real("exp"), 1000.0);
    // atoi stops at the decimal point, unlike a float parse + truncate.
    assert_eq!(cfg.get_integer("float_as_int"), 1);
}
