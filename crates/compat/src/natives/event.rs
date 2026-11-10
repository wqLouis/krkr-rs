//! The event object every native hands to the script.
//!
//! A native that fires an event builds the same object: a dictionary that
//! carries the event's `type` and the object the event happened on as
//! `target`. The dictionary is the VM's own dictionary class, so a script
//! sees a `Dictionary` — and the VM allocates it, so no script is compiled
//! to build it.
//!
//! The KAG's tag dictionary is the same kind of object: the parser keeps one
//! dictionary and clears it before every tag.

use std::ffi::c_void;

use tjs::{DetachedValue, Tjs2Engine, TjsValue};

/// Allocate the VM's dictionary object.
///
/// The dictionary is the VM's own dictionary class — the object a script sees
/// as a `Dictionary`.
pub fn event_dictionary(host: &Tjs2Engine) -> Result<DetachedValue, String> {
    host.new_dictionary()
}

/// Build the event object.
///
/// The dictionary carries two members: `type`, the event's name, and
/// `target`, the object the event happened on. The target is written as a
/// variant whose `objthis` is the target object itself, so the script reads
/// the target with itself as the context.
pub fn event_object(
    host: &Tjs2Engine,
    event_type: &str,
    target: *mut c_void,
) -> Result<DetachedValue, String> {
    let event = event_dictionary(host)?;
    host.set_member(
        event.raw_id(),
        "type",
        &TjsValue::String(event_type.to_string()),
    )?;
    set_target(host, &event, target)?;
    Ok(event)
}

/// Write the event's `target` member.
///
/// Every caller names the object the event happened on, so a null target is
/// a situation the callers cannot reach: the member is left out and the
/// script reads it as void.
fn set_target(host: &Tjs2Engine, event: &DetachedValue, target: *mut c_void) -> Result<(), String> {
    if target.is_null() {
        return Ok(());
    }
    let target = host.retain_object_detached(target)?;
    let target_id = target.raw_id().as_ptr() as u64;
    // The write consumes the target's retention; the drop below is the
    // idempotent release either way.
    let result = host.set_member(event.raw_id(), "target", &TjsValue::Retained(target_id));
    drop(target);
    result
}
