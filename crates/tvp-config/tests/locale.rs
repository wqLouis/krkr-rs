//! Integration tests for the locale message catalogs.

use std::path::{Path, PathBuf};

use tvp_config::{GlobalConfig, LocaleConfig};

fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(rel)
}

/// The fixtures root: catalogs live under `fixtures/locale/<lang>.json`,
/// matching the C++ `locale/` prefix relative to the app/game base dir.
fn fixtures_root() -> PathBuf {
    fixture("")
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tvp-config-locale-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn locale_lookup_with_language_and_fallback() {
    let base = fixtures_root();
    let zh = LocaleConfig::load(&base, "zh_cn").unwrap();
    assert_eq!(zh.lang(), "zh_cn");
    assert!(!zh.is_default_language());
    assert_eq!(zh.get_text("preference_title").as_deref(), Some("全局设置"));
    assert_eq!(
        zh.get_text("preference_output_log").as_deref(),
        Some("打印日志")
    );
    // missing in zh_cn but present in the en_us catalog → fallback text
    assert_eq!(
        zh.get_text("preference_select_renderer").as_deref(),
        Some("Select Renderer")
    );
    // missing everywhere → None (the C++ returned the tid itself)
    assert_eq!(zh.get_text("no_such_tid"), None);

    let en = LocaleConfig::load(&base, "en_us").unwrap();
    assert_eq!(en.lang(), "en_us");
    assert!(en.is_default_language());
    assert_eq!(
        en.get_text("preference_title").as_deref(),
        Some("Global Preference")
    );
    assert_eq!(en.get_text("en_only_key").as_deref(), Some("English Only"));
    assert_eq!(en.get_text("no_such_tid"), None);
}

#[test]
fn locale_missing_language_file_falls_back_to_default() {
    // GetFilePath(): locale/<lang> missing → restore to en_us ("must exist")
    let base = fixtures_root();
    let loc = LocaleConfig::load(&base, "ja_jp").unwrap();
    assert_eq!(loc.lang(), "en_us");
    assert_eq!(
        loc.get_text("preference_title").as_deref(),
        Some("Global Preference")
    );
}

#[test]
fn locale_missing_everything_no_panic() {
    // even the default-language file is missing → empty catalog, no panic
    // (the C++ would recurse forever here; the port does not)
    let base = temp_dir("none");
    let loc = LocaleConfig::load(&base, "ja_jp").unwrap();
    assert_eq!(loc.lang(), "en_us");
    assert!(loc.is_empty());
    assert_eq!(loc.get_text("anything"), None);
}

#[test]
fn locale_flat_object_format() {
    let base = temp_dir("flat");
    std::fs::create_dir_all(base.join("locale")).unwrap();
    std::fs::write(
        base.join("locale/en_us.json"),
        r#"{ "preference_title": "Flat Title", "other": "Other" }"#,
    )
    .unwrap();
    let loc = LocaleConfig::load(&base, "en_us").unwrap();
    assert_eq!(
        loc.get_text("preference_title").as_deref(),
        Some("Flat Title")
    );
    assert_eq!(loc.get_text("other").as_deref(), Some("Other"));
    assert_eq!(loc.len(), 2);
}

#[test]
fn locale_load_with_global_user_language() {
    let base = fixtures_root();
    // global user_language wins over sysLang (C++ Initialize)
    let global = GlobalConfig::new();
    global.set_string("user_language", "zh_cn");
    let loc = LocaleConfig::load_with_global(&base, "en_us", &global).unwrap();
    assert_eq!(loc.lang(), "zh_cn");
    assert_eq!(
        loc.get_text("preference_title").as_deref(),
        Some("全局设置")
    );
    // empty user_language → sysLang
    let global2 = GlobalConfig::new();
    let loc2 = LocaleConfig::load_with_global(&base, "en_us", &global2).unwrap();
    assert_eq!(loc2.lang(), "en_us");
    assert_eq!(
        loc2.get_text("preference_title").as_deref(),
        Some("Global Preference")
    );
    // a missing user_language preference falls back to sysLang too, and is
    // recorded into the global config (C++ GetValue semantics)
    let global3 = GlobalConfig::new();
    let _ = LocaleConfig::load_with_global(&base, "zh_cn", &global3).unwrap();
    assert!(global3.is_value_exist("user_language"));
    assert_eq!(global3.get_string("user_language"), "");
}

#[test]
fn locale_entries_missing_id_or_text_are_skipped() {
    // mirrors the C++ `if (key && val)` skip
    let base = temp_dir("skip");
    std::fs::create_dir_all(base.join("locale")).unwrap();
    std::fs::write(
        base.join("locale/en_us.json"),
        r#"{
            "lang": "en_us",
            "items": [
                { "id": "good", "text": "Good" },
                { "id": "no_text" },
                { "text": "no_id" },
                { "id": 42, "text": "numeric id" },
                { "id": "numeric_text", "text": 7 }
            ]
        }"#,
    )
    .unwrap();
    let loc = LocaleConfig::load(&base, "en_us").unwrap();
    assert_eq!(loc.len(), 2);
    assert_eq!(loc.get_text("good").as_deref(), Some("Good"));
    assert_eq!(loc.get_text("numeric_text").as_deref(), Some("7"));
    assert_eq!(loc.get_text("no_text"), None);
}

#[test]
fn locale_malformed_catalog_is_error() {
    let base = temp_dir("bad");
    std::fs::create_dir_all(base.join("locale")).unwrap();
    std::fs::write(base.join("locale/en_us.json"), "{ not json").unwrap();
    let err = LocaleConfig::load(&base, "en_us").unwrap_err();
    assert!(matches!(err, tvp_config::ConfigError::Parse { .. }));
}
