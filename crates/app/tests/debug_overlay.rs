//! Integration tests for the debug overlay's render-only node.
//!
//! The overlay is enabled by `HostConfig::debug_overlay`; it must append
//! exactly one texture node to the projected scene and nothing a script can
//! reach. These tests drive the public host surface only.

use app::{Host, HostConfig, InjectedFrameTime};
use render::RenderContent;

/// Number of texture nodes in the host's projected scene.
fn texture_nodes(host: &Host) -> usize {
    host.render_scene()
        .nodes
        .iter()
        .filter(|node| matches!(&node.content, RenderContent::Texture(_)))
        .count()
}

#[test]
fn overlay_node_present_only_when_enabled() {
    let mut disabled = Host::new(HostConfig::headless()).expect("headless host");
    disabled.step(&InjectedFrameTime::new(16.0)).expect("step");
    assert!(!disabled.debug_overlay_enabled());
    assert_eq!(texture_nodes(&disabled), 0);

    let config = HostConfig::headless().with_debug_overlay(true);
    let mut enabled = Host::new(config).expect("headless host");
    enabled.step(&InjectedFrameTime::new(16.0)).expect("step");
    assert!(enabled.debug_overlay_enabled());
    assert_eq!(texture_nodes(&enabled), 1);

    // A frame with an unchanged scene revision must reuse the single overlay
    // node instead of appending another.
    enabled.step(&InjectedFrameTime::new(16.0)).expect("step");
    assert_eq!(texture_nodes(&enabled), 1);
}

#[test]
fn overlay_texture_is_uploaded_before_it_is_drawn() {
    let config = HostConfig::headless().with_debug_overlay(true);
    let mut host = Host::new(config).expect("headless host");
    let stats = host.step(&InjectedFrameTime::new(16.0)).expect("step").stats.expect("null stats");
    assert_eq!(stats.texture_draws, 1);
    assert_eq!(stats.missing_textures, 0);
}
