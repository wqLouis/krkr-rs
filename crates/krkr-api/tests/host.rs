//! Integration tests for [`krkr_api::host`].

use krkr_api::host::*;

#[test]
fn key_state_press_release_and_bounds() {
    let mut keys = KeyState::new();
    assert!(keys.is_empty());
    assert!(!keys.is_down(10));

    keys.press(10);
    keys.press(200);
    assert!(keys.is_down(10));
    assert!(keys.is_down(200));
    assert_eq!(keys.iter_down().collect::<Vec<_>>(), vec![10, 200]);

    keys.release(10);
    assert!(!keys.is_down(10));
    assert_eq!(keys.iter_down().collect::<Vec<_>>(), vec![200]);

    keys.press(MAX_KEY_CODE);
    assert!(!keys.is_down(MAX_KEY_CODE));

    keys.clear();
    assert!(keys.is_empty());
}

#[test]
fn input_state_default() {
    let input = InputState::default();
    assert_eq!(input.mouse_pos, (0.0, 0.0));
    assert_eq!(input.mouse_buttons, [false; 3]);
    assert!(input.touch.is_empty());
    assert!(input.text_input.is_empty());
}

#[test]
fn presentation_default() {
    let presentation = Presentation::default();
    assert!(!presentation.presented);
    assert_eq!(presentation.surface_event, None);
}
