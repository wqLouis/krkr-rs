//! Integration tests for [`api::host`].

use api::host::Presentation;

#[test]
fn presentation_default() {
    let presentation = Presentation::default();
    assert!(!presentation.presented);
    assert_eq!(presentation.surface_event, None);
}
