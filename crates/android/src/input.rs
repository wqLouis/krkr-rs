//! Android input events -> platform-neutral [`PlatformEvent`]s.
//!
//! The translation is deliberately thin: it reads the Android key code, motion
//! action and button off the `android-activity` event and hands them to the
//! L1 translation functions in `platform`'s Android backend. No TVP concept is
//! named here; the compatibility layer maps the physical key onto the
//! reference virtual-key table.

use android_activity::input::{
    InputEvent as AndroidInputEvent, KeyAction, KeyEvent, MotionAction, MotionEvent, Pointer,
};
use android_activity::{AndroidApp, InputStatus};

use api::host::TouchPhase;
use platform::{InputEvent, PlatformWindow};

use crate::host::AndroidHost;

/// Drains the activity's queued input into `handler`.
///
/// A queue that cannot be read is logged and skipped; an unreadable queue must
/// not stop the frame loop. Every event is reported as handled so the platform
/// applies no default gesture synthesis of its own.
pub(crate) fn translate_input(app: &AndroidApp, handler: &mut AndroidHost) {
    let mut iter = match app.input_events_iter() {
        Ok(iter) => iter,
        Err(error) => {
            log::error!("cannot read the Android input queue: {error:?}");
            return;
        }
    };
    while iter.next(|event| {
        deliver(handler, event);
        InputStatus::Handled
    }) {}
}

/// Translates one Android input event.
fn deliver(handler: &mut AndroidHost, event: &AndroidInputEvent<'_>) {
    match event {
        AndroidInputEvent::MotionEvent(motion) => deliver_motion(handler, motion),
        AndroidInputEvent::KeyEvent(key) => deliver_key(handler, key),
        // Text and IME actions carry committed characters, not the raw key
        // state the engine's input model consumes; they are not translated.
        AndroidInputEvent::TextEvent(_) | AndroidInputEvent::TextAction(_) => {}
        // `AndroidInputEvent` is non-exhaustive; an event the engine does not
        // consume is ignored rather than guessed at.
        _ => {}
    }
}

/// Translates a motion event into touch, pointer-move or mouse-button input.
///
/// The contact phases are fed to the touch synthesizer ([`AndroidHost::touch`])
/// rather than forwarded as raw touch: KAG reads the mouse event family, and
/// sending both would fire twice (`docs/android.md` §5). A hover move becomes a
/// pointer move; a button press/release becomes a mouse button. An action that
/// is none of these (outside, scroll) is dropped.
fn deliver_motion(handler: &mut AndroidHost, motion: &MotionEvent<'_>) {
    let action = match i32::try_from(u32::from(motion.action())) {
        Ok(action) => action,
        Err(_) => {
            log::warn!("an Android motion action does not fit in i32");
            return;
        }
    };
    if let Some(phase) = PlatformWindow::touch_phase_from_android(action) {
        match phase {
            // A move or a cancel names every active contact; a change of
            // contact set names only the pointer that changed.
            TouchPhase::Moved | TouchPhase::Cancelled => {
                for pointer in motion.pointers() {
                    deliver_touch(handler, &pointer, phase);
                }
            }
            TouchPhase::Started | TouchPhase::Ended => {
                let index = motion.pointer_index();
                if index < motion.pointer_count() {
                    deliver_touch(handler, &motion.pointer_at_index(index), phase);
                } else {
                    log::warn!(
                        "touch action index {index} is out of range for {} pointer(s)",
                        motion.pointer_count()
                    );
                }
            }
        }
        return;
    }

    match motion.action() {
        MotionAction::HoverMove => {
            if let Some(pointer) = motion.pointers().next() {
                handler.send_input(InputEvent::PointerMoved {
                    x: pointer.x(),
                    y: pointer.y(),
                });
            }
        }
        MotionAction::ButtonPress | MotionAction::ButtonRelease => {
            let pressed = matches!(motion.action(), MotionAction::ButtonPress);
            let raw = u32::from(motion.action_button());
            match i32::try_from(raw)
                .ok()
                .and_then(PlatformWindow::mouse_button_from_android)
            {
                Some(button) => handler.send_input(InputEvent::MouseButton { button, pressed }),
                None => log::warn!("unmapped Android mouse button {raw}"),
            }
        }
        _ => {}
    }
}

/// Feeds one Android pointer into the host's touch synthesizer.
fn deliver_touch(handler: &mut AndroidHost, pointer: &Pointer<'_>, phase: TouchPhase) {
    let id = match u64::try_from(pointer.pointer_id()) {
        Ok(id) => id,
        Err(_) => {
            log::warn!(
                "negative Android pointer id {} ignored",
                pointer.pointer_id()
            );
            return;
        }
    };
    handler.touch(id, phase, pointer.x(), pointer.y());
}

/// Translates a key event into [`InputEvent::Key`].
fn deliver_key(handler: &mut AndroidHost, key: &KeyEvent<'_>) {
    let pressed = match key.action() {
        KeyAction::Down => true,
        KeyAction::Up => false,
        _ => return,
    };
    let raw = u32::from(key.key_code());
    let raw = match i32::try_from(raw) {
        Ok(raw) => raw,
        Err(_) => {
            log::warn!("Android key code {raw} does not fit in i32");
            return;
        }
    };
    handler.send_input(InputEvent::Key {
        code: PlatformWindow::physical_key_from_android(raw),
        pressed,
    });
}
