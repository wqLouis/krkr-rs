// Property callbacks and method callbacks for `PlayerInst`.
//
// This file is `include!`d into `player.rs` so it shares the payload type and
// helper functions. Keeping the ~130 one-line member callbacks here keeps the
// registration table in `player.rs` readable.

fn string_list(ctx: &CompatCtx, items: &[String]) -> Result<RetainedValue, String> {
    let array = eval_object(ctx, "[]")?;
    // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `string_list`.
    let engine = unsafe { ctx.engine() };
    for (index, item) in items.iter().enumerate() {
        engine
            .set_member(
                array.raw_id(),
                &index.to_string(),
                &TjsValue::String(item.clone()),
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(RetainedValue::Object(array))
}

fn number_pair(ctx: &CompatCtx, first: f64, second: f64) -> Result<RetainedValue, String> {
    let array = eval_object(ctx, "[]")?;
    // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `number_pair`.
    let engine = unsafe { ctx.engine() };
    engine
        .set_member(array.raw_id(), "0", &TjsValue::Real(first))
        .map_err(|error| error.to_string())?;
    engine
        .set_member(array.raw_id(), "1", &TjsValue::Real(second))
        .map_err(|error| error.to_string())?;
    Ok(RetainedValue::Object(array))
}

// -- simple properties ------------------------------------------------------

mp_get_real!(player_get_x, PlayerInst, |p: &PlayerInst| p.coord.x.value()
    as f64);
mp_set_real!(player_set_x, PlayerInst, |p: &mut PlayerInst, v: f64| p
    .coord
    .x
    .set(v as f32));
mp_get_real!(player_get_y, PlayerInst, |p: &PlayerInst| p.coord.y.value()
    as f64);
mp_set_real!(player_set_y, PlayerInst, |p: &mut PlayerInst, v: f64| p
    .coord
    .y
    .set(v as f32));

mp_get_int!(
    player_get_completion,
    PlayerInst,
    |p: &PlayerInst| p.completion_type as i64
);
mp_set_int!(
    player_set_completion,
    PlayerInst,
    |p: &mut PlayerInst, v: i64| p.completion_type = v as i32
);

mp_get_variant!(
    player_get_metadata,
    PlayerInst,
    |p: &PlayerInst, ctx: &CompatCtx| { p.metadata.to_retained(ctx) }
);
mp_set_variant!(
    player_set_metadata,
    PlayerInst,
    |p: &mut PlayerInst, value: StoredValue| {
        p.metadata = value;
    }
);

mp_get_string!(player_get_chara, PlayerInst, |p: &PlayerInst| p
    .chara
    .clone());
mp_set_string!(
    player_set_chara,
    PlayerInst,
    |p: &mut PlayerInst, v: String| {
        p.chara = v;
    }
);

mp_get_string!(player_get_motion_key, PlayerInst, |p: &PlayerInst| p
    .motion_key
    .clone());
mp_set_string!(
    player_set_motion_key,
    PlayerInst,
    |p: &mut PlayerInst, v: String| {
        p.motion_key = v;
    }
);
mp_get_string!(player_get_outline, PlayerInst, |p: &PlayerInst| p
    .outline
    .clone());
mp_set_string!(
    player_set_outline,
    PlayerInst,
    |p: &mut PlayerInst, v: String| {
        p.outline = v;
    }
);

mp_get_real!(player_get_prior_draw, PlayerInst, |p: &PlayerInst| p
    .prior_draw);
mp_set_real!(
    player_set_prior_draw,
    PlayerInst,
    |p: &mut PlayerInst, v: f64| {
        p.prior_draw = v;
    }
);
mp_get_real!(player_get_frame_last, PlayerInst, |p: &PlayerInst| p
    .frame_last_time);
mp_set_real!(
    player_set_frame_last,
    PlayerInst,
    |p: &mut PlayerInst, v: f64| {
        p.frame_last_time = v;
    }
);
mp_get_real!(player_get_frame_loop, PlayerInst, |p: &PlayerInst| p
    .frame_loop_time);
mp_set_real!(
    player_set_frame_loop,
    PlayerInst,
    |p: &mut PlayerInst, v: f64| {
        p.frame_loop_time = v;
    }
);
mp_get_real!(player_get_loop_time, PlayerInst, |p: &PlayerInst| p
    .loop_time);
mp_set_real!(
    player_set_loop_time,
    PlayerInst,
    |p: &mut PlayerInst, v: f64| {
        p.loop_time = v;
    }
);
mp_get_int!(
    player_get_processed,
    PlayerInst,
    |p: &PlayerInst| p.processed_mesh_vertices_num as i64
);
mp_set_int!(
    player_set_processed,
    PlayerInst,
    |p: &mut PlayerInst, v: i64| { p.processed_mesh_vertices_num = v as i32 }
);
mp_get_bool!(player_get_playing, PlayerInst, |p: &PlayerInst| p.playing());
mp_get_bool!(player_get_queuing, PlayerInst, |p: &PlayerInst| p.queuing);
mp_set_bool!(
    player_set_queuing,
    PlayerInst,
    |p: &mut PlayerInst, v: bool| {
        p.queuing = v;
    }
);
mp_get_bool!(player_get_direct_edit, PlayerInst, |p: &PlayerInst| p
    .direct_edit);
mp_set_bool!(
    player_set_direct_edit,
    PlayerInst,
    |p: &mut PlayerInst, v: bool| {
        p.direct_edit = v;
    }
);
mp_get_bool!(player_get_selector_enabled, PlayerInst, |p: &PlayerInst| p
    .selector_enabled);
mp_set_bool!(
    player_set_selector_enabled,
    PlayerInst,
    |p: &mut PlayerInst, v: bool| {
        p.selector_enabled = v;
    }
);
mp_get_variant!(
    player_get_variable_keys,
    PlayerInst,
    |p: &PlayerInst, ctx: &CompatCtx| { p.variable_keys.to_retained(ctx) }
);
mp_set_variant!(
    player_set_variable_keys,
    PlayerInst,
    |p: &mut PlayerInst, value: StoredValue| {
        p.variable_keys = value;
    }
);
mp_get_bool!(player_get_allplaying, PlayerInst, |p: &PlayerInst| p
    .allplaying);
mp_get_bool!(player_get_sync_waiting, PlayerInst, |p: &PlayerInst| p
    .sync_waiting);
mp_set_bool!(
    player_set_sync_waiting,
    PlayerInst,
    |p: &mut PlayerInst, v: bool| {
        p.sync_waiting = v;
    }
);
mp_get_bool!(player_get_sync_active, PlayerInst, |p: &PlayerInst| p
    .sync_active);
mp_set_bool!(
    player_set_sync_active,
    PlayerInst,
    |p: &mut PlayerInst, v: bool| {
        p.sync_active = v;
    }
);
mp_get_bool!(player_get_has_camera, PlayerInst, |p: &PlayerInst| p
    .has_camera);
mp_set_bool!(
    player_set_has_camera,
    PlayerInst,
    |p: &mut PlayerInst, v: bool| {
        p.has_camera = v;
    }
);
mp_get_bool!(player_get_camera_active, PlayerInst, |p: &PlayerInst| p
    .camera_active);
mp_set_bool!(
    player_set_camera_active,
    PlayerInst,
    |p: &mut PlayerInst, v: bool| {
        p.camera_active = v;
    }
);
mp_get_bool!(player_get_stereo, PlayerInst, |p: &PlayerInst| p
    .stereovision_active);
mp_set_bool!(
    player_set_stereo,
    PlayerInst,
    |p: &mut PlayerInst, v: bool| {
        p.stereovision_active = v;
    }
);
mp_get_real!(
    player_get_tick,
    PlayerInst,
    |p: &PlayerInst| if p.frame_tick_count > 0.0 {
        p.frame_tick_count * 1000.0 / 60.0
    } else {
        0.0
    }
);
mp_set_real!(player_set_tick, PlayerInst, |p: &mut PlayerInst, v: f64| {
    p.frame_tick_count = v * 60.0 / 1000.0;
});
mp_get_bool!(player_get_speed, PlayerInst, |p: &PlayerInst| p.speed);
mp_set_bool!(
    player_set_speed,
    PlayerInst,
    |p: &mut PlayerInst, v: bool| {
        p.speed = v;
    }
);
mp_get_real!(player_get_frame_tick, PlayerInst, |p: &PlayerInst| p
    .frame_tick_count);
mp_set_real!(
    player_set_frame_tick,
    PlayerInst,
    |p: &mut PlayerInst, v: f64| {
        p.frame_tick_count = v;
    }
);
mp_get_int!(player_get_mask, PlayerInst, |p: &PlayerInst| p.mask_mode
    as i64);
mp_set_int!(player_set_mask, PlayerInst, |p: &mut PlayerInst, v: i64| {
    p.mask_mode = v as i32
});
mp_get_int!(
    player_get_color_weight,
    PlayerInst,
    |p: &PlayerInst| p.color_weight as i64
);
mp_set_int!(
    player_set_color_weight,
    PlayerInst,
    |p: &mut PlayerInst, v: i64| { p.color_weight = v as u32 }
);
mp_get_bool!(player_get_independent, PlayerInst, |p: &PlayerInst| p
    .independent_layer_inherit);
mp_set_bool!(
    player_set_independent,
    PlayerInst,
    |p: &mut PlayerInst, v: bool| {
        p.independent_layer_inherit = v;
    }
);
mp_get_real!(player_get_z, PlayerInst, |p: &PlayerInst| p.z_factor);
mp_set_real!(player_set_z, PlayerInst, |p: &mut PlayerInst, v: f64| p
    .z_factor =
    v);
mp_get_variant!(
    player_get_camera_target,
    PlayerInst,
    |p: &PlayerInst, ctx: &CompatCtx| p.camera_target.to_retained(ctx)
);
mp_set_variant!(
    player_set_camera_target,
    PlayerInst,
    |p: &mut PlayerInst, value: StoredValue| {
        p.camera_target = value;
    }
);
mp_get_variant!(
    player_get_camera_position,
    PlayerInst,
    |p: &PlayerInst, ctx: &CompatCtx| p.camera_position.to_retained(ctx)
);
mp_set_variant!(
    player_set_camera_position,
    PlayerInst,
    |p: &mut PlayerInst, value: StoredValue| {
        p.camera_position = value;
    }
);
mp_get_real!(player_get_fov, PlayerInst, |p: &PlayerInst| p.camera_fov);
mp_set_real!(player_set_fov, PlayerInst, |p: &mut PlayerInst, v: f64| p
    .camera_fov =
    v);
mp_get_bool!(player_get_camera_alive, PlayerInst, |p: &PlayerInst| p
    .camera_alive);
mp_set_bool!(
    player_set_camera_alive,
    PlayerInst,
    |p: &mut PlayerInst, v: bool| {
        p.camera_alive = v;
    }
);
mp_get_bool!(player_get_canvas_capture, PlayerInst, |p: &PlayerInst| p
    .canvas_capture_enabled);
mp_set_bool!(
    player_set_canvas_capture,
    PlayerInst,
    |p: &mut PlayerInst, v: bool| {
        p.canvas_capture_enabled = v;
    }
);
mp_get_bool!(player_get_clear_enabled, PlayerInst, |p: &PlayerInst| p
    .clear_enabled);
mp_set_bool!(
    player_set_clear_enabled,
    PlayerInst,
    |p: &mut PlayerInst, v: bool| {
        p.clear_enabled = v;
    }
);
mp_get_real!(player_get_hit_threshold, PlayerInst, |p: &PlayerInst| p
    .hit_threshold);
mp_set_real!(
    player_set_hit_threshold,
    PlayerInst,
    |p: &mut PlayerInst, v: f64| {
        p.hit_threshold = v;
    }
);
mp_get_bool!(player_get_preview, PlayerInst, |p: &PlayerInst| p.preview);
mp_set_bool!(
    player_set_preview,
    PlayerInst,
    |p: &mut PlayerInst, v: bool| {
        p.preview = v;
    }
);
mp_get_real!(player_get_outside, PlayerInst, |p: &PlayerInst| p
    .outside_factor);
mp_set_real!(
    player_set_outside,
    PlayerInst,
    |p: &mut PlayerInst, v: f64| {
        p.outside_factor = v;
    }
);
mp_get_variant!(
    player_get_resource_manager,
    PlayerInst,
    |p: &PlayerInst, ctx: &CompatCtx| p.resource_manager.to_retained(ctx)
);
mp_set_variant!(
    player_set_resource_manager,
    PlayerInst,
    |p: &mut PlayerInst, value: StoredValue| {
        p.resource_manager = value;
    }
);
mp_get_string!(player_get_stealth_chara, PlayerInst, |p: &PlayerInst| p
    .stealth_chara
    .clone());
mp_set_string!(
    player_set_stealth_chara,
    PlayerInst,
    |p: &mut PlayerInst, v: String| {
        p.stealth_chara = v;
    }
);
mp_get_string!(player_get_stealth_motion, PlayerInst, |p: &PlayerInst| p
    .stealth_motion
    .clone());
mp_set_string!(
    player_set_stealth_motion,
    PlayerInst,
    |p: &mut PlayerInst, v: String| {
        p.stealth_motion = v;
    }
);
mp_get_variant!(
    player_get_tags,
    PlayerInst,
    |p: &PlayerInst, ctx: &CompatCtx| p.tags.to_retained(ctx)
);
mp_set_variant!(
    player_set_tags,
    PlayerInst,
    |p: &mut PlayerInst, value: StoredValue| {
        p.tags = value;
    }
);
mp_get_variant!(
    player_get_project,
    PlayerInst,
    |p: &PlayerInst, ctx: &CompatCtx| p.project.to_retained(ctx)
);
mp_set_variant!(
    player_set_project,
    PlayerInst,
    |p: &mut PlayerInst, value: StoredValue| {
        p.project = value;
    }
);
mp_get_bool!(player_get_use_d3d, PlayerInst, |p: &PlayerInst| p.use_d3d);
mp_set_bool!(
    player_set_use_d3d,
    PlayerInst,
    |p: &mut PlayerInst, v: bool| {
        p.use_d3d = v;
    }
);
mp_get_string!(player_get_meshline, PlayerInst, |p: &PlayerInst| p
    .meshline
    .clone());
mp_set_string!(
    player_set_meshline,
    PlayerInst,
    |p: &mut PlayerInst, v: String| {
        p.meshline = v;
    }
);
mp_get_bool!(player_get_busy, PlayerInst, |p: &PlayerInst| p.busy);

// -- custom property callbacks ---------------------------------------------

extern "C" fn player_get_motion(
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
    let Some(inst) = (unsafe { super::helpers::inst_ref::<PlayerInst>(instance) }) else {
        return report_error(out_error, "Player: null instance");
    };
    set_string(&res.ctx(), out, &inst.motion_key);
    0
}

extern "C" fn player_set_motion(
    engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    value: *const Value,
    out_error: *mut *mut std::ffi::c_char,
    objthis: *mut std::ffi::c_void,
) -> std::os::raw::c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, super::helpers::NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<PlayerInst>(instance) }) else {
        return report_error(out_error, "Player: null instance");
    };
    // SAFETY: the VM passes a valid value for the duration of the call.
    let motion = if value.is_null() {
        String::new()
    } else {
        value_as_string(unsafe { &*value })
    };
    let ctx = res.ctx();
    // A same-key assignment is a no-op *before* `onFindMotion` runs
    // (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:441-443`).
    if inst.motion_key == motion {
        return 0;
    }
    // `EmotePlayer::setMotion` only stores the clip label: no callback, no
    // state reset (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:167-171`).
    if super::helpers::is_emote_instance(&ctx, objthis) {
        inst.motion_key = motion;
        inst.modified = true;
        return 0;
    }
    // Build `{chara, motion}`, call `objthis.onFindMotion(dict)` — through
    // the object, so a script override wins — and read the *returned*
    // object back: the override may rewrite both fields
    // (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:445-474`).
    let mut motion_value = motion;
    // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `player_set_motion`.
    if let Ok(target) = unsafe { ctx.engine() }.retain_object_detached(objthis) {
        let expression = format!(
            "%[chara: {}, motion: {}]",
            super::helpers::tjs_string_literal(&inst.chara),
            super::helpers::tjs_string_literal(&motion_value)
        );
        if let Ok(RetainedValue::Object(dict)) =
            // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `player_set_motion`.
            unsafe { ctx.engine() }.eval_retained(&expression, "onFindMotion")
        {
            let id = dict.raw_id() as usize as u64;
            // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `player_set_motion`.
            let called = unsafe { ctx.engine() }.call_member(
                target.raw_id(),
                "onFindMotion",
                &[TjsValue::Retained(id)],
            );
            // The call result resolves the engine's most-recent-object slot,
            // so the retain must happen before any other object work.
            if let Ok(result) = called
                && matches!(result, TjsValue::Object)
                // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `player_set_motion`.
                && let Ok(returned) = unsafe { ctx.engine() }.retain_value_detached(&result)
            {
                // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `player_set_motion`.
                if let Ok(chara) = unsafe { ctx.engine() }.member(returned.raw_id(), "chara")
                    && let Some(chara) = readback_string(chara)
                {
                    inst.chara = chara;
                }
                // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `player_set_motion`.
                if let Ok(motion) = unsafe { ctx.engine() }.member(returned.raw_id(), "motion")
                    && let Some(motion) = readback_string(motion)
                {
                    motion_value = motion;
                }
            }
            drop(dict);
        }
    }
    // Reset the playback state and reload
    // (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:474-489`).
    inst.motion_key = motion_value;
    inst.snapshot = None;
    inst.timelines.clear();
    inst.playing_timelines.clear();
    inst.variable_values.clear();
    inst.variable_keys = StoredValue::Void;
    inst.affine = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    inst.ensure_loaded(&ctx);
    0
}

// -- methods ---------------------------------------------------------------

/// Convert an `onFindMotion` read-back member to the reference `ttstr`
/// coercion (strings pass through, numbers use their decimal form, and
/// anything else — including `void` — is not applied
/// (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:458-472`).
fn readback_string(value: TjsValue) -> Option<String> {
    match value {
        TjsValue::String(text) => Some(text),
        TjsValue::Integer(number) => Some(number.to_string()),
        TjsValue::Real(number) => Some(number.to_string()),
        _ => None,
    }
}

/// TJS `AsReal` coercion for a read-back member value.
fn tjs_value_as_f64(value: &TjsValue) -> f64 {
    match value {
        TjsValue::Integer(number) => *number as f64,
        TjsValue::Real(number) => *number,
        TjsValue::String(text) => crate::natives::system::tjs_string_to_real(text),
        _ => 0.0,
    }
}

mp_method_real!(
    player_random,
    PlayerInst,
    |_p: &mut PlayerInst, _args: &[Value], ctx: &CompatCtx| {
        // The reference delegates to the player's `Math.RandomGenerator`
        // and converts its `random()` result to a double — a fresh real in
        // `[0, 1)` per call
        // (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:1318-1332`).
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let services = unsafe { ctx.services() };
        let mut store = services.state::<MotionStore>();
        Ok(store.next_random())
    }
);

mp_method_void!(
    player_init_physics,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `void initPhysics(tTJSVariant metadata)` / `initPhysics(rule)`
        // — ncbind arity 1 on both classes
        // (`reference/cpp/plugins/motionplayer/Player.h:256`,
        // `EmotePlayer.h:141`, `reference/cpp/core/plugin/ncbind.hpp:1178`).
        super::helpers::require_args(args, 1, "initPhysics")?;
        p.metadata = StoredValue::from_value(&args[0], ctx);
        Ok(())
    }
);

mp_method_variant!(
    player_serialize,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], ctx: &CompatCtx| { p.serialize(ctx) }
);

mp_method_void!(
    player_unserialize,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `void unserialize(tTJSVariant data)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:258`).
        super::helpers::require_args(args, 1, "unserialize")?;
        let stored = StoredValue::from_value(&args[0], ctx);
        p.unserialize(ctx, &stored);
        Ok(())
    }
);

extern "C" fn player_set_rotate(
    engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    argc: std::os::raw::c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    objthis: *mut std::ffi::c_void,
) -> std::os::raw::c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, super::helpers::NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<PlayerInst>(instance) }) else {
        return report_error(out_error, "Player: null instance");
    };
    // SAFETY: the trampoline guarantees `argc` valid entries.
    let arguments = unsafe { args(argv, argc) };
    let ctx = res.ctx();
    // The two classes register different entry points: `Player.setRotate`
    // is an `NCB_METHOD` over `setRotate(double, double, double)` and
    // requires all three arguments (`reference/cpp/plugins/
    // motionplayer/Player.h:197,263` + `ncbind.hpp:1178`), while
    // `EmotePlayer.setRot`/`setRotate` is a raw compat that only requires
    // the rotation itself (`reference/cpp/plugins/motionplayer/
    // EmotePlayer.cpp:305-324`).
    let emote = super::helpers::is_emote_instance(&ctx, objthis);
    let required = if emote { 1 } else { 3 };
    if let Err(error) = super::helpers::require_args(arguments, required, "setRotate") {
        return report_error(out_error, &error);
    }
    let rotate = value_as_f64(&arguments[0]);
    let transition = arguments.get(1).map(value_as_f64).unwrap_or(0.0);
    let ease = arguments.get(2).map(value_as_f64).unwrap_or(0.0);
    inst.rotate
        .transition_to(rotate as f32, transition as f32, ease_weight(ease));
    if emote {
        // `EmotePlayer::setRot` marks the shell dirty
        // (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:295-299`).
        inst.modified = true;
    }
    set_void(out);
    0
}

mp_method_void!(
    player_set_mirror,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `void setMirror(bool)` — ncbind arity 1 on both classes
        // (`reference/cpp/plugins/motionplayer/Player.h:266`,
        // `EmotePlayer.h:160`).
        super::helpers::require_args(args, 1, "setMirror")?;
        p.mirror_requested = value_as_bool(&args[0]);
        p.mirror_changed = p.mirror_requested != p.mirror_base;
        // `EmotePlayer::setMirror` marks the shell dirty
        // (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:390-399`).
        p.modified = true;
        Ok(())
    }
);

macro_rules! scale_setter {
    ($name:ident, $field:ident) => {
        mp_method_void!(
            $name,
            PlayerInst,
            |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
                // `void set*Scale(double)` — ncbind arity 1
                // (`reference/cpp/plugins/motionplayer/Player.h:268-270`,
                // `EmotePlayer.h:95-101`, `ncbind.hpp:1178`).
                super::helpers::require_args(args, 1, stringify!($name))?;
                p.$field = value_as_f64(&args[0]);
                p.modified = true;
                Ok(())
            }
        );
    };
}
scale_setter!(player_set_hair_scale, hair_scale);
scale_setter!(player_set_parts_scale, parts_scale);
scale_setter!(player_set_bust_scale, bust_scale);

mp_method_void!(
    player_set_affine,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // Six scalar arguments (none of them void), or a single matrix
        // object; anything else fails —
        // `Player::setDrawAffineTranslateMatrixCompat`
        // (`reference/cpp/plugins/motionplayer/PlayerDrawDispatch.cpp:11-60`).
        if args.len() >= 6 {
            for (index, slot) in p.affine.iter_mut().enumerate() {
                if args[index].ty == tjs::VAL_VOID {
                    return Err("setDrawAffineTranslateMatrix: invalid parameter".into());
                }
                *slot = value_as_f64(&args[index]);
            }
            return Ok(());
        }
        if args.len() == 1 && args[0].ty == tjs::VAL_OBJECT {
            // SAFETY: the argument is a live object for the call.
            let Ok(object) = (unsafe { ctx.engine() }).retain_object_arg(&args[0]) else {
                return Ok(());
            };
            let mut matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
            for (slot, name) in matrix
                .iter_mut()
                .zip(["m11", "m21", "m12", "m22", "m14", "m24"])
            {
                // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `player_set_rotate`.
                if let Ok(value) = unsafe { ctx.engine() }.member(object.raw_id(), name)
                    && !matches!(value, TjsValue::Void)
                {
                    *slot = tjs_value_as_f64(&value);
                }
            }
            p.affine = matrix;
            return Ok(());
        }
        Err("setDrawAffineTranslateMatrix: bad parameter count".into())
    }
);

mp_method_variant!(
    player_get_camera_offset,
    PlayerInst,
    |_p: &mut PlayerInst, _args: &[Value], ctx: &CompatCtx| { number_pair(ctx, 0.0, 0.0) }
);

mp_method_void!(
    player_set_camera_offset,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `void setCameraOffset(tTJSVariant offset)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:279`).
        super::helpers::require_args(args, 1, "setCameraOffset")?;
        p.camera_target = StoredValue::Real(value_as_f64(&args[0]));
        Ok(())
    }
);

mp_method_void!(
    player_modify_root,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `void modifyRoot(tTJSVariant data)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:280`).
        super::helpers::require_args(args, 1, "modifyRoot")?;
        p.metadata = StoredValue::from_value(&args[0], ctx);
        p.ensure_root_offsets();
        Ok(())
    }
);

mp_method_void!(
    player_debug_print,
    PlayerInst,
    |_p: &mut PlayerInst, _args: &[Value], _ctx: &CompatCtx| Ok(())
);

mp_method_void!(
    player_unload,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `void unload(ttstr name)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:298`).
        super::helpers::require_args(args, 1, "unload")?;
        let name = value_as_string(&args[0]);
        p.layer_ids.remove(&name);
        Ok(())
    }
);

mp_method_void!(
    player_unload_all,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], _ctx: &CompatCtx| {
        p.layer_ids.clear();
        p.layer_names.clear();
        p.snapshot = None;
        p.timelines.clear();
        p.playing_timelines.clear();
        Ok(())
    }
);

mp_method_bool!(
    player_is_exist_motion,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `bool isExistMotion(ttstr name)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:300`).
        super::helpers::require_args(args, 1, "isExistMotion")?;
        let name = value_as_string(&args[0]);
        Ok(p.snapshot().is_some_and(|snapshot| {
            snapshot.clip_index.contains_key(&name) || p.timelines.contains_key(&name)
        }))
    }
);

mp_method_variant!(
    player_find_motion,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `tTJSVariant findMotion(ttstr name)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:301`).
        super::helpers::require_args(args, 1, "findMotion")?;
        p.project.to_retained(ctx)
    }
);

mp_method_int!(
    player_require_layer_id,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `tjs_int requireLayerId(ttstr name)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:302`).
        super::helpers::require_args(args, 1, "requireLayerId")?;
        let name = value_as_string(&args[0]);
        Ok(i64::from(p.require_layer_id(&name)))
    }
);

mp_method_void!(
    player_release_layer_id,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `void releaseLayerId(tjs_int id)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:303`).
        super::helpers::require_args(args, 1, "releaseLayerId")?;
        let id = value_as_i64(&args[0]) as i32;
        p.layer_names.remove(&id);
        p.layer_ids.retain(|_, value| *value != id);
        Ok(())
    }
);

mp_method_void!(
    player_set_clear_color,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `void setClearColor(tjs_int color)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:306`).
        super::helpers::require_args(args, 1, "setClearColor")?;
        p.clear_color = value_as_i64(&args[0]) as i32;
        Ok(())
    }
);

mp_method_void!(
    player_set_resizable,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `void setResizable(bool v)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:307`).
        super::helpers::require_args(args, 1, "setResizable")?;
        p.resizable = value_as_bool(&args[0]);
        Ok(())
    }
);

mp_method_void!(
    player_remove_all_textures,
    PlayerInst,
    |_p: &mut PlayerInst, _args: &[Value], _ctx: &CompatCtx| Ok(())
);

mp_method_void!(
    player_remove_all_bg,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], _ctx: &CompatCtx| {
        p.backgrounds.clear();
        Ok(())
    }
);

mp_method_void!(
    player_remove_all_caption,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], _ctx: &CompatCtx| {
        p.captions.clear();
        Ok(())
    }
);

mp_method_void!(
    player_register_bg,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `void registerBg(tTJSVariant bg)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:311`).
        super::helpers::require_args(args, 1, "registerBg")?;
        p.backgrounds.push(StoredValue::from_value(&args[0], ctx));
        Ok(())
    }
);

mp_method_void!(
    player_register_caption,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `void registerCaption(tTJSVariant caption)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:312`).
        super::helpers::require_args(args, 1, "registerCaption")?;
        p.captions.push(StoredValue::from_value(&args[0], ctx));
        Ok(())
    }
);

mp_method_void!(
    player_unload_unused_textures,
    PlayerInst,
    |_p: &mut PlayerInst, _args: &[Value], _ctx: &CompatCtx| Ok(())
);

mp_method_int!(
    player_alpha_op_add,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], _ctx: &CompatCtx| {
        p.next_layer_id += 1;
        Ok(i64::from(p.next_layer_id))
    }
);

mp_method_variant!(
    player_capture_canvas,
    PlayerInst,
    |_p: &mut PlayerInst, _args: &[Value], _ctx: &CompatCtx| Ok(RetainedValue::Value(
        TjsValue::Void
    ))
);

mp_method_string!(
    player_find_source,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `tTJSVariant findSource(ttstr name)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:316`).
        super::helpers::require_args(args, 1, "findSource")?;
        let name = value_as_string(&args[0]);
        Ok(p.layer_ids
            .get(&name)
            .map(|id| id.to_string())
            .unwrap_or_default())
    }
);

mp_method_void!(
    player_load_source,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `void loadSource(ttstr name)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:317`).
        super::helpers::require_args(args, 1, "loadSource")?;
        let name = value_as_string(&args[0]);
        p.require_layer_id(&name);
        Ok(())
    }
);

mp_method_void!(
    player_clear_cache,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], _ctx: &CompatCtx| {
        p.layer_ids.clear();
        p.layer_names.clear();
        Ok(())
    }
);

mp_method_void!(
    player_set_size,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `void setSize(tjs_int w, tjs_int h)` — ncbind arity 2
        // (`reference/cpp/plugins/motionplayer/Player.h:319`).
        super::helpers::require_args(args, 2, "setSize")?;
        p.size = (value_as_i64(&args[0]) as i32, value_as_i64(&args[1]) as i32);
        Ok(())
    }
);

mp_method_void!(
    player_copy_rect,
    PlayerInst,
    |_p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `void copyRect(tTJSVariant args)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:320`).
        super::helpers::require_args(args, 1, "copyRect")?;
        Ok(())
    }
);

mp_method_void!(
    player_adjust_gamma,
    PlayerInst,
    |_p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `void adjustGamma(tTJSVariant args)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:321`).
        super::helpers::require_args(args, 1, "adjustGamma")?;
        Ok(())
    }
);

mp_method_void!(
    player_draw,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // The class registers `draw(tTJSVariant)` — one required argument
        // (`reference/cpp/plugins/motionplayer/main.cpp:234`,
        // `Player.h:322-323`); a scalar argument is a no-op, while an object
        // that is not a native `Layer` is a typed error in `draw`
        // (`reference/cpp/plugins/motionplayer/PlayerDrawDispatch.cpp:109-146`,
        // `PlayerRenderInternal.cpp:334-345`).
        super::helpers::require_args(args, 1, "draw")?;
        let target = StoredValue::from_value(&args[0], ctx);
        p.modified = true;
        p.draw(ctx, &target)?;
        Ok(())
    }
);

mp_method_void!(
    player_frame_progress,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `void frameProgress(double dt)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:324`).
        super::helpers::require_args(args, 1, "frameProgress")?;
        let dt = value_as_f64(&args[0]);
        p.current_frame += dt;
        p.update_layers();
        Ok(())
    }
);

mp_method_void!(
    player_set_flip,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `void setFlip(bool v)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:327`).
        super::helpers::require_args(args, 1, "setFlip")?;
        p.flip = value_as_bool(&args[0]);
        Ok(())
    }
);

mp_method_void!(
    player_set_opacity,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `void setOpacity(double v)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:328`).
        super::helpers::require_args(args, 1, "setOpacity")?;
        p.opacity = value_as_f64(&args[0]);
        Ok(())
    }
);

mp_method_void!(
    player_set_visible,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `void setVisible(bool v)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:329`).
        super::helpers::require_args(args, 1, "setVisible")?;
        p.visible = value_as_bool(&args[0]);
        Ok(())
    }
);

mp_method_void!(
    player_set_slant,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `void setSlant(double v)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:330`).
        super::helpers::require_args(args, 1, "setSlant")?;
        p.slant = value_as_f64(&args[0]);
        Ok(())
    }
);

mp_method_void!(
    player_set_zoom,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `void setZoom(double v)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:331`).
        super::helpers::require_args(args, 1, "setZoom")?;
        p.zoom = value_as_f64(&args[0]);
        Ok(())
    }
);

mp_method_variant!(
    player_get_layer_names,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], ctx: &CompatCtx| {
        let names: Vec<String> = p.nodes.iter().map(|node| node.label.clone()).collect();
        string_list(ctx, &names)
    }
);

mp_method_void!(
    player_release_sync_wait,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], _ctx: &CompatCtx| {
        // `releaseSyncWait` clears both sync flags
        // (`reference/cpp/plugins/motionplayer/PlayerLayerQuery.cpp:118-121`).
        p.sync_waiting = false;
        p.sync_active = false;
        Ok(())
    }
);

mp_method_variant!(
    player_calc_view_param,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], ctx: &CompatCtx| {
        let object = eval_object(ctx, "%[]")?;
        // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for this callback.
        let engine = unsafe { ctx.engine() };
        engine
            .set_member(object.raw_id(), "x", &TjsValue::Real(p.root_x))
            .map_err(|error| error.to_string())?;
        engine
            .set_member(object.raw_id(), "y", &TjsValue::Real(p.root_y))
            .map_err(|error| error.to_string())?;
        engine
            .set_member(object.raw_id(), "zoom", &TjsValue::Real(p.zoom))
            .map_err(|error| error.to_string())?;
        Ok(RetainedValue::Object(object))
    }
);

mp_method_variant!(
    player_get_layer_motion,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `tTJSVariant getLayerMotion(ttstr name)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:335`).
        super::helpers::require_args(args, 1, "getLayerMotion")?;
        let name = value_as_string(&args[0]);
        let node = p.nodes.iter().find(|node| node.label == name);
        Ok(match node {
            Some(node) => RetainedValue::Value(TjsValue::Integer(i64::from(node.node_type))),
            None => RetainedValue::Value(TjsValue::Void),
        })
    }
);

mp_method_variant!(
    player_get_layer_getter,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `tTJSVariant getLayerGetter(ttstr name)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:336`).
        super::helpers::require_args(args, 1, "getLayerGetter")?;
        let name = value_as_string(&args[0]);
        let Some(node) = p.nodes.iter().find(|node| node.label == name) else {
            return Ok(RetainedValue::Value(TjsValue::Void));
        };
        let node = node.clone();
        Ok(RetainedValue::Object(p.layer_getter_object(ctx, &node)?))
    }
);

mp_method_variant!(
    player_get_layer_getter_list,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], ctx: &CompatCtx| {
        let array = eval_object(ctx, "[]")?;
        // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for this callback.
        let engine = unsafe { ctx.engine() };
        for (index, node) in p.nodes.iter().enumerate() {
            let getter = p.layer_getter_object(ctx, node)?;
            let id = getter.raw_id() as usize as u64;
            engine
                .set_member(array.raw_id(), &index.to_string(), &TjsValue::Retained(id))
                .map_err(|error| error.to_string())?;
            drop(getter);
        }
        Ok(RetainedValue::Object(array))
    }
);

/// Seek every timeline to the sync point — the body shared by
/// `Player.skipToSync()` and `EmotePlayer.skip()`
/// (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:12-60`,
/// `EmotePlayer.cpp:800-812`).
fn skip_to_sync(p: &mut PlayerInst) {
    let sync = p
        .snapshot()
        .and_then(|snapshot| {
            snapshot
                .clips
                .iter()
                .map(|clip| clip.total_frames)
                .fold(0.0, f64::max)
                .into()
        })
        .unwrap_or(0.0);
    for state in p.timelines.values_mut() {
        state.current_time = sync;
        if state.total_frames > 0.0 && state.current_time >= state.total_frames && !state.looping {
            state.playing = false;
        }
    }
    p.current_frame = sync;
    p.sync_waiting = false;
    p.sync_active = false;
    p.playing_timelines
        .retain(|label| p.timelines.get(label).is_some_and(|state| state.playing));
    p.allplaying = !p.playing_timelines.is_empty();
}

mp_method_void!(
    player_skip_to_sync,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], ctx: &CompatCtx| {
        // `skipToSync` lazily resolves the active motion first
        // (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:12-13`).
        p.ensure_loaded(ctx);
        skip_to_sync(p);
        Ok(())
    }
);

mp_method_void!(
    player_set_stereo_camera,
    PlayerInst,
    |_p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `void setStereovisionCameraPosition(double x, double y, double z)`
        // — ncbind arity 3
        // (`reference/cpp/plugins/motionplayer/Player.h:339`).
        super::helpers::require_args(args, 3, "setStereovisionCameraPosition")?;
        Ok(())
    }
);

mp_method_real!(
    player_get_variable,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `double getVariable(ttstr label)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:344`); the reference
        // resolves the active motion lazily before reading
        // (`reference/cpp/plugins/motionplayer/PlayerVariable.cpp:539-541`).
        super::helpers::require_args(args, 1, "getVariable")?;
        p.ensure_loaded(ctx);
        let label = value_as_string(&args[0]);
        Ok(p.get_variable_value(&label))
    }
);

mp_method_int!(
    player_count_variables,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], ctx: &CompatCtx| {
        // The reference lazily resolves the active motion first
        // (`reference/cpp/plugins/motionplayer/PlayerVariable.cpp:568-574`).
        p.ensure_loaded(ctx);
        Ok(p.snapshot()
            .map(|snapshot| snapshot.variable_labels.len() as i64)
            .unwrap_or(0))
    }
);

mp_method_string!(
    player_variable_label_at,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `ttstr getVariableLabelAt(tjs_int idx)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:346`); the reference
        // lazily resolves the active motion first
        // (`reference/cpp/plugins/motionplayer/PlayerVariable.cpp:576-577`).
        super::helpers::require_args(args, 1, "getVariableLabelAt")?;
        p.ensure_loaded(ctx);
        let index = value_as_i64(&args[0]).max(0) as usize;
        Ok(p.snapshot()
            .and_then(|snapshot| snapshot.variable_labels.get(index).cloned())
            .unwrap_or_default())
    }
);

mp_method_int!(
    player_count_variable_frame,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `tjs_int countVariableFrameAt(tjs_int idx)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:347`); the reference
        // routes through `getVariableLabelAt`, which loads lazily
        // (`reference/cpp/plugins/motionplayer/PlayerVariable.cpp:577-580`).
        super::helpers::require_args(args, 1, "countVariableFrameAt")?;
        p.ensure_loaded(ctx);
        let index = value_as_i64(&args[0]).max(0) as usize;
        let label = p
            .snapshot()
            .and_then(|snapshot| snapshot.variable_labels.get(index).cloned())
            .unwrap_or_default();
        Ok(p.snapshot()
            .and_then(|snapshot| snapshot.variable_frames.get(&label))
            .map(|frames| frames.len() as i64)
            .unwrap_or(0))
    }
);

mp_method_string!(
    player_variable_frame_label_at,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `ttstr getVariableFrameLabelAt(tjs_int idx, tjs_int frameIdx)` —
        // ncbind arity 2 (`reference/cpp/plugins/motionplayer/Player.h:348`);
        // the reference routes through `getVariableLabelAt`, which loads
        // lazily (`reference/cpp/plugins/motionplayer/PlayerVariable.cpp:576-577`).
        super::helpers::require_args(args, 2, "getVariableFrameLabelAt")?;
        p.ensure_loaded(ctx);
        let index = value_as_i64(&args[0]).max(0) as usize;
        let frame = value_as_i64(&args[1]).max(0) as usize;
        let label = p
            .snapshot()
            .and_then(|snapshot| snapshot.variable_labels.get(index).cloned())
            .unwrap_or_default();
        Ok(p.snapshot()
            .and_then(|snapshot| snapshot.variable_frames.get(&label))
            .and_then(|frames| frames.get(frame))
            .map(|frame| frame.label.clone())
            .unwrap_or_default())
    }
);

mp_method_real!(
    player_variable_frame_value_at,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `double getVariableFrameValueAt(tjs_int idx, tjs_int frameIdx)` —
        // ncbind arity 2 (`reference/cpp/plugins/motionplayer/Player.h:349`);
        // the reference routes through `getVariableLabelAt`, which loads
        // lazily (`reference/cpp/plugins/motionplayer/PlayerVariable.cpp:576-577`).
        super::helpers::require_args(args, 2, "getVariableFrameValueAt")?;
        p.ensure_loaded(ctx);
        let index = value_as_i64(&args[0]).max(0) as usize;
        let frame = value_as_i64(&args[1]).max(0) as usize;
        let label = p
            .snapshot()
            .and_then(|snapshot| snapshot.variable_labels.get(index).cloned())
            .unwrap_or_default();
        Ok(p.snapshot()
            .and_then(|snapshot| snapshot.variable_frames.get(&label))
            .and_then(|frames| frames.get(frame))
            .map(|frame| frame.value)
            .unwrap_or(0.0))
    }
);

mp_method_bool!(
    player_timeline_playing,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `bool getTimelinePlaying(ttstr label)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:350`); the reference
        // lazily resolves the active motion first
        // (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:64-66`).
        super::helpers::require_args(args, 1, "getTimelinePlaying")?;
        p.ensure_loaded(ctx);
        let label = value_as_string(&args[0]);
        Ok(p.timelines.get(&label).is_some_and(|state| state.playing))
    }
);

mp_method_variant!(
    player_variable_range,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `tTJSVariant getVariableRange(ttstr label)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:351`); the reference
        // lazily resolves the active motion first
        // (`reference/cpp/plugins/motionplayer/PlayerVariable.cpp:632-633`).
        super::helpers::require_args(args, 1, "getVariableRange")?;
        p.ensure_loaded(ctx);
        let label = value_as_string(&args[0]);
        match p
            .snapshot()
            .and_then(|snapshot| snapshot.variable_ranges.get(&label))
        {
            Some((min, max)) => number_pair(ctx, *min, *max),
            None => Ok(RetainedValue::Value(TjsValue::Void)),
        }
    }
);

mp_method_variant!(
    player_variable_frame_list,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `tTJSVariant getVariableFrameList(ttstr label)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:352`); the reference
        // lazily resolves the active motion first
        // (`reference/cpp/plugins/motionplayer/PlayerVariable.cpp:647-648`).
        super::helpers::require_args(args, 1, "getVariableFrameList")?;
        p.ensure_loaded(ctx);
        let label = value_as_string(&args[0]);
        let frames = p
            .snapshot()
            .and_then(|snapshot| snapshot.variable_frames.get(&label))
            .cloned()
            .unwrap_or_default();
        let array = eval_object(ctx, "[]")?;
        // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `skip_to_sync`.
        let engine = unsafe { ctx.engine() };
        for (index, frame) in frames.iter().enumerate() {
            let entry = eval_object(ctx, "%[]")?;
            engine
                .set_member(
                    entry.raw_id(),
                    "label",
                    &TjsValue::String(frame.label.clone()),
                )
                .map_err(|error| error.to_string())?;
            engine
                .set_member(entry.raw_id(), "frame", &TjsValue::Real(frame.value))
                .map_err(|error| error.to_string())?;
            engine
                .set_member(entry.raw_id(), "value", &TjsValue::Real(frame.value))
                .map_err(|error| error.to_string())?;
            let id = entry.raw_id() as usize as u64;
            engine
                .set_member(array.raw_id(), &index.to_string(), &TjsValue::Retained(id))
                .map_err(|error| error.to_string())?;
            drop(entry);
        }
        Ok(RetainedValue::Object(array))
    }
);

mp_method_int!(
    player_count_main_timelines,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], ctx: &CompatCtx| {
        // The reference lazily resolves the active motion first
        // (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:75-76`).
        p.ensure_loaded(ctx);
        Ok(p.snapshot()
            .map(|snapshot| snapshot.main_timelines.len() as i64)
            .unwrap_or(0))
    }
);

mp_method_string!(
    player_main_timeline_at,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `ttstr getMainTimelineLabelAt(tjs_int idx)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:354`); the reference
        // lazily resolves the active motion first
        // (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:83-84`).
        super::helpers::require_args(args, 1, "getMainTimelineLabelAt")?;
        p.ensure_loaded(ctx);
        let index = value_as_i64(&args[0]).max(0) as usize;
        Ok(p.snapshot()
            .and_then(|snapshot| snapshot.main_timelines.get(index).cloned())
            .unwrap_or_default())
    }
);

mp_method_variant!(
    player_main_timeline_list,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], ctx: &CompatCtx| {
        // The reference lazily resolves the active motion first
        // (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:94-95`).
        p.ensure_loaded(ctx);
        let labels = p
            .snapshot()
            .map(|snapshot| snapshot.main_timelines.clone())
            .unwrap_or_default();
        string_list(ctx, &labels)
    }
);

mp_method_int!(
    player_count_diff_timelines,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], ctx: &CompatCtx| {
        // The reference lazily resolves the active motion first
        // (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:103-104`).
        p.ensure_loaded(ctx);
        Ok(p.snapshot()
            .map(|snapshot| snapshot.diff_timelines.len() as i64)
            .unwrap_or(0))
    }
);

mp_method_string!(
    player_diff_timeline_at,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `ttstr getDiffTimelineLabelAt(tjs_int idx)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:357`); the reference
        // lazily resolves the active motion first
        // (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:111-112`).
        super::helpers::require_args(args, 1, "getDiffTimelineLabelAt")?;
        p.ensure_loaded(ctx);
        let index = value_as_i64(&args[0]).max(0) as usize;
        Ok(p.snapshot()
            .and_then(|snapshot| snapshot.diff_timelines.get(index).cloned())
            .unwrap_or_default())
    }
);

mp_method_variant!(
    player_diff_timeline_list,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], ctx: &CompatCtx| {
        // The reference lazily resolves the active motion first
        // (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:121-122`).
        p.ensure_loaded(ctx);
        let labels = p
            .snapshot()
            .map(|snapshot| snapshot.diff_timelines.clone())
            .unwrap_or_default();
        string_list(ctx, &labels)
    }
);

mp_method_bool!(
    player_loop_timeline,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `bool getLoopTimeline(ttstr label)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:359`); the reference
        // lazily resolves the active motion first
        // (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:130-131`).
        super::helpers::require_args(args, 1, "getLoopTimeline")?;
        p.ensure_loaded(ctx);
        let label = value_as_string(&args[0]);
        Ok(p.snapshot()
            .and_then(|snapshot| snapshot.loop_timelines.get(&label).copied())
            .unwrap_or(false))
    }
);

mp_method_int!(
    player_count_playing,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], ctx: &CompatCtx| {
        // The reference lazily resolves the active motion first
        // (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:150-151`).
        p.ensure_loaded(ctx);
        Ok(p.playing_timelines.len() as i64)
    }
);

mp_method_string!(
    player_playing_label_at,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `ttstr getPlayingTimelineLabelAt(tjs_int idx)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:361`); the reference
        // lazily resolves the active motion first
        // (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:164-165`).
        super::helpers::require_args(args, 1, "getPlayingTimelineLabelAt")?;
        p.ensure_loaded(ctx);
        let index = value_as_i64(&args[0]).max(0) as usize;
        Ok(p.playing_timelines.get(index).cloned().unwrap_or_default())
    }
);

mp_method_int!(
    player_playing_flags_at,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `tjs_int getPlayingTimelineFlagsAt(tjs_int idx)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:362`); the reference
        // lazily resolves the active motion first
        // (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:177-178`).
        super::helpers::require_args(args, 1, "getPlayingTimelineFlagsAt")?;
        p.ensure_loaded(ctx);
        let index = value_as_i64(&args[0]).max(0) as usize;
        Ok(p.playing_timelines
            .get(index)
            .and_then(|label| p.timelines.get(label))
            .map(|state| i64::from(state.flags))
            .unwrap_or(0))
    }
);

mp_method_int!(
    player_timeline_total_frames,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `tjs_int getTimelineTotalFrameCount(ttstr label)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:363`); the reference
        // lazily resolves the active motion first
        // (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:176-177`).
        super::helpers::require_args(args, 1, "getTimelineTotalFrameCount")?;
        p.ensure_loaded(ctx);
        let label = value_as_string(&args[0]);
        Ok(p.timelines
            .get(&label)
            .map(|state| state.total_frames as i64)
            .or_else(|| {
                p.snapshot()
                    .and_then(|snapshot| snapshot.timeline_total_frames.get(&label))
                    .map(|value| *value as i64)
            })
            .unwrap_or(0))
    }
);

mp_method_void!(
    player_play_timeline,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `void playTimeline(ttstr label, tjs_int flags)` — ncbind arity 2
        // (`reference/cpp/plugins/motionplayer/Player.h:364`).
        super::helpers::require_args(args, 2, "playTimeline")?;
        p.ensure_loaded(ctx);
        let label = value_as_string(&args[0]);
        let flags = value_as_i64(&args[1]) as i32;
        // `EmotePlayer::playTimeline` always marks the shell dirty
        // (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:578-581`).
        p.modified = true;
        // `Player::playTimeline` returns immediately while no motion is
        // active (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:207-210`).
        if p.snapshot.is_some() {
            p.play_timeline(&label, flags);
        }
        Ok(())
    }
);

mp_method_void!(
    player_stop_timeline,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `void stopTimeline(ttstr label)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:365`).
        super::helpers::require_args(args, 1, "stopTimeline")?;
        let label = value_as_string(&args[0]);
        p.stop_timeline(&label);
        Ok(())
    }
);

mp_method_void!(
    player_set_timeline_blend,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `void setTimelineBlendRatio(ttstr label, double ratio)` — ncbind
        // arity 2 (`reference/cpp/plugins/motionplayer/Player.h:366`); the
        // reference lazily resolves the active motion first
        // (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:294-295`)
        // and creates the state through `timelines[key]`.
        super::helpers::require_args(args, 2, "setTimelineBlendRatio")?;
        p.ensure_loaded(ctx);
        let label = value_as_string(&args[0]);
        let ratio = value_as_f64(&args[1]);
        let state = p
            .timelines
            .entry(label.clone())
            .or_insert_with(|| TimelineState::new(0.0, false, -1.0));
        state.blend_ratio = ratio;
        state.blend.set(ratio as f32);
        state.blend_auto_stop = false;
        Ok(())
    }
);

mp_method_real!(
    player_get_timeline_blend,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `double getTimelineBlendRatio(ttstr label)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:367`).
        super::helpers::require_args(args, 1, "getTimelineBlendRatio")?;
        let label = value_as_string(&args[0]);
        Ok(p.timelines
            .get(&label)
            .map(|state| state.blend_ratio)
            .unwrap_or(1.0))
    }
);

extern "C" fn player_fade_in_timeline(
    engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    argc: std::os::raw::c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    objthis: *mut std::ffi::c_void,
) -> std::os::raw::c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, super::helpers::NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<PlayerInst>(instance) }) else {
        return report_error(out_error, "Player: null instance");
    };
    // SAFETY: the trampoline guarantees `argc` valid entries.
    let arguments = unsafe { args(argv, argc) };
    // `void fadeInTimeline(ttstr label, double duration, tjs_int flags)` —
    // ncbind arity 3 on both classes
    // (`reference/cpp/plugins/motionplayer/Player.h:368`,
    // `EmotePlayer.h:213`, `ncbind.hpp:1178`).
    if let Err(error) = super::helpers::require_args(arguments, 3, "fadeInTimeline") {
        return report_error(out_error, &error);
    }
    let label = value_as_string(&arguments[0]);
    let duration = value_as_f64(&arguments[1]);
    let flags = value_as_i64(&arguments[2]) as i32;
    let ctx = res.ctx();
    let emote = super::helpers::is_emote_instance(&ctx, objthis);
    if emote && duration <= 0.0 {
        // The EmotePlayer shell folds a zero-duration fade-in into
        // `playTimeline(label, flags)`
        // (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:601-612`).
        inst.modified = true;
        if inst.snapshot.is_some() {
            inst.play_timeline(&label, flags);
        }
        set_void(out);
        return 0;
    }
    // `Player::fadeInTimeline` starts the timeline with flags 3 when it is
    // not yet playing and blends the blend ratio to 1 over `duration`
    // (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:318-329`).
    let already_playing = inst.playing_timelines.iter().any(|item| item == &label);
    if !already_playing {
        if inst.snapshot.is_some() {
            inst.play_timeline(&label, 3);
        }
        if let Some(state) = inst.timelines.get_mut(&label) {
            state.blend.set(0.0);
        }
    }
    if let Some(state) = inst.timelines.get_mut(&label) {
        state
            .blend
            .transition_to(1.0, duration.max(0.0) as f32, 1.0);
        state.blend_auto_stop = false;
    }
    if emote {
        inst.modified = true;
    }
    set_void(out);
    0
}

extern "C" fn player_fade_out_timeline(
    engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    argc: std::os::raw::c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    objthis: *mut std::ffi::c_void,
) -> std::os::raw::c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, super::helpers::NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<PlayerInst>(instance) }) else {
        return report_error(out_error, "Player: null instance");
    };
    // SAFETY: the trampoline guarantees `argc` valid entries.
    let arguments = unsafe { args(argv, argc) };
    // ncbind arity 3 (`reference/cpp/plugins/motionplayer/Player.h:369`,
    // `EmotePlayer.h:214`, `ncbind.hpp:1178`).
    if let Err(error) = super::helpers::require_args(arguments, 3, "fadeOutTimeline") {
        return report_error(out_error, &error);
    }
    let label = value_as_string(&arguments[0]);
    let duration = value_as_f64(&arguments[1]);
    let ctx = res.ctx();
    let emote = super::helpers::is_emote_instance(&ctx, objthis);
    if emote && duration <= 0.0 {
        // The EmotePlayer shell folds a zero-duration fade-out into
        // `stopTimeline(label)`
        // (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:615-627`).
        inst.modified = true;
        inst.stop_timeline(&label);
        set_void(out);
        return 0;
    }
    // `Player::fadeOutTimeline` blends to 0 over `duration` and auto-stops
    // (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:331-335`).
    if let Some(state) = inst.timelines.get_mut(&label) {
        state
            .blend
            .transition_to(0.0, duration.max(0.0) as f32, 1.0);
        state.blend_auto_stop = true;
    }
    if emote {
        inst.modified = true;
    }
    set_void(out);
    0
}

mp_method_variant!(
    player_timeline_info_list,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], ctx: &CompatCtx| {
        // The reference lazily resolves the active motion first
        // (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:337-338`).
        p.ensure_loaded(ctx);
        p.build_info_list(ctx)
    }
);

mp_method_bool!(
    player_is_selector_target,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `bool isSelectorTarget(ttstr name)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:373`).
        super::helpers::require_args(args, 1, "isSelectorTarget")?;
        let name = value_as_string(&args[0]);
        Ok(p.selector_enabled && !name.is_empty())
    }
);

mp_method_void!(
    player_deactivate_selector_target,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `void deactivateSelectorTarget(ttstr name)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:374`).
        super::helpers::require_args(args, 1, "deactivateSelectorTarget")?;
        let name = value_as_string(&args[0]);
        p.layer_ids.remove(&name);
        Ok(())
    }
);

mp_method_variant!(
    player_command_list,
    PlayerInst,
    |_p: &mut PlayerInst, _args: &[Value], ctx: &CompatCtx| { string_list(ctx, &[]) }
);

mp_method_bool!(
    player_get_d3d,
    PlayerInst,
    |_p: &mut PlayerInst, _args: &[Value], _ctx: &CompatCtx| Ok(true)
);

mp_method_void!(
    player_do_alpha_mask,
    PlayerInst,
    |_p: &mut PlayerInst, _args: &[Value], _ctx: &CompatCtx| Ok(())
);

mp_method_void!(
    player_on_find_motion,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `void onFindMotion(ttstr name, int flags = 0)` registers an
        // ncbind arity of 2 — the default is invisible to the binder, so a
        // one-argument call fails (`reference/cpp/plugins/motionplayer/
        // Player.h:380`, `reference/cpp/core/plugin/ncbind.hpp:1178`). This
        // is also why the `motion` setter's one-argument `FuncCall` skips
        // the native default in the reference
        // (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:452-456`).
        super::helpers::require_args(args, 2, "onFindMotion")?;
        let name = value_as_string(&args[0]);
        let flags = value_as_i64(&args[1]) as i32;
        // `PlayFlagStealth` stores the motion only while no project is
        // bound (`reference/cpp/plugins/motionplayer/PlayerMotionLoad.cpp:20-25`).
        if (flags & 16) != 0 && matches!(p.project, StoredValue::Void) {
            p.stealth_motion = name;
            return Ok(());
        }
        // Same-key requests without force/as-can are ignored
        // (`reference/cpp/plugins/motionplayer/PlayerMotionLoad.cpp:30-34`).
        if p.snapshot.is_some() && p.motion_key == name && (flags & (1 | 4)) == 0 {
            return Ok(());
        }
        // `PlayFlagForce` clears the key so the load runs again
        // (`reference/cpp/plugins/motionplayer/PlayerMotionLoad.cpp:37-40`).
        if (flags & 1) != 0 && p.motion_key == name {
            p.motion_key.clear();
        }
        // Requested key drives the load; the port resolves its snapshot
        // through the project/manager store
        // (`reference/cpp/plugins/motionplayer/PlayerMotionLoad.cpp:47-79`).
        p.motion_key = name.clone();
        p.snapshot = None;
        p.timelines.clear();
        p.playing_timelines.clear();
        p.ensure_loaded(ctx);
        // Prime and start: the requested label wins, otherwise every
        // primary timeline starts with the same flags
        // (`reference/cpp/plugins/motionplayer/PlayerMotionLoad.cpp:97-138`).
        if p.snapshot.is_some() && !p.timelines.is_empty() {
            let cleared = flags & !16;
            if p.timelines.contains_key(&name) {
                p.play_timeline(&name, cleared);
            } else {
                let labels: Vec<String> = p
                    .snapshot()
                    .map(|snapshot| {
                        if snapshot.main_timelines.is_empty() {
                            snapshot.diff_timelines.clone()
                        } else {
                            snapshot.main_timelines.clone()
                        }
                    })
                    .unwrap_or_default();
                p.playing_timelines.clear();
                for label in labels {
                    p.play_timeline(&label, cleared);
                }
            }
        }
        // A pending stealth motion is consumed into `stealthChara`
        // (`reference/cpp/plugins/motionplayer/PlayerMotionLoad.cpp:140-146`).
        if !p.stealth_motion.is_empty() {
            p.stealth_chara = p.chara.clone();
            p.stealth_motion.clear();
        }
        Ok(())
    }
);

mp_method_variant!(
    player_motion_list,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], ctx: &CompatCtx| {
        let labels = p
            .snapshot()
            .map(|snapshot| {
                snapshot
                    .clips
                    .iter()
                    .map(|clip| clip.label.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        string_list(ctx, &labels)
    }
);

mp_method_void!(
    player_emote_edit,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `void emoteEdit(tTJSVariant args)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/Player.h:424`).
        super::helpers::require_args(args, 1, "emoteEdit")?;
        p.metadata = StoredValue::from_value(&args[0], ctx);
        Ok(())
    }
);

// -- EmotePlayer shell members ---------------------------------------------

mp_method_void!(
    player_emote_create,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], _ctx: &CompatCtx| {
        // `create()` is "destroy/reset": it drops the module and the
        // player's motion state but leaves the transform fields alone
        // (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:183-187`).
        p.module = StoredValue::Void;
        p.snapshot = None;
        p.timelines.clear();
        p.playing_timelines.clear();
        p.variable_values.clear();
        p.variable_animators.clear();
        p.modified = true;
        Ok(())
    }
);

mp_method_void!(
    player_emote_load,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `void load(tTJSVariant data)` — ncbind arity 1
        // (`reference/cpp/plugins/motionplayer/EmotePlayer.h:134`); the
        // module is stored and bridged into the playback pipeline
        // (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:189-196`).
        super::helpers::require_args(args, 1, "load")?;
        p.module = StoredValue::from_value(&args[0], ctx);
        p.ensure_loaded(ctx);
        p.modified = true;
        Ok(())
    }
);

mp_method_variant!(
    player_emote_clone,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], ctx: &CompatCtx| {
        // `clone()` builds a fresh `EmotePlayer` adaptor and copies the
        // shell state into it
        // (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:198-253`;
        // `reference/cpp/core/plugin/ncbind.hpp:203` creates it on the
        // `EmotePlayer` class).
        let cloned = eval_object(ctx, "new EmotePlayer(null)")?;
        // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `player_fade_out_timeline`.
        let engine = unsafe { ctx.engine() };
        let source = cloned.raw_id();
        // Identity/transform state travels through the serialize pair.
        if let RetainedValue::Object(state) = p.serialize(ctx)? {
            let id = state.raw_id() as usize as u64;
            // Ownership of the retained id moves into the call argument.
            std::mem::forget(state);
            engine.call_member(source, "unserialize", &[TjsValue::Retained(id)])?;
        }
        // The shell members the reference copies field-by-field
        // (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:204-235`).
        let scalars: [(&str, TjsValue); 16] = [
            ("motionKey", TjsValue::String(p.motion_key.clone())),
            ("motion", TjsValue::String(p.motion_key.clone())),
            ("useD3D", TjsValue::Integer(i64::from(p.use_d3d))),
            ("smoothing", TjsValue::Integer(i64::from(p.smoothing))),
            ("meshDivisionRatio", TjsValue::Real(p.mesh_division_ratio)),
            ("queing", TjsValue::Integer(i64::from(p.queing))),
            ("hairScale", TjsValue::Real(p.hair_scale)),
            ("partsScale", TjsValue::Real(p.parts_scale)),
            ("bustScale", TjsValue::Real(p.bust_scale)),
            ("bodyScale", TjsValue::Real(p.body_scale)),
            ("visible", TjsValue::Integer(i64::from(p.visible))),
            ("drawvisible", TjsValue::Integer(i64::from(p.draw_visible))),
            ("drawOpacity", TjsValue::Real(p.draw_opacity)),
            ("opengl", TjsValue::Integer(i64::from(p.opengl))),
            (
                "playCallback",
                TjsValue::Integer(i64::from(p.play_callback)),
            ),
            ("modified", TjsValue::Integer(i64::from(p.modified))),
        ];
        for (name, value) in scalars {
            engine
                .set_member(source, name, &value)
                .map_err(|error| error.to_string())?;
        }
        // The module object is copied through `load`
        // (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:204`, `:245-249`).
        match p.module.to_retained(ctx)? {
            RetainedValue::Object(module) => {
                let id = module.raw_id() as usize as u64;
                std::mem::forget(module);
                engine.call_member(source, "load", &[TjsValue::Retained(id)])?;
            }
            RetainedValue::Value(value) => {
                engine.call_member(source, "load", &[value])?;
            }
        }
        Ok(RetainedValue::Object(cloned))
    }
);

mp_method_void!(
    player_emote_show,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], _ctx: &CompatCtx| {
        p.visible = true;
        Ok(())
    }
);

mp_method_void!(
    player_emote_hide,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], _ctx: &CompatCtx| {
        p.visible = false;
        Ok(())
    }
);

mp_method_void!(
    player_emote_assign_state,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], _ctx: &CompatCtx| {
        p.ensure_root_offsets();
        // `assignState()` clears the dirty flag when it re-applies the
        // cached shell state
        // (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:264-278`).
        p.modified = false;
        Ok(())
    }
);

mp_method_real!(
    player_emote_get_rot,
    PlayerInst,
    |_p: &mut PlayerInst, _args: &[Value], _ctx: &CompatCtx| {
        // The binary getter returns a hardcoded 0.0
        // (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:325-326`).
        Ok(0.0)
    }
);

mp_method_void!(
    player_emote_set_coord,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `setCoord(double x, double y, ...)` — the compat requires both
        // coordinates (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:345-347`).
        super::helpers::require_args(args, 2, "setCoord")?;
        let x = value_as_f64(&args[0]);
        let y = value_as_f64(&args[1]);
        let time = args.get(2).map(value_as_f64).unwrap_or(0.0);
        let ease = args.get(3).map(value_as_f64).unwrap_or(0.0);
        p.coord
            .x
            .transition_to(x as f32, time as f32, ease_weight(ease));
        p.coord
            .y
            .transition_to(y as f32, time as f32, ease_weight(ease));
        p.modified = true;
        Ok(())
    }
);

mp_method_void!(
    player_emote_set_scale,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `setScale(double s, ...)` — the compat requires the scale
        // (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:375-377`).
        super::helpers::require_args(args, 1, "setScale")?;
        let scale = value_as_f64(&args[0]);
        let time = args.get(1).map(value_as_f64).unwrap_or(0.0);
        let ease = args.get(2).map(value_as_f64).unwrap_or(0.0);
        p.scale
            .transition_to(scale as f32, time as f32, ease_weight(ease));
        p.modified = true;
        Ok(())
    }
);

mp_method_real!(
    player_emote_get_scale,
    PlayerInst,
    |_p: &mut PlayerInst, _args: &[Value], _ctx: &CompatCtx| {
        // The binary getter returns a hardcoded 1.0
        // (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:387-388`).
        Ok(1.0)
    }
);

mp_method_void!(
    player_emote_set_color,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `setColor(tjs_int color, ...)` — the compat requires the colour
        // (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:415-417`).
        super::helpers::require_args(args, 1, "setColor")?;
        let color = value_as_i64(&args[0]) as u32;
        p.color = ColorAnimator::from_packed(color);
        p.modified = true;
        Ok(())
    }
);

mp_method_int!(
    player_emote_get_color,
    PlayerInst,
    |_p: &mut PlayerInst, _args: &[Value], _ctx: &CompatCtx| {
        // The binary getter returns a hardcoded 0
        // (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:426-427`).
        Ok(0)
    }
);

mp_method_void!(
    player_emote_start_wind,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `startWindCompat` demands all five parameters even though the
        // C++ convenience signature defaults the two frequencies
        // (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:492-509`,
        // `EmotePlayer.h:183-185`).
        super::helpers::require_args(args, 5, "startWind")?;
        p.wind.active = true;
        p.wind.min_angle = value_as_f64(&args[0]);
        p.wind.max_angle = value_as_f64(&args[1]);
        p.wind.amplitude = value_as_f64(&args[2]);
        p.wind.freq_x = value_as_f64(&args[3]);
        p.wind.freq_y = value_as_f64(&args[4]);
        p.modified = true;
        Ok(())
    }
);

mp_method_void!(
    player_emote_stop_wind,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], _ctx: &CompatCtx| {
        // `stopWind()` marks the shell dirty
        // (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:511-514`).
        p.wind.active = false;
        p.modified = true;
        Ok(())
    }
);

mp_method_void!(
    player_emote_set_outer_force,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `setOuterForceCompat(label, x, y, transition = 0, ease = 0)` —
        // three required parameters, routed by label
        // (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:853-879`).
        super::helpers::require_args(args, 3, "setOuterForce")?;
        let label = value_as_string(&args[0]).to_ascii_lowercase();
        // The reference keeps three label slots and reads back only
        // `"bust"` through `getOuterForce`
        // (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:1666-1681`).
        // The other two are not reachable from the script surface, so only
        // `bust` is stored; unknown labels are dropped like the reference.
        if label == "bust" {
            p.outer_force.x = value_as_f64(&args[1]);
            p.outer_force.y = value_as_f64(&args[2]);
            p.outer_force.active = true;
        }
        p.modified = true;
        Ok(())
    }
);

mp_method_variant!(
    player_emote_get_outer_force,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], ctx: &CompatCtx| {
        // `getOuterForce()` returns the `bust` entry as a *dictionary*
        // (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:1666-1681`,
        // `EmotePlayer.cpp:881-883`).
        let object = eval_object(ctx, "%[]")?;
        // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for this callback.
        let engine = unsafe { ctx.engine() };
        engine
            .set_member(object.raw_id(), "x", &TjsValue::Real(p.outer_force.x))
            .map_err(|error| error.to_string())?;
        engine
            .set_member(object.raw_id(), "y", &TjsValue::Real(p.outer_force.y))
            .map_err(|error| error.to_string())?;
        Ok(RetainedValue::Object(object))
    }
);

mp_method_void!(
    player_emote_skip,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], ctx: &CompatCtx| {
        // `skip()` delegates to `skipToSync()`, which loads the motion
        // lazily (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:800-812`,
        // `reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:12-13`).
        p.ensure_loaded(ctx);
        skip_to_sync(p);
        p.modified = true;
        Ok(())
    }
);

mp_method_void!(
    player_emote_pass,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], _ctx: &CompatCtx| {
        // `pass()` is parameterless and only releases the sync wait —
        // time advances through `progress(tickStep)` called separately
        // (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:815-819`,
        // `PlayerLayerQuery.cpp:118-121`).
        p.sync_waiting = false;
        p.sync_active = false;
        p.modified = true;
        Ok(())
    }
);

mp_method_void!(
    player_emote_add_play_callback,
    PlayerInst,
    |p: &mut PlayerInst, _args: &[Value], _ctx: &CompatCtx| {
        p.play_callback = true;
        Ok(())
    }
);

mp_method_bool!(
    player_emote_contains,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], _ctx: &CompatCtx| {
        // `containsCompat` accepts `(label, x, y)` or `(x, y)` and fails
        // otherwise (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:913-937`).
        if args.len() >= 3 {
            // (label, x, y): a missing/empty label or an invisible shell
            // never hits (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:906-911`).
            let label = value_as_string(&args[0]);
            if !p.visible || label.is_empty() {
                return Ok(false);
            }
            let x = value_as_f64(&args[1]);
            let y = value_as_f64(&args[2]);
            return Ok(p.hit_test(&label, x, y));
        }
        if args.len() >= 2 {
            // AABB against the active motion size, scaled by the user scale
            // only (`reference/cpp/plugins/motionplayer/EmotePlayer.cpp:885-904`).
            if !p.visible {
                return Ok(false);
            }
            let (width, height) = p
                .snapshot()
                .map(|snapshot| (snapshot.width, snapshot.height))
                .unwrap_or((0.0, 0.0));
            if width <= 0.0 || height <= 0.0 {
                return Ok(false);
            }
            let scale = p.scale.value() as f64;
            let scaled_width = width * scale;
            let scaled_height = height * scale;
            let x = value_as_f64(&args[0]);
            let y = value_as_f64(&args[1]);
            let left = p.coord.x.value() as f64;
            let top = p.coord.y.value() as f64;
            return Ok(x >= left
                && x <= left + scaled_width
                && y >= top
                && y <= top + scaled_height);
        }
        Err("contains: bad parameter count".into())
    }
);

mp_method_void!(
    player_emote_set_timeline,
    PlayerInst,
    |p: &mut PlayerInst, args: &[Value], ctx: &CompatCtx| {
        // `void setTimeline(ttstr label, bool loop)` — ncbind arity 2; the
        // reference ignores the `loop` argument and just starts the
        // timeline (`reference/cpp/plugins/motionplayer/EmotePlayer.h:217`,
        // `EmotePlayer.cpp:633-636`, `PlayerTimeline.cpp:207-210`).
        super::helpers::require_args(args, 2, "setTimeline")?;
        p.ensure_loaded(ctx);
        let label = value_as_string(&args[0]);
        if p.snapshot.is_some() {
            p.play_timeline(&label, 0);
        }
        Ok(())
    }
);

mp_get_variant!(
    player_emote_get_module,
    PlayerInst,
    |p: &PlayerInst, ctx: &CompatCtx| p.module.to_retained(ctx)
);
mp_get_bool!(player_emote_get_smoothing, PlayerInst, |p: &PlayerInst| p
    .smoothing);
mp_set_bool!(
    player_emote_set_smoothing,
    PlayerInst,
    |p: &mut PlayerInst, v: bool| {
        p.smoothing = v;
    }
);
mp_get_real!(
    player_emote_get_mesh_division,
    PlayerInst,
    |p: &PlayerInst| p.mesh_division_ratio
);
mp_set_real!(
    player_emote_set_mesh_division,
    PlayerInst,
    |p: &mut PlayerInst, v: f64| {
        p.mesh_division_ratio = v;
    }
);
mp_get_bool!(player_emote_get_queing, PlayerInst, |p: &PlayerInst| p
    .queing);
mp_set_bool!(
    player_emote_set_queing,
    PlayerInst,
    |p: &mut PlayerInst, v: bool| {
        p.queing = v;
    }
);
mp_get_real!(player_emote_get_body, PlayerInst, |p: &PlayerInst| p
    .body_scale);
mp_set_real!(
    player_emote_set_body,
    PlayerInst,
    |p: &mut PlayerInst, v: f64| {
        p.body_scale = v;
    }
);
mp_get_real!(player_emote_get_hair, PlayerInst, |p: &PlayerInst| p
    .hair_scale);
mp_set_real!(
    player_emote_set_hair,
    PlayerInst,
    |p: &mut PlayerInst, v: f64| {
        p.hair_scale = v;
    }
);
mp_get_real!(player_emote_get_parts, PlayerInst, |p: &PlayerInst| p
    .parts_scale);
mp_set_real!(
    player_emote_set_parts,
    PlayerInst,
    |p: &mut PlayerInst, v: f64| {
        p.parts_scale = v;
    }
);
mp_get_real!(player_emote_get_bust, PlayerInst, |p: &PlayerInst| p
    .bust_scale);
mp_set_real!(
    player_emote_set_bust,
    PlayerInst,
    |p: &mut PlayerInst, v: f64| {
        p.bust_scale = v;
    }
);
mp_get_bool!(player_emote_get_visible, PlayerInst, |p: &PlayerInst| p
    .visible);
mp_set_bool!(
    player_emote_set_visible,
    PlayerInst,
    |p: &mut PlayerInst, v: bool| {
        p.visible = v;
    }
);
mp_get_bool!(player_emote_get_modified, PlayerInst, |p: &PlayerInst| p
    .modified);
mp_set_bool!(
    player_emote_set_modified,
    PlayerInst,
    |p: &mut PlayerInst, v: bool| {
        p.modified = v;
    }
);
mp_get_bool!(
    player_emote_get_draw_visible,
    PlayerInst,
    |p: &PlayerInst| p.draw_visible
);
mp_set_bool!(
    player_emote_set_draw_visible,
    PlayerInst,
    |p: &mut PlayerInst, v: bool| {
        p.draw_visible = v;
    }
);
mp_get_real!(
    player_emote_get_draw_opacity,
    PlayerInst,
    |p: &PlayerInst| p.draw_opacity
);
mp_set_real!(
    player_emote_set_draw_opacity,
    PlayerInst,
    |p: &mut PlayerInst, v: f64| {
        p.draw_opacity = v;
    }
);
mp_get_bool!(player_emote_get_opengl, PlayerInst, |p: &PlayerInst| p
    .opengl);
mp_set_bool!(
    player_emote_set_opengl,
    PlayerInst,
    |p: &mut PlayerInst, v: bool| {
        p.opengl = v;
    }
);
mp_get_bool!(player_emote_get_animating, PlayerInst, |p: &PlayerInst| p
    .allplaying);
mp_get_bool!(
    player_emote_get_play_callback,
    PlayerInst,
    |p: &PlayerInst| p.play_callback
);
