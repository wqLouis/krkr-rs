//! `fonts.json` schema + resolution tests.
//!
//! The font configuration is process-global, so these tests hold a shared
//! `TEST_LOCK` and reset the config to `None` around every test. Font *file*
//! tests use system fonts and skip on machines that lack them, mirroring
//! `text_cache.rs`'s "no CJK font" escape hatch.

use std::sync::Mutex;

use tvp_text::{FaceRequest, FontConfig, FontFace, resolve_face, set_font_config};

/// Serializes tests that mutate the process-global font configuration.
static TEST_LOCK: Mutex<()> = Mutex::new(());

const FREE_SANS: &str = "/usr/share/fonts/gnu-free/FreeSans.otf";
const FREE_SERIF: &str = "/usr/share/fonts/gnu-free/FreeSerif.otf";
const NOTO_CJK: &str = "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc";
/// A path that is guaranteed not to exist, exercising the negative cache and
/// the fallback chain's "keep looking" behavior.
const MISSING: &str = "/nonexistent/krkr-rs-font-config-test.ttf";

/// Run `f` with the process-global config reset to `None` afterwards (even on
/// panic, via a drop guard) and the test lock held.
fn with_clean_config(f: impl FnOnce()) {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    set_font_config(None);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    set_font_config(None);
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

fn font_exists(path: &str) -> bool {
    std::path::Path::new(path).is_file()
}

#[test]
fn parses_string_and_object_entries() {
    let json = r#"{
        "faces": {
            "MS Gothic": "/tmp/gothic.ttf",
            "MS 明朝": { "path": "/tmp/mincho.ttc", "index": 1 }
        },
        "fallback": [
            "/tmp/a.ttf",
            { "path": "/tmp/b.ttc", "index": 2 }
        ],
        "allow_system_discovery": true
    }"#;
    let config = FontConfig::from_json_str(json).expect("valid config");
    assert_eq!(config.faces.len(), 2);
    assert_eq!(
        config.faces["MS Gothic"].path().to_str(),
        Some("/tmp/gothic.ttf")
    );
    assert_eq!(config.faces["MS Gothic"].index(), 0);
    assert_eq!(
        config.faces["MS 明朝"].path().to_str(),
        Some("/tmp/mincho.ttc")
    );
    assert_eq!(config.faces["MS 明朝"].index(), 1);

    assert_eq!(config.fallback.len(), 2);
    assert_eq!(config.fallback[0].path().to_str(), Some("/tmp/a.ttf"));
    assert_eq!(config.fallback[0].index(), 0);
    assert_eq!(config.fallback[1].path().to_str(), Some("/tmp/b.ttc"));
    assert_eq!(config.fallback[1].index(), 2);
    assert!(config.allow_system_discovery);
}

#[test]
fn object_entry_index_defaults_to_zero() {
    let config = FontConfig::from_json_str(r#"{ "faces": { "X": { "path": "x.ttf" } } }"#).unwrap();
    assert_eq!(config.faces["X"].index(), 0);
}

#[test]
fn malformed_json_reports_a_clear_error() {
    let err = FontConfig::from_json_str("{ this is not json").unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("malformed font config"),
        "unexpected error message: {message}"
    );
    // A wrong value shape (number where a font entry is expected) must error
    // too, not silently coerce.
    assert!(FontConfig::from_json_str(r#"{ "faces": { "X": 12 } }"#).is_err());
}

#[test]
fn faces_mapping_is_case_insensitive() {
    if !font_exists(FREE_SANS) {
        eprintln!("skipping: {FREE_SANS} not present");
        return;
    }
    with_clean_config(|| {
        let config = FontConfig::from_json_str(&format!(
            r#"{{ "faces": {{ "MS Gothic": "{FREE_SANS}" }} }}"#
        ))
        .unwrap();
        set_font_config(Some(config));

        let lower = resolve_face(&FaceRequest::Named("ms gothic".into())).expect("mapped face");
        let mixed = resolve_face(&FaceRequest::Named("MS Gothic".into())).expect("mapped face");
        assert_eq!(lower.family_name(), Some("FreeSans"));
        assert_eq!(mixed.family_name(), Some("FreeSans"));

        // An unmapped name gets no face (discovery is off by default).
        assert!(
            resolve_face(&FaceRequest::Named("Unknown Face".into())).is_none(),
            "unmapped face must not silently fall back when discovery is disabled"
        );
    });
}

#[test]
fn comma_separated_face_list_matches_tokens() {
    if !font_exists(FREE_SANS) {
        eprintln!("skipping: {FREE_SANS} not present");
        return;
    }
    with_clean_config(|| {
        let config = FontConfig::from_json_str(&format!(
            r#"{{ "faces": {{ "メイリオ": "{FREE_SANS}" }} }}"#
        ))
        .unwrap();
        set_font_config(Some(config));

        // KAG passes `Font.face` as a comma-separated preference list.
        let face = resolve_face(&FaceRequest::Named("Missing,メイリオ,Other".into()))
            .expect("second token must match");
        assert_eq!(face.family_name(), Some("FreeSans"));
    });
}

#[test]
fn at_prefixed_vertical_face_name_resolves_to_base() {
    if !font_exists(FREE_SANS) {
        eprintln!("skipping: {FREE_SANS} not present");
        return;
    }
    with_clean_config(|| {
        let config = FontConfig::from_json_str(&format!(
            r#"{{ "faces": {{ "MS Gothic": "{FREE_SANS}" }} }}"#
        ))
        .unwrap();
        set_font_config(Some(config));

        // The reference vertical-font form is a leading `@`; `TVPFindFont`
        // strips it and looks up the base name (`FontImpl.cpp:282`).
        let vertical = resolve_face(&FaceRequest::Named("@MS Gothic".into()))
            .expect("@-prefixed face must resolve via the base name");
        assert_eq!(vertical.family_name(), Some("FreeSans"));
        assert_eq!(
            resolve_face(&FaceRequest::Named("@MS Gothic".into()))
                .unwrap()
                .family_name(),
            Some("FreeSans")
        );
    });
}

#[test]
fn fallback_chain_uses_first_entry_that_loads() {
    if !font_exists(FREE_SANS) || !font_exists(FREE_SERIF) {
        eprintln!("skipping: GNU FreeFont files not present");
        return;
    }
    with_clean_config(|| {
        // The missing first entry must be skipped; the second loads.
        let config = FontConfig::from_json_str(&format!(
            r#"{{ "fallback": ["{MISSING}", "{FREE_SERIF}", "{FREE_SANS}"] }}"#
        ))
        .unwrap();
        set_font_config(Some(config));
        let face = resolve_face(&FaceRequest::SystemJp).expect("fallback face");
        assert_eq!(face.family_name(), Some("FreeSerif"));

        // Reorder: the first loadable entry changes.
        let config = FontConfig::from_json_str(&format!(
            r#"{{ "fallback": ["{MISSING}", "{FREE_SANS}", "{FREE_SERIF}"] }}"#
        ))
        .unwrap();
        set_font_config(Some(config));
        let face = resolve_face(&FaceRequest::SystemJp).expect("fallback face");
        assert_eq!(face.family_name(), Some("FreeSans"));
    });
}

#[test]
fn explicit_faces_win_over_fallback() {
    if !font_exists(FREE_SANS) || !font_exists(FREE_SERIF) {
        eprintln!("skipping: GNU FreeFont files not present");
        return;
    }
    with_clean_config(|| {
        let config = FontConfig::from_json_str(&format!(
            r#"{{
                "faces": {{ "Named": "{FREE_SANS}" }},
                "fallback": ["{FREE_SERIF}"]
            }}"#
        ))
        .unwrap();
        set_font_config(Some(config));
        assert_eq!(
            resolve_face(&FaceRequest::Named("Named".into()))
                .unwrap()
                .family_name(),
            Some("FreeSans")
        );
        // SystemJp still uses the fallback chain.
        assert_eq!(
            resolve_face(&FaceRequest::SystemJp).unwrap().family_name(),
            Some("FreeSerif")
        );
    });
}

#[test]
fn object_entry_loads_collection_index() {
    if !font_exists(NOTO_CJK) {
        eprintln!("skipping: {NOTO_CJK} not present");
        return;
    }
    with_clean_config(|| {
        let config = FontConfig::from_json_str(&format!(
            r#"{{ "faces": {{ "CJK": {{ "path": "{NOTO_CJK}", "index": 1 }} }} }}"#
        ))
        .unwrap();
        set_font_config(Some(config));
        let face = resolve_face(&FaceRequest::Named("CJK".into())).expect("mapped collection face");
        assert_eq!(face.collection_index(), 1);
    });
}

#[test]
fn allow_system_discovery_gates_the_system_scan() {
    with_clean_config(|| {
        let config = FontConfig::from_json_str(r#"{ "allow_system_discovery": false }"#).unwrap();
        set_font_config(Some(config));
        assert!(
            resolve_face(&FaceRequest::Named("Unmapped".into())).is_none(),
            "discovery=false must not scan the system"
        );
        assert!(
            resolve_face(&FaceRequest::SystemJp).is_none(),
            "discovery=false must not scan the system for the default face"
        );

        let config = FontConfig::from_json_str(r#"{ "allow_system_discovery": true }"#).unwrap();
        set_font_config(Some(config));
        // With discovery on, the config behaves like no-config for these
        // requests; compare presence against a direct discovery probe.
        let expected = FontFace::discover_system_jp().is_some();
        assert_eq!(
            resolve_face(&FaceRequest::Named("Unmapped".into())).is_some(),
            expected
        );
        assert_eq!(resolve_face(&FaceRequest::SystemJp).is_some(), expected);
    });
}

#[test]
fn no_config_keeps_system_discovery() {
    with_clean_config(|| {
        // No config installed: the port's original out-of-the-box behavior.
        let named = resolve_face(&FaceRequest::Named("Anything".into()));
        let system = resolve_face(&FaceRequest::SystemJp);
        let expected = FontFace::discover_system_jp().is_some();
        assert_eq!(system.is_some(), expected, "SystemJp must keep discovery");
        assert_eq!(named.is_some(), expected, "Named must keep discovery");
    });
}

#[test]
fn config_change_invalidates_the_face_cache() {
    if !font_exists(FREE_SANS) || !font_exists(FREE_SERIF) {
        eprintln!("skipping: GNU FreeFont files not present");
        return;
    }
    with_clean_config(|| {
        let request = FaceRequest::Named("Same".into());

        let config =
            FontConfig::from_json_str(&format!(r#"{{ "faces": {{ "Same": "{FREE_SANS}" }} }}"#))
                .unwrap();
        set_font_config(Some(config));
        assert_eq!(
            resolve_face(&request).unwrap().family_name(),
            Some("FreeSans")
        );

        // Installing a new config must invalidate the cached resolution.
        let config =
            FontConfig::from_json_str(&format!(r#"{{ "faces": {{ "Same": "{FREE_SERIF}" }} }}"#))
                .unwrap();
        set_font_config(Some(config));
        assert_eq!(
            resolve_face(&request).unwrap().family_name(),
            Some("FreeSerif")
        );
    });
}
