// Self-test input for the void-getter check: the guarded getter must stay
// clean, because it maps the void default to the reference's empty array.
//
// Reference: `reference/cpp/plugins/motionplayer/PlayerCore.cpp:1248-1254`.

pub(crate) struct PlayerInst {
    pub(crate) guarded_badge: StoredValue,
}

impl Default for PlayerInst {
    fn default() -> Self {
        Self {
            guarded_badge: StoredValue::Void,
        }
    }
}

extern "C" fn player_get_guarded_badge(
    engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut std::ffi::c_void,
) -> std::os::raw::c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, super::helpers::NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<PlayerInst>(instance) }) else {
        return report_error(out_error, "Player: null instance");
    };
    let ctx = res.ctx();
    if matches!(&inst.guarded_badge, StoredValue::Void) {
        // The reference returns a fresh empty array while the stored value
        // stays unset, so the property is always an object.
        return match eval_object(&ctx, "[]") {
            Ok(array) => {
                super::helpers::set_retained(out, array);
                0
            }
            Err(error) => report_error(out_error, &error),
        };
    }
    match inst.guarded_badge.to_retained(&ctx) {
        Ok(RetainedValue::Object(value)) => {
            super::helpers::set_retained(out, value);
            0
        }
        Ok(RetainedValue::Value(value)) => {
            super::helpers::set_scalar(&ctx, out, value);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}
