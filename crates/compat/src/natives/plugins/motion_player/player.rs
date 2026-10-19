//! `Motion.Player` / top-level `Player` — the reference player core.
//!
//! Behavioural reference: `reference/cpp/plugins/motionplayer/Player.h`,
//! `PlayerCore.cpp`, `PlayerTimeline.cpp`, `PlayerVariable.cpp`,
//! `PlayerFrameProgress.cpp`, `PlayerUpdate*.cpp` and `PlayerLayerQuery.cpp`.
//!
//! The port keeps the whole script-visible surface and the playback model
//! (timeline state, variable animators, coordinate/scale/rotate/color
//! transitions, wind / outer force, hit testing, layer queries). Node-tree
//! evaluation and drawing compose the L2 `engine_api::Scene` layer system:
//! [`PlayerInst::draw`] writes the root transform onto the target layer, then
//! maps each node's icon pixels onto its transformed quad with
//! `engine_api::layer_ops`, decoding the container's `RL`/palettized pixel
//! chunks itself.
#![allow(unsafe_code)]

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use tjs::{DetachedValue, RetainedValue, TjsValue, VAL_OBJECT, Value};

use super::StoredValue;
use super::adaptors::SeparateInst;
use super::animator::{Animator, ColorAnimator, ease_weight};
use super::helpers::{eval_object, inst_mut, inst_ref, set_string, value_as_f64};
use super::model::{
    FrameState, IconInfo, MotionEvent, MotionNode, MotionSnapshot, TimelineState, build_node_tree,
    evaluate_frames, prime_timelines, step_timelines,
};
use super::resource::{MotionStore, ResourceInst};
use super::texture::{Pixels, decode_pixels};
use crate::engine_api::layer_ops::{AffineBlit, BlitOptions, BlitSource, RectI, affine_blit};
use crate::engine_api::{BitmapState, Storage, StorageReadError};
use crate::native_util::{args, report_error, set_bool, set_void, value_as_string};
use crate::natives::system::{resolve, value_as_bool, value_as_i64};
use crate::storage::strip_file_media;
use crate::{CompatCtx, CompatError};

/// A pending coordinate transition (`EmoteCoordState`).
#[derive(Debug, Clone, Default)]
pub(crate) struct CoordState {
    x: Animator,
    y: Animator,
}

/// Wind state (`WindState`).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct WindState {
    active: bool,
    min_angle: f64,
    max_angle: f64,
    amplitude: f64,
    freq_x: f64,
    freq_y: f64,
    phase: f64,
}

/// Per-region outer force (`OuterForceState`).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct OuterForce {
    active: bool,
    x: f64,
    y: f64,
}

/// A motion resolved for one player: the parsed snapshot, its engine-store id
/// and the storage path it was loaded from.
#[derive(Clone)]
pub(crate) struct ResolvedMotion {
    pub(crate) snapshot: Arc<MotionSnapshot>,
    pub(crate) id: u64,
    pub(crate) path: String,
}

/// `Player` instance payload.
pub(crate) struct PlayerInst {
    // Core properties.
    pub(crate) completion_type: i32,
    pub(crate) metadata: StoredValue,
    pub(crate) chara: String,
    pub(crate) motion_key: String,
    pub(crate) outline: String,
    pub(crate) prior_draw: f64,
    pub(crate) frame_last_time: f64,
    pub(crate) frame_loop_time: f64,
    pub(crate) loop_time: f64,
    pub(crate) processed_mesh_vertices_num: i32,
    pub(crate) allplaying: bool,
    pub(crate) queuing: bool,
    pub(crate) direct_edit: bool,
    pub(crate) selector_enabled: bool,
    pub(crate) variable_keys: StoredValue,
    pub(crate) sync_waiting: bool,
    pub(crate) sync_active: bool,
    pub(crate) has_camera: bool,
    pub(crate) camera_active: bool,
    pub(crate) stereovision_active: bool,
    pub(crate) frame_tick_count: f64,
    pub(crate) speed: bool,
    pub(crate) mask_mode: i32,
    pub(crate) color_weight: u32,
    pub(crate) independent_layer_inherit: bool,
    pub(crate) z_factor: f64,
    pub(crate) camera_target: StoredValue,
    pub(crate) camera_position: StoredValue,
    pub(crate) camera_fov: f64,
    pub(crate) camera_alive: bool,
    pub(crate) canvas_capture_enabled: bool,
    pub(crate) clear_enabled: bool,
    pub(crate) hit_threshold: f64,
    pub(crate) preview: bool,
    pub(crate) outside_factor: f64,
    pub(crate) resource_manager: StoredValue,
    pub(crate) stealth_chara: String,
    pub(crate) stealth_motion: String,
    pub(crate) tags: StoredValue,
    pub(crate) project: StoredValue,
    pub(crate) use_d3d: bool,
    pub(crate) meshline: String,
    pub(crate) busy: bool,

    // Runtime.
    pub(crate) snapshot: Option<Arc<MotionSnapshot>>,
    /// Motions resolved by name or path, keyed by both.
    pub(crate) motions_by_key: HashMap<String, ResolvedMotion>,
    pub(crate) timelines: HashMap<String, TimelineState>,
    pub(crate) playing_timelines: Vec<String>,
    pub(crate) variable_values: HashMap<String, f64>,
    pub(crate) variable_animators: HashMap<String, Animator>,
    pub(crate) nodes: Vec<MotionNode>,
    pub(crate) current_frame: f64,
    pub(crate) pending_events: Vec<MotionEvent>,
    /// The decoded pixel chunks the active motion draws, keyed by their
    /// `psb://` storage name.
    pub(crate) textures: HashMap<String, Arc<Pixels>>,
    /// The `src` keys already reported as having no readable texture, so a
    /// missing icon is reported once instead of on every frame.
    pub(crate) missing_textures: BTreeSet<String>,

    // Emote transform state.
    pub(crate) coord: CoordState,
    pub(crate) scale: Animator,
    pub(crate) rotate: Animator,
    pub(crate) color: ColorAnimator,
    pub(crate) mirror_base: bool,
    pub(crate) mirror_requested: bool,
    pub(crate) mirror_changed: bool,
    pub(crate) hair_scale: f64,
    pub(crate) parts_scale: f64,
    pub(crate) bust_scale: f64,
    pub(crate) body_scale: f64,
    pub(crate) mesh_division_ratio: f64,
    /// EmotePlayer shell state.
    pub(crate) module: StoredValue,
    pub(crate) smoothing: bool,
    pub(crate) queing: bool,
    pub(crate) modified: bool,
    pub(crate) draw_visible: bool,
    pub(crate) draw_opacity: f64,
    pub(crate) opengl: bool,
    pub(crate) play_callback: bool,
    pub(crate) wind: WindState,
    pub(crate) outer_force: OuterForce,
    pub(crate) affine: [f64; 6],

    // Root / viewport.
    pub(crate) root_x: f64,
    pub(crate) root_y: f64,
    pub(crate) visible: bool,
    pub(crate) opacity: f64,
    pub(crate) flip: bool,
    pub(crate) slant: f64,
    pub(crate) zoom: f64,
    pub(crate) clear_color: i32,
    pub(crate) resizable: bool,
    pub(crate) size: (i32, i32),
    pub(crate) backgrounds: Vec<StoredValue>,
    pub(crate) captions: Vec<StoredValue>,

    // Layer ids.
    pub(crate) layer_ids: HashMap<String, i32>,
    pub(crate) layer_names: HashMap<i32, String>,
    pub(crate) next_layer_id: i32,
}

impl Default for PlayerInst {
    fn default() -> Self {
        Self {
            completion_type: 0,
            metadata: StoredValue::Void,
            chara: String::new(),
            motion_key: String::new(),
            outline: String::new(),
            prior_draw: 0.0,
            frame_last_time: 0.0,
            frame_loop_time: 0.0,
            loop_time: 0.0,
            processed_mesh_vertices_num: 0,
            allplaying: false,
            queuing: false,
            direct_edit: false,
            selector_enabled: false,
            variable_keys: StoredValue::Void,
            sync_waiting: false,
            sync_active: false,
            has_camera: false,
            camera_active: false,
            stereovision_active: false,
            frame_tick_count: 0.0,
            speed: true,
            mask_mode: 0,
            color_weight: 0xFF80_8080,
            independent_layer_inherit: false,
            z_factor: 1.0,
            camera_target: StoredValue::Void,
            camera_position: StoredValue::Void,
            camera_fov: 60.0,
            camera_alive: false,
            canvas_capture_enabled: false,
            clear_enabled: false,
            hit_threshold: 0.0,
            preview: false,
            outside_factor: 0.0,
            resource_manager: StoredValue::Void,
            stealth_chara: String::new(),
            stealth_motion: String::new(),
            tags: StoredValue::Void,
            project: StoredValue::Void,
            use_d3d: false,
            meshline: String::new(),
            busy: false,
            snapshot: None,
            motions_by_key: HashMap::new(),
            timelines: HashMap::new(),
            playing_timelines: Vec::new(),
            variable_values: HashMap::new(),
            variable_animators: HashMap::new(),
            nodes: Vec::new(),
            current_frame: 0.0,
            pending_events: Vec::new(),
            textures: HashMap::new(),
            missing_textures: BTreeSet::new(),
            coord: CoordState::default(),
            scale: Animator::at(1.0),
            rotate: Animator::at(0.0),
            color: ColorAnimator::default(),
            mirror_base: false,
            mirror_requested: false,
            mirror_changed: false,
            hair_scale: 1.0,
            parts_scale: 1.0,
            bust_scale: 1.0,
            body_scale: 1.0,
            mesh_division_ratio: 1.0,
            module: StoredValue::Void,
            smoothing: true,
            queing: false,
            modified: false,
            draw_visible: true,
            draw_opacity: 1.0,
            opengl: false,
            play_callback: false,
            wind: WindState::default(),
            outer_force: OuterForce::default(),
            affine: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            root_x: 0.0,
            root_y: 0.0,
            visible: true,
            opacity: 1.0,
            flip: false,
            slant: 0.0,
            zoom: 1.0,
            clear_color: 0,
            resizable: false,
            size: (0, 0),
            backgrounds: Vec::new(),
            captions: Vec::new(),
            layer_ids: HashMap::new(),
            layer_names: HashMap::new(),
            next_layer_id: 0,
        }
    }
}

impl PlayerInst {
    pub(crate) fn playing(&self) -> bool {
        self.allplaying
    }

    pub(crate) fn snapshot(&self) -> Option<&MotionSnapshot> {
        self.snapshot.as_deref()
    }

    /// Resolve `name` to a parsed motion, loading and caching it on first use.
    pub(crate) fn resolve_motion(&mut self, name: &str, ctx: &CompatCtx) -> Option<ResolvedMotion> {
        if name.is_empty() {
            return None;
        }
        if let Some(cached) = self.motions_by_key.get(name) {
            return Some(cached.clone());
        }
        let candidates = motion_lookup_candidates(name);
        match placed_motion_path(ctx, &candidates) {
            Ok(Some(path)) => {
                if let Some(cached) = self.motions_by_key.get(&path) {
                    let cached = cached.clone();
                    self.motions_by_key.insert(name.to_string(), cached.clone());
                    return Some(cached);
                }
                match super::resource::read_motion_module(ctx, &path) {
                    Ok(Some((id, snapshot, module))) => {
                        drop(module);
                        let resolved = ResolvedMotion {
                            snapshot,
                            id,
                            path: path.clone(),
                        };
                        self.motions_by_key
                            .insert(name.to_string(), resolved.clone());
                        self.motions_by_key.insert(path, resolved.clone());
                        return Some(resolved);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        log::warn!("motion_player: could not load motion {path:?}: {error}");
                    }
                }
            }
            Ok(None) => {}
            Err(error) => {
                log::warn!("motion_player: motion path lookup failed: {error}");
            }
        }
        if let Some(manager) = self.resource_manager.object_id() {
            for candidate in candidates {
                match load_manager_module(ctx, manager, &candidate) {
                    Ok(Some((id, snapshot))) => {
                        let resolved = ResolvedMotion {
                            snapshot,
                            id,
                            path: candidate.clone(),
                        };
                        self.motions_by_key
                            .insert(name.to_string(), resolved.clone());
                        self.motions_by_key.insert(candidate, resolved.clone());
                        return Some(resolved);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        log::warn!("motion_player: could not load motion {candidate:?}: {error}");
                    }
                }
            }
        }
        None
    }

    /// Load the motion named by `project`, the manager's last module or
    /// `motionKey`, if none is active yet.
    pub(crate) fn ensure_motion_loaded(&mut self, ctx: &CompatCtx) -> Result<bool, String> {
        if self.snapshot.is_some() {
            return Ok(true);
        }
        if let Some(id) = self.project_snapshot_id(ctx)
            && let Some(resolved) = self.resolved_from_store(ctx, id)
        {
            self.activate_motion(&resolved, ctx)?;
            return Ok(true);
        }
        if looks_like_storage(&self.motion_key) {
            let key = self.motion_key.clone();
            if let Some(resolved) = self.resolve_motion(&key, ctx) {
                self.activate_motion(&resolved, ctx)?;
                return Ok(true);
            }
        }
        if let Some(id) = self.resource_manager_snapshot_id(ctx)
            && let Some(resolved) = self.resolved_from_store(ctx, id)
        {
            self.activate_motion(&resolved, ctx)?;
            return Ok(true);
        }
        if self.motion_key.is_empty() {
            return Ok(false);
        }
        let key = self.motion_key.clone();
        if let Some(resolved) = self.resolve_motion(&key, ctx) {
            self.activate_motion(&resolved, ctx)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Bind a motion key and load it: the per-motion state is reset, then one
    /// lazy load runs through [`Self::ensure_motion_loaded`].
    pub(crate) fn bind_motion_key(&mut self, key: &str, ctx: &CompatCtx) -> Result<(), String> {
        if self.motion_key == key {
            return Ok(());
        }
        self.motion_key = key.to_string();
        self.reset_motion_state();
        self.ensure_motion_loaded(ctx)?;
        Ok(())
    }

    /// Drop the per-motion runtime state before another motion is bound.
    pub(crate) fn reset_motion_state(&mut self) {
        self.snapshot = None;
        self.timelines.clear();
        self.playing_timelines.clear();
        self.variable_keys = StoredValue::Void;
        self.variable_values.clear();
        self.variable_animators.clear();
        self.affine = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        self.textures.clear();
        self.missing_textures.clear();
    }

    /// Activate a resolved motion and rebuild the timeline, node and variable
    /// state it owns.
    fn activate_motion(
        &mut self,
        resolved: &ResolvedMotion,
        ctx: &CompatCtx,
    ) -> Result<(), String> {
        self.snapshot = Some(resolved.snapshot.clone());
        self.timelines.clear();
        self.playing_timelines.clear();
        prime_timelines(&resolved.snapshot, &mut self.timelines);
        self.nodes = build_node_tree(&resolved.snapshot, None);
        for label in &resolved.snapshot.variable_labels {
            self.variable_values
                .entry(label.clone())
                .or_insert_with(|| default_variable(&resolved.snapshot, label));
        }
        self.loop_time = resolved
            .snapshot
            .clips
            .first()
            .map(|clip| clip.loop_time)
            .unwrap_or(-1.0);
        self.sync_variable_keys(ctx)
    }

    /// Replace `variableKeys` with the active motion's labels, or an empty
    /// array when no motion is active.
    pub(crate) fn sync_variable_keys(&mut self, ctx: &CompatCtx) -> Result<(), String> {
        let labels = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.variable_labels.clone())
            .unwrap_or_default();
        let array = eval_object(ctx, "[]")?;
        // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `sync_variable_keys`.
        let engine = unsafe { ctx.engine() };
        for (index, label) in labels.iter().enumerate() {
            engine
                .set_member(
                    array.raw_id(),
                    &index.to_string(),
                    &TjsValue::String(label.clone()),
                )
                .map_err(|error| error.to_string())?;
        }
        self.variable_keys = StoredValue::Object(array);
        Ok(())
    }

    /// Activate a snapshot directly, replacing the current motion.
    pub(crate) fn load_from_snapshot(
        &mut self,
        resolved: &ResolvedMotion,
        ctx: &CompatCtx,
    ) -> Result<(), String> {
        self.reset_motion_state();
        self.activate_motion(resolved, ctx)
    }

    /// Drop the cached motions named by `name`, or loaded from that path.
    pub(crate) fn unload_motion(&mut self, name: &str) {
        if name.is_empty() {
            return;
        }
        let active = self.snapshot.as_ref().map(Arc::as_ptr);
        let mut removed_active = false;
        self.motions_by_key.retain(|key, resolved| {
            if key != name && resolved.path != name {
                return true;
            }
            if active == Some(Arc::as_ptr(&resolved.snapshot)) {
                removed_active = true;
            }
            false
        });
        if removed_active {
            self.snapshot = None;
            self.timelines.clear();
            self.playing_timelines.clear();
            self.textures.clear();
            self.missing_textures.clear();
        }
    }

    /// Drop every cached motion and the bound manager's cache.
    pub(crate) fn unload_all(&mut self, ctx: &CompatCtx) {
        self.motions_by_key.clear();
        self.snapshot = None;
        self.timelines.clear();
        self.playing_timelines.clear();
        self.layer_ids.clear();
        self.layer_names.clear();
        self.variable_keys = StoredValue::Void;
        self.variable_values.clear();
        self.variable_animators.clear();
        self.affine = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        self.textures.clear();
        self.missing_textures.clear();
        self.motion_key.clear();
        if let Some(manager) = self.resource_manager.object_id() {
            clear_manager_cache(ctx, manager);
        }
    }

    /// The module object of a resolved motion, rebuilt when the engine
    /// registry no longer holds it.
    pub(crate) fn motion_module(
        &self,
        resolved: &ResolvedMotion,
        ctx: &CompatCtx,
    ) -> Result<RetainedValue, String> {
        let registered = {
            // SAFETY: the caller resolved `ctx` from the installed binding.
            let services = unsafe { ctx.services() };
            let store = services.state::<MotionStore>();
            store.module(resolved.id).and_then(StoredValue::object_id)
        };
        if let Some(id) = registered {
            // SAFETY: the engine store retains the registered module object.
            let value = unsafe { ctx.engine() }
                .duplicate_retained(tjs::Tjs2ValueId::from_raw(id as *mut std::ffi::c_void))
                .map_err(|error| error.to_string())?;
            return Ok(RetainedValue::Object(value));
        }
        super::resource::module_for_snapshot(ctx, resolved.id, &resolved.path, &resolved.snapshot)
            .map(RetainedValue::Object)
    }

    fn resolved_from_store(&self, ctx: &CompatCtx, id: u64) -> Option<ResolvedMotion> {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let services = unsafe { ctx.services() };
        let store = services.state::<MotionStore>();
        let snapshot = store.get(id)?;
        let path = store.path(id).unwrap_or_default().to_string();
        Some(ResolvedMotion { snapshot, id, path })
    }

    fn resource_manager_snapshot_id(&self, ctx: &CompatCtx) -> Option<u64> {
        let manager = self.resource_manager.object_id()?;
        // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `resource_manager_snapshot_id`.
        let engine = unsafe { ctx.engine() };
        let payload = match engine.native_instance_payload(
            tjs::Tjs2ValueId::from_raw(manager as *mut std::ffi::c_void),
            "ResourceManager",
        ) {
            Ok(payload) => payload,
            Err(error) => {
                log::warn!("motion_player: resource manager lookup failed: {error}");
                return None;
            }
        };
        let payload = payload?;
        // SAFETY: the manager object is alive for this call, so its native payload is too.
        let instance = unsafe { inst_ref::<ResourceInst>(payload.as_ptr()) }?;
        module_snapshot_id(ctx, instance.last_module()?)
    }

    pub(crate) fn project_snapshot_id(&self, ctx: &CompatCtx) -> Option<u64> {
        module_snapshot_id(ctx, &self.project)
    }

    pub(crate) fn require_layer_id(&mut self, name: &str) -> i32 {
        if let Some(id) = self.layer_ids.get(name) {
            return *id;
        }
        self.next_layer_id += 1;
        let id = self.next_layer_id;
        self.layer_ids.insert(name.to_string(), id);
        self.layer_names.insert(id, name.to_string());
        id
    }

    /// Advance everything by `dt_ms` milliseconds.
    pub(crate) fn progress(&mut self, dt_ms: f64, ctx: &CompatCtx) {
        if !self.speed {
            return;
        }
        let frames = dt_ms * 60.0 / 1000.0;
        self.frame_tick_count += frames;
        self.current_frame += frames;
        self.coord.x.step(frames as f32);
        self.coord.y.step(frames as f32);
        self.scale.step(frames as f32);
        self.rotate.step(frames as f32);
        if self.wind.active {
            self.wind.phase += frames;
        }
        self.busy = false;
        self.update_layers();
        self.step_animators(frames as f32);
        let events = if let Some(snapshot) = &self.snapshot {
            step_timelines(snapshot, &mut self.timelines, frames)
        } else {
            Vec::new()
        };
        self.allplaying = self.timelines.values().any(|state| state.playing);
        self.pending_events.extend(events);
        self.dispatch_events(ctx);
    }

    pub(crate) fn step_animators(&mut self, dt: f32) {
        for animator in self.variable_animators.values_mut() {
            animator.step(dt);
        }
    }

    /// Evaluate the node tree at the current frame time.
    pub(crate) fn update_layers(&mut self) {
        let time = self.current_frame;
        // Evaluate each node's local state.
        for index in 0..self.nodes.len() {
            let state = evaluate_frames(&self.nodes[index].frames, time);
            let node = &mut self.nodes[index];
            node.local = state;
            node.draw_flag = true;
        }
        // Accumulate parent transforms into world coordinates.
        for index in 1..self.nodes.len() {
            let parent = self.nodes[index].parent;
            let local = self.nodes[index].local.clone();
            let parent_state = self.nodes[parent].accumulated.clone();
            let (px, py) = (parent_state.x, parent_state.y);
            let node = &mut self.nodes[index];
            node.accumulated = local.clone();
            node.accumulated.x = px + local.x;
            node.accumulated.y = py + local.y;
            node.accumulated.z = parent_state.z + local.z;
            node.accumulated.opacity = parent_state.opacity * local.opacity;
            node.accumulated.angle = parent_state.angle + local.angle;
            node.accumulated.scale_x = parent_state.scale_x * local.scale_x;
            node.accumulated.scale_y = parent_state.scale_y * local.scale_y;
            node.draw_flag = parent_state.active && local.active && parent_state.opacity > 0.0;
        }
        if self.nodes.is_empty() {
            return;
        }
        let root = &mut self.nodes[0];
        root.accumulated.x = root.local.x + self.root_x;
        root.accumulated.y = root.local.y + self.root_y;
        root.accumulated.active = self.visible;
        root.accumulated.opacity = self.opacity;
        root.draw_flag = self.visible;
        let vertices = self.nodes.iter().filter(|node| node.draw_flag).count();
        self.processed_mesh_vertices_num = vertices as i32;
    }

    pub(crate) fn dispatch_events(&mut self, ctx: &CompatCtx) {
        if self.pending_events.is_empty() {
            return;
        }
        let events = std::mem::take(&mut self.pending_events);
        for event in events {
            let _ = (event, ctx);
        }
    }

    pub(crate) fn get_variable_value(&self, label: &str) -> f64 {
        if let Some(value) = self.variable_values.get(label) {
            return *value;
        }
        if let Some(snapshot) = &self.snapshot {
            if let Some(first) = snapshot
                .variable_frames
                .get(label)
                .and_then(|frames| frames.first())
            {
                return first.value;
            }
            if let Some(range) = snapshot.variable_ranges.get(label) {
                return range.0;
            }
        }
        0.0
    }

    pub(crate) fn stop_timeline(&mut self, label: &str) {
        if label.is_empty() {
            for state in self.timelines.values_mut() {
                state.playing = false;
                state.blend_ratio = 1.0;
                state.blend = Animator::at(1.0);
            }
            self.playing_timelines.clear();
        } else {
            if let Some(state) = self.timelines.get_mut(label) {
                state.playing = false;
                state.blend_ratio = 1.0;
                state.blend = Animator::at(1.0);
            }
            self.playing_timelines.retain(|item| item != label);
        }
        self.allplaying = !self.playing_timelines.is_empty();
    }

    pub(crate) fn play_timeline(&mut self, label: &str, flags: i32) {
        if (flags & 1) != 0 {
            self.stop_timeline("");
        }
        let Some(state) = self.timelines.get_mut(label) else {
            return;
        };
        state.flags = flags;
        state.playing = true;
        state.current_time = 0.0;
        state.blend_ratio = 1.0;
        state.blend = Animator::at(1.0);
        if !self.playing_timelines.iter().any(|item| item == label) {
            self.playing_timelines.push(label.to_string());
        }
        self.allplaying = true;
    }

    pub(crate) fn play(
        &mut self,
        label: &str,
        flags: i32,
        ctx: &CompatCtx,
    ) -> Result<bool, String> {
        self.ensure_motion_loaded(ctx)?;
        if !label.is_empty() {
            if (flags & 16) != 0 {
                self.stealth_motion = label.to_string();
            } else if self.snapshot.is_some() {
                self.motion_key = label.to_string();
            }
        }
        if self.snapshot.is_none() && !label.is_empty() {
            self.bind_motion_key(label, ctx)?;
        }
        let Some(snapshot) = self.snapshot.clone() else {
            return Ok(false);
        };
        if (flags & 1) != 0 {
            self.stop_timeline("");
        }
        let started = if !label.is_empty() && self.timelines.contains_key(label) {
            self.play_timeline(label, flags);
            true
        } else {
            let labels: Vec<String> = if snapshot.main_timelines.is_empty() {
                snapshot.diff_timelines.clone()
            } else {
                snapshot.main_timelines.clone()
            };
            if labels.is_empty() {
                false
            } else {
                for item in labels {
                    self.play_timeline(&item, flags);
                }
                true
            }
        };
        if started && !label.is_empty() {
            self.play_clip_layers(&snapshot, label);
            self.rebuild_tree(&snapshot, label);
        }
        self.allplaying = started;
        Ok(started)
    }

    pub(crate) fn play_clip_layers(&mut self, snapshot: &MotionSnapshot, label: &str) {
        if let Some(clip) = snapshot.clip(label) {
            self.loop_time = clip.loop_time;
            self.frame_last_time = clip.total_frames;
        }
    }

    pub(crate) fn rebuild_tree(&mut self, snapshot: &MotionSnapshot, label: &str) {
        self.nodes = build_node_tree(snapshot, snapshot.clip(label));
        self.ensure_root_offsets();
    }

    pub(crate) fn ensure_root_offsets(&mut self) {
        if let Some(root) = self.nodes.first_mut() {
            root.accumulated.scale_x = self.scale.value() as f64;
            root.accumulated.scale_y = self.scale.value() as f64;
            root.accumulated.angle = self.rotate.value() as f64;
            root.accumulated.x = self.coord.x.value() as f64 + self.root_x;
            root.accumulated.y = self.coord.y.value() as f64 + self.root_y;
            root.accumulated.opacity = self.opacity * self.color.bytes[3] as f64;
        }
    }

    pub(crate) fn layer_getter_object(
        &self,
        ctx: &CompatCtx,
        node: &MotionNode,
    ) -> Result<DetachedValue, String> {
        let object = eval_object(ctx, "%[]")?;
        // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `layer_getter_object`.
        let engine = unsafe { ctx.engine() };
        let set = |key: &str, value: TjsValue| -> Result<(), String> {
            engine
                .set_member(object.raw_id(), key, &value)
                .map_err(|error| error.to_string())
        };
        set("type", TjsValue::Integer(i64::from(node.node_type)))?;
        set("label", TjsValue::String(node.label.clone()))?;
        set("visible", TjsValue::Integer(i64::from(node.draw_flag)))?;
        set(
            "branchVisible",
            TjsValue::Integer(i64::from(node.draw_flag)),
        )?;
        set(
            "layerVisible",
            TjsValue::Integer(i64::from(node.local.active)),
        )?;
        set("x", TjsValue::Real(node.accumulated.x))?;
        set("y", TjsValue::Real(node.accumulated.y))?;
        set("left", TjsValue::Real(node.accumulated.x))?;
        set("top", TjsValue::Real(node.accumulated.y))?;
        set("flipX", TjsValue::Integer(i64::from(self.flip)))?;
        set("flipY", TjsValue::Integer(0))?;
        set("zoomX", TjsValue::Real(node.accumulated.scale_x))?;
        set("zoomY", TjsValue::Real(node.accumulated.scale_y))?;
        set(
            "angleDeg",
            TjsValue::Real(node.accumulated.angle.to_degrees()),
        )?;
        set("angleRad", TjsValue::Real(node.accumulated.angle))?;
        set("slantX", TjsValue::Real(node.accumulated.slant_x))?;
        set("slantY", TjsValue::Real(node.accumulated.slant_y))?;
        set("originX", TjsValue::Real(0.0))?;
        set("originY", TjsValue::Real(0.0))?;
        set("opacity", TjsValue::Real(node.accumulated.opacity))?;
        set("mtx", TjsValue::Integer(0))?;
        set("vtx", TjsValue::Integer(0))?;
        set("color", TjsValue::Integer(i64::from(self.color.packed)))?;
        set("bezierPatch", TjsValue::Integer(0))?;
        set("shape", TjsValue::Integer(i64::from(node.shape)))?;
        set("motion", TjsValue::Integer(0))?;
        set("particle", TjsValue::Integer(node.particle_count as i64))?;
        Ok(object)
    }

    pub(crate) fn hit_test(&self, label: &str, x: f64, y: f64) -> bool {
        for node in &self.nodes {
            if !label.is_empty() && node.label != label {
                continue;
            }
            if !node.draw_flag {
                continue;
            }
            let half_w = 32.0 * node.accumulated.scale_x.abs().max(0.0001);
            let half_h = 32.0 * node.accumulated.scale_y.abs().max(0.0001);
            let dx = x - node.accumulated.x;
            let dy = y - node.accumulated.y;
            if dx >= -half_w && dx <= half_w && dy >= -half_h && dy <= half_h {
                return true;
            }
        }
        false
    }

    pub(crate) fn serialize(&mut self, ctx: &CompatCtx) -> Result<RetainedValue, String> {
        self.ensure_motion_loaded(ctx)?;
        let object = eval_object(ctx, "%[]")?;
        // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `serialize`.
        let engine = unsafe { ctx.engine() };
        let set = |key: &str, value: TjsValue| -> Result<(), String> {
            engine
                .set_member(object.raw_id(), key, &value)
                .map_err(|error| error.to_string())
        };
        set("chara", TjsValue::String(self.chara.clone()))?;
        set("motionKey", TjsValue::String(self.motion_key.clone()))?;
        set("x", TjsValue::Real(self.coord.x.value() as f64))?;
        set("y", TjsValue::Real(self.coord.y.value() as f64))?;
        set("scale", TjsValue::Real(self.scale.value() as f64))?;
        set("rotate", TjsValue::Real(self.rotate.value() as f64))?;
        set("color", TjsValue::Integer(i64::from(self.color.packed)))?;
        set("mirror", TjsValue::Integer(i64::from(self.mirror_base)))?;
        set("tickCount", TjsValue::Real(self.frame_tick_count))?;
        set("allplaying", TjsValue::Integer(i64::from(self.allplaying)))?;
        let variables = eval_object(ctx, "%[]")?;
        for (label, value) in &self.variable_values {
            engine
                .set_member(variables.raw_id(), label, &TjsValue::Real(*value))
                .map_err(|error| error.to_string())?;
        }
        install_object(ctx, object.raw_id(), "variables", variables)?;
        let timelines = eval_object(ctx, "%[]")?;
        for (label, state) in &self.timelines {
            engine
                .set_member(
                    timelines.raw_id(),
                    label,
                    &TjsValue::Real(state.current_time),
                )
                .map_err(|error| error.to_string())?;
        }
        install_object(ctx, object.raw_id(), "timelines", timelines)?;
        Ok(RetainedValue::Object(object))
    }

    pub(crate) fn unserialize(
        &mut self,
        ctx: &CompatCtx,
        value: &StoredValue,
    ) -> Result<(), String> {
        let StoredValue::Object(object) = value else {
            return Ok(());
        };
        // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `unserialize`.
        let engine = unsafe { ctx.engine() };
        if let Ok(TjsValue::String(chara)) = engine.member(object.raw_id(), "chara") {
            self.chara = chara;
        }
        if let Ok(TjsValue::String(key)) = engine.member(object.raw_id(), "motionKey") {
            self.motion_key = key;
        }
        if let Ok(TjsValue::Real(x)) = engine.member(object.raw_id(), "x") {
            self.coord.x.set(x as f32);
        }
        if let Ok(TjsValue::Real(y)) = engine.member(object.raw_id(), "y") {
            self.coord.y.set(y as f32);
        }
        if let Ok(TjsValue::Real(scale)) = engine.member(object.raw_id(), "scale") {
            self.scale.set(scale as f32);
        }
        if let Ok(TjsValue::Real(rotate)) = engine.member(object.raw_id(), "rotate") {
            self.rotate.set(rotate as f32);
        }
        if let Ok(TjsValue::Integer(color)) = engine.member(object.raw_id(), "color") {
            self.color = ColorAnimator::from_packed(color as u32);
        }
        self.ensure_motion_loaded(ctx)?;
        Ok(())
    }

    pub(crate) fn build_info_list(&self, ctx: &CompatCtx) -> Result<RetainedValue, String> {
        let array = eval_object(ctx, "[]")?;
        // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `build_info_list`.
        let engine = unsafe { ctx.engine() };
        for (index, label) in self.playing_timelines.iter().enumerate() {
            let entry = eval_object(ctx, "%[]")?;
            engine
                .set_member(entry.raw_id(), "label", &TjsValue::String(label.clone()))
                .map_err(|error| error.to_string())?;
            let flags = self
                .timelines
                .get(label)
                .map(|state| state.flags)
                .unwrap_or(1);
            engine
                .set_member(
                    entry.raw_id(),
                    "flags",
                    &TjsValue::Integer(1 | i64::from(flags & 2)),
                )
                .map_err(|error| error.to_string())?;
            let ratio = self
                .timelines
                .get(label)
                .map(|state| state.blend_ratio)
                .unwrap_or(1.0);
            engine
                .set_member(entry.raw_id(), "blendRatio", &TjsValue::Real(ratio))
                .map_err(|error| error.to_string())?;
            let id = entry.raw_id().as_ptr() as usize as u64;
            engine
                .set_member(array.raw_id(), &index.to_string(), &TjsValue::Retained(id))
                .map_err(|error| error.to_string())?;
            drop(entry);
        }
        Ok(RetainedValue::Object(array))
    }

    /// Draw the current pose onto the target layer through the L2 scene.
    pub(crate) fn draw(&mut self, ctx: &CompatCtx, target: &StoredValue) -> Result<(), String> {
        self.update_layers();
        let Some(target_id) = target.object_id() else {
            return Ok(());
        };
        // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `draw`.
        let engine = unsafe { ctx.engine() };
        // A draw target is a `SeparateLayerAdaptor`, which draws into the layer
        // its live `targetLayer` names, or a `Layer` itself. A target that
        // names no layer leaves nothing to draw into and is a no-op. The D3D
        // adaptor route is not wired.
        let layer_id = match resolve_draw_target(engine, target_id)? {
            Some(id) => id,
            None => return Ok(()),
        };
        // SAFETY: the host keeps the attached scene alive for the binding.
        let Some(scene) = unsafe { ctx.services() }.scene() else {
            return Ok(());
        };
        // SAFETY: the host keeps the attached scene alive for the binding.
        let scene = unsafe { &mut *scene };
        let root = self.nodes.first().cloned();
        if let Some(layer) = scene.layer_mut(layer_id)
            && let Some(root) = &root
        {
            layer.rect.x = root.accumulated.x.round() as i32;
            layer.rect.y = root.accumulated.y.round() as i32;
            layer.opacity = (root.accumulated.opacity.clamp(0.0, 1.0)) as f32;
            layer.visible = root.draw_flag && self.visible;
        }
        let color = color_bytes(self.color.bytes);
        let nodes: Vec<MotionNode> = self
            .nodes
            .iter()
            .filter(|node| {
                node.draw_flag
                    && (node.shape > 0 || (node.node_type == 0 && !node.local.src.is_empty()))
            })
            .cloned()
            .collect();
        let (width, height) = scene
            .layer(layer_id)
            .map(|layer| (layer.rect.w as i32, layer.rect.h as i32))
            .unwrap_or((0, 0));
        if width <= 0 || height <= 0 {
            return Ok(());
        }
        let snapshot = self.snapshot.clone();
        // SAFETY: the host keeps the attached storage alive for the binding.
        let storage = unsafe { ctx.services() }.storage();
        // SAFETY: the host keeps the attached storage alive for the binding.
        let storage = storage.map(|storage| unsafe { &*storage });
        let textures = &mut self.textures;
        let rendered = scene.with_main_image_pixels_mut(layer_id, |pixels| {
            let mut bitmap = BitmapState::new(width as u32, height as u32, pixels.to_vec());
            let mut missing = Vec::new();
            for node in &nodes {
                draw_node(
                    &mut bitmap,
                    node,
                    snapshot.as_deref(),
                    storage,
                    color,
                    textures,
                    &mut missing,
                );
            }
            (bitmap.rgba, missing)
        });
        let Some((rgba, missing)) = rendered else {
            return Ok(());
        };
        for src in missing {
            if self.missing_textures.insert(src.clone()) {
                log::warn!("Motion.Player.draw: no texture for {src}");
            }
        }
        if let Some(bitmap_id) = scene.layer(layer_id).and_then(|layer| layer.bitmap)
            && let Some(bitmap) = scene.bitmap_mut(bitmap_id)
        {
            bitmap.rgba = rgba;
            bitmap.mark_dirty();
        }
        Ok(())
    }
}

/// Draw one node: the icon texture it names, mapped onto the quad its
/// accumulated transform describes.
///
/// A shape node draws the placeholder quad instead, and a node whose icon has
/// no readable texture draws nothing and is reported through `missing`.
fn draw_node(
    bitmap: &mut BitmapState,
    node: &MotionNode,
    snapshot: Option<&MotionSnapshot>,
    storage: Option<&Storage>,
    color: [u8; 4],
    textures: &mut HashMap<String, Arc<Pixels>>,
    missing: &mut Vec<String>,
) {
    let icon = snapshot.and_then(|snapshot| snapshot.icons.get(&node.local.src));
    let (Some(snapshot), Some(icon), Some(storage)) = (snapshot, icon, storage) else {
        if node.shape > 0 {
            fill_placeholder_quad(bitmap, node, color);
        } else if node.local.src.starts_with("src/") {
            // Only a `src/<group>/<name>` source names an icon; the layout and
            // blank sources of container and mask nodes draw nothing.
            missing.push(node.local.src.clone());
        }
        return;
    };
    let Some((pixels, crop)) = icon_pixels(snapshot, storage, icon, textures) else {
        missing.push(node.local.src.clone());
        return;
    };
    let width = (crop.2 - crop.0) as u32;
    let height = (crop.3 - crop.1) as u32;
    if width == 0 || height == 0 {
        return;
    }
    // The quad's source is the cropped icon tinted by the player color, so
    // the corners map that rectangle 1:1 onto the layer.
    let mut source = BitmapState::new(
        width,
        height,
        vec![0u8; width as usize * height as usize * 4],
    );
    for y in 0..height as usize {
        for x in 0..width as usize {
            let from = ((crop.1 as usize + y) * pixels.width as usize + crop.0 as usize + x) * 4;
            let to = (y * width as usize + x) * 4;
            for (channel, value) in source.rgba[to..to + 4].iter_mut().enumerate() {
                *value = (u32::from(pixels.rgba[from + channel]) * u32::from(color[channel]) / 255)
                    as u8;
            }
        }
    }
    let matrix = local_matrix(&node.accumulated, &node.transform_order);
    let (center_x, center_y) = (node.accumulated.x, node.accumulated.y);
    let corner = |local_x: f64, local_y: f64| {
        (
            center_x + matrix[0] * local_x + matrix[1] * local_y,
            center_y + matrix[2] * local_x + matrix[3] * local_y,
        )
    };
    let (origin_x, origin_y) = (icon.origin_x, icon.origin_y);
    affine_blit(
        bitmap,
        AffineBlit {
            corners: [
                corner(-origin_x, -origin_y),
                corner(f64::from(width) - origin_x, -origin_y),
                corner(-origin_x, f64::from(height) - origin_y),
            ],
            stretch_type: 2,
            clear: false,
            clear_color: [0, 0, 0, 0],
        },
        BlitSource {
            bitmap: &source,
            rect: (0, 0, width as i32, height as i32),
        },
        BlitOptions {
            mode: 2,
            opa: (node.accumulated.opacity.clamp(0.0, 1.0) * 255.0).round() as u8,
            treat_as_opaque: false,
            hold_alpha: false,
        },
    );
}

/// Draw the fixed-size solid quad a shape node approximates to.
fn fill_placeholder_quad(bitmap: &mut BitmapState, node: &MotionNode, color: [u8; 4]) {
    let (cx, cy) = (node.accumulated.x, node.accumulated.y);
    let (sx, sy) = (
        node.accumulated.scale_x.abs().max(0.01),
        node.accumulated.scale_y.abs().max(0.01),
    );
    let points = [
        (cx - 32.0 * sx, cy - 32.0 * sy),
        (cx + 32.0 * sx, cy - 32.0 * sy),
        (cx + 32.0 * sx, cy + 32.0 * sy),
        (cx - 32.0 * sx, cy + 32.0 * sy),
    ];
    let alpha = (f64::from(color[3]) * node.accumulated.opacity.clamp(0.0, 1.0)) as u8;
    crate::engine_api::raster::fill_polygon(bitmap, &points, [color[0], color[1], color[2], alpha]);
}

/// Read and decode the pixels `icon` draws from, and the sub-rectangle of them
/// it occupies.
///
/// The icon is cropped from its group's `texture` atlas when the atlas and the
/// crop are usable, and from its own `pixel` chunk otherwise.
fn icon_pixels(
    snapshot: &MotionSnapshot,
    storage: &Storage,
    icon: &IconInfo,
    textures: &mut HashMap<String, Arc<Pixels>>,
) -> Option<(Arc<Pixels>, RectI)> {
    let width = pixel_size(icon.width)?;
    let height = pixel_size(icon.height)?;
    if let Some(texture) = snapshot.textures.get(&icon.group) {
        let atlas_path = format!(
            "psb://{}/source/{}/texture/pixel",
            snapshot.container, icon.group
        );
        let palette_path = format!(
            "psb://{}/source/{}/texture/pal",
            snapshot.container, icon.group
        );
        if let (Some(atlas_width), Some(atlas_height)) =
            (pixel_size(texture.width), pixel_size(texture.height))
            && let Some(atlas) = cached_pixels(
                storage,
                &atlas_path,
                &palette_path,
                atlas_width,
                atlas_height,
                texture.compressed,
                textures,
            )
            && let (Some(left), Some(top)) = (pixel_offset(icon.left), pixel_offset(icon.top))
            && left + width <= atlas.width
            && top + height <= atlas.height
        {
            let crop = (
                left as i32,
                top as i32,
                (left + width) as i32,
                (top + height) as i32,
            );
            return Some((atlas, crop));
        }
    }
    let icon_path = format!(
        "psb://{}/source/{}/icon/{}/pixel",
        snapshot.container, icon.group, icon.name
    );
    let palette_path = format!(
        "psb://{}/source/{}/icon/{}/pal",
        snapshot.container, icon.group, icon.name
    );
    let pixels = cached_pixels(
        storage,
        &icon_path,
        &palette_path,
        width,
        height,
        icon.compressed,
        textures,
    )?;
    Some((pixels, (0, 0, width as i32, height as i32)))
}

/// The decoded pixels at `path`, from the cache or freshly decoded.
fn cached_pixels(
    storage: &Storage,
    path: &str,
    palette_path: &str,
    width: u32,
    height: u32,
    compressed: bool,
    textures: &mut HashMap<String, Arc<Pixels>>,
) -> Option<Arc<Pixels>> {
    if let Some(pixels) = textures.get(path) {
        return Some(pixels.clone());
    }
    let bytes = match storage.read(strip_file_media(path)) {
        Ok(bytes) => bytes,
        Err(StorageReadError::NotFound(_)) => return None,
        Err(error) => {
            log::warn!("Motion.Player.draw: cannot read {path}: {error}");
            return None;
        }
    };
    let palette = match storage.read(strip_file_media(palette_path)) {
        Ok(palette) => Some(palette),
        Err(StorageReadError::NotFound(_)) => None,
        Err(error) => {
            log::warn!("Motion.Player.draw: cannot read {palette_path}: {error}");
            None
        }
    };
    match decode_pixels(&bytes, palette.as_deref(), width, height, compressed) {
        Ok(decoded) => {
            let pixels = Arc::new(decoded);
            textures.insert(path.to_string(), pixels.clone());
            Some(pixels)
        }
        Err(error) => {
            log::warn!("Motion.Player.draw: cannot decode {path}: {error}");
            None
        }
    }
}

/// An image dimension rounded to whole pixels, or `None` when it carries no
/// usable size.
fn pixel_size(value: f64) -> Option<u32> {
    let rounded = value.round();
    if !rounded.is_finite() || rounded < 1.0 || rounded > f64::from(u32::MAX) {
        return None;
    }
    Some(rounded as u32)
}

/// A whole-pixel offset into an atlas, or `None` when it is negative.
fn pixel_offset(value: f64) -> Option<u32> {
    let rounded = value.round();
    if !rounded.is_finite() || rounded < 0.0 || rounded > f64::from(u32::MAX) {
        return None;
    }
    Some(rounded as u32)
}

/// The node's local 2x2 transform as `[m11, m12, m21, m22]`, composed from the
/// flip, angle, zoom and slant steps in the layer's `transformOrder`.
///
/// The stored angle is in degrees. A step id outside `0..=3` is skipped, and a
/// flip is already folded into the sign of the node's scale.
fn local_matrix(state: &FrameState, order: &[i32; 4]) -> [f64; 4] {
    let (mut m11, mut m12, mut m21, mut m22) = (1.0f64, 0.0f64, 0.0f64, 1.0f64);
    for &step in order {
        match step {
            0 => {}
            1 => {
                if state.angle != 0.0 {
                    let (sin, cos) = state.angle.to_radians().sin_cos();
                    let (t11, t12) = (cos * m11 - sin * m21, cos * m12 - sin * m22);
                    let (t21, t22) = (sin * m11 + cos * m21, sin * m12 + cos * m22);
                    (m11, m12, m21, m22) = (t11, t12, t21, t22);
                }
            }
            2 => {
                m11 *= state.scale_x;
                m12 *= state.scale_x;
                m21 *= state.scale_y;
                m22 *= state.scale_y;
            }
            3 if state.slant_x != 0.0 || state.slant_y != 0.0 => {
                let (t11, t12) = (m11 + state.slant_x * m21, m22 * state.slant_x + m12);
                let (t21, t22) = (m11 * state.slant_y + m21, m22 + m12 * state.slant_y);
                (m11, m12, m21, m22) = (t11, t12, t21, t22);
            }
            3 => {}
            _ => {}
        }
    }
    [m11, m12, m21, m22]
}

/// The byte channels a normalized color animator hands the rasterizer.
fn color_bytes(channels: [f32; 4]) -> [u8; 4] {
    channels.map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// The layer a `Player.draw` target draws into.
///
/// A `SeparateLayerAdaptor` draws into the layer its live `targetLayer` names;
/// any other object is resolved as a `Layer` directly. `None` means the target
/// names no layer, which `draw` treats as a no-op.
fn resolve_draw_target(engine: &tjs::Tjs2Engine, target_id: usize) -> Result<Option<u32>, String> {
    let payload = match engine.native_instance_payload(
        tjs::Tjs2ValueId::from_raw(target_id as *mut std::ffi::c_void),
        "SeparateLayerAdaptor",
    ) {
        Ok(Some(payload)) => payload,
        Ok(None) => {
            return crate::CompatCtx::native_instance_id(
                engine,
                tjs::Tjs2ValueId::from_raw(target_id as *mut std::ffi::c_void),
                &["Layer"],
            );
        }
        Err(error) => return Err(error),
    };
    // SAFETY: `target_id` is a live object retained for the draw call, so the
    // VM keeps the `SeparateLayerAdaptor` payload alive for this call.
    let Some(adaptor) = (unsafe { inst_ref::<SeparateInst>(payload.as_ptr()) }) else {
        return Ok(None);
    };
    let Some(target) = adaptor.target_object_id() else {
        return Ok(None);
    };
    crate::CompatCtx::native_instance_id(
        engine,
        tjs::Tjs2ValueId::from_raw(target as *mut std::ffi::c_void),
        &["Layer"],
    )
}

/// Install an object member by retaining it.
fn install_object(
    ctx: &CompatCtx,
    parent: tjs::Tjs2ValueId,
    key: &str,
    object: DetachedValue,
) -> Result<(), String> {
    // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `install_object`.
    let engine = unsafe { ctx.engine() };
    let id = object.raw_id().as_ptr() as usize as u64;
    engine
        .set_member(parent, key, &TjsValue::Retained(id))
        .map_err(|error| error.to_string())?;
    drop(object);
    Ok(())
}

fn default_variable(snapshot: &MotionSnapshot, label: &str) -> f64 {
    if let Some(first) = snapshot
        .variable_frames
        .get(label)
        .and_then(|frames| frames.first())
    {
        return first.value;
    }
    if let Some(range) = snapshot.variable_ranges.get(label) {
        return range.0;
    }
    0.0
}

/// The engine-store id a module object carries in `__motionSnapshot`.
fn module_snapshot_id(ctx: &CompatCtx, module: &StoredValue) -> Option<u64> {
    let id = module.object_id()?;
    // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `module_snapshot_id`.
    match unsafe { ctx.engine() }.member(
        tjs::Tjs2ValueId::from_raw(id as *mut std::ffi::c_void),
        "__motionSnapshot",
    ) {
        Ok(TjsValue::Integer(value)) => u64::try_from(value).ok(),
        _ => None,
    }
}

/// Whether a motion name carries a path separator or an extension and
/// therefore resolves to itself.
fn looks_like_storage(name: &str) -> bool {
    name.contains('/') || name.contains('\\') || name.contains('.')
}

/// Candidate paths a bare motion name resolves through.
fn motion_lookup_candidates(name: &str) -> Vec<String> {
    if looks_like_storage(name) {
        return vec![name.to_string()];
    }
    vec![
        format!("{name}.mtn"),
        format!("{name}.psb"),
        format!("motion/{name}.mtn"),
        format!("motion/{name}.psb"),
    ]
}

/// The first candidate mounted in storage.
fn placed_motion_path(ctx: &CompatCtx, candidates: &[String]) -> Result<Option<String>, String> {
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let Some(storage) = services.storage() else {
        return Ok(None);
    };
    // SAFETY: the host keeps the attached storage alive for the binding.
    let storage = unsafe { &*storage };
    for candidate in candidates {
        match storage.find(strip_file_media(candidate)) {
            Ok(Some(_)) => return Ok(Some(candidate.clone())),
            Ok(None) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(None)
}

/// Load `path` through a bound resource manager so its cache and last-module
/// slot observe the load.
fn load_manager_module(
    ctx: &CompatCtx,
    manager: usize,
    path: &str,
) -> Result<Option<(u64, Arc<MotionSnapshot>)>, String> {
    // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `load_manager_module`.
    let engine = unsafe { ctx.engine() };
    let payload = engine.native_instance_payload(
        tjs::Tjs2ValueId::from_raw(manager as *mut std::ffi::c_void),
        "ResourceManager",
    )?;
    let Some(payload) = payload else {
        return Ok(None);
    };
    // SAFETY: the manager object is alive for this call, so its native payload is too.
    let Some(instance) = (unsafe { inst_mut::<ResourceInst>(payload.as_ptr()) }) else {
        return Ok(None);
    };
    instance.load_module(ctx, path)
}

/// Drop a bound manager's cache and its engine snapshot registry entries.
fn clear_manager_cache(ctx: &CompatCtx, manager: usize) {
    // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `clear_manager_cache`.
    let engine = unsafe { ctx.engine() };
    let payload = match engine.native_instance_payload(
        tjs::Tjs2ValueId::from_raw(manager as *mut std::ffi::c_void),
        "ResourceManager",
    ) {
        Ok(payload) => payload,
        Err(error) => {
            log::warn!("motion_player: resource manager lookup failed: {error}");
            return;
        }
    };
    let Some(payload) = payload else {
        return;
    };
    // SAFETY: the manager object is alive for this call, so its native payload is too.
    let Some(instance) = (unsafe { inst_mut::<ResourceInst>(payload.as_ptr()) }) else {
        return;
    };
    instance.clear_cache(ctx);
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

extern "C" fn player_create(_engine: *mut std::ffi::c_void) -> *mut std::ffi::c_void {
    Box::into_raw(Box::new(PlayerInst::default())) as *mut std::ffi::c_void
}

extern "C" fn player_destroy(_engine: *mut std::ffi::c_void, instance: *mut std::ffi::c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` came from `player_create` and is released once.
    drop(unsafe { Box::from_raw(instance as *mut PlayerInst) });
}

extern "C" fn player_ctor(
    engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    argc: std::os::raw::c_int,
    argv: *const Value,
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
    // SAFETY: the trampoline guarantees `argc` valid entries.
    let arguments = unsafe { args(argv, argc) };
    let ctx = res.ctx();
    let passed = arguments.first().filter(|value| value.ty == VAL_OBJECT);
    let mut manager = None;
    if let Some(value) = passed {
        // SAFETY: the argument is a live object for the duration of the call.
        match unsafe { ctx.engine() }.retain_object_arg(value) {
            Ok(retained) => manager = Some(StoredValue::Object(retained)),
            Err(error) => return report_error(out_error, &format!("Player: {error}")),
        }
    }
    inst.resource_manager = match manager {
        Some(manager) => manager,
        None => match eval_object(&ctx, "new Motion.ResourceManager(null, 1024)") {
            Ok(manager) => StoredValue::Object(manager),
            Err(error) => {
                return report_error(
                    out_error,
                    &format!("Player: cannot create the default resource manager: {error}"),
                );
            }
        },
    };
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// Raw callbacks (need objthis)
// ---------------------------------------------------------------------------

extern "C" fn player_set_variable_raw(
    engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    argc: std::os::raw::c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut std::ffi::c_void,
) -> std::os::raw::c_int {
    let Some(_res) = resolve(engine) else {
        return report_error(out_error, super::helpers::NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<PlayerInst>(instance) }) else {
        return report_error(out_error, "Player: null instance");
    };
    // SAFETY: the trampoline guarantees `argc` valid entries.
    let arguments = unsafe { args(argv, argc) };
    if arguments.len() < 2 {
        return report_error(out_error, "setVariable requires (label, value)");
    }
    let label = value_as_string(&arguments[0]);
    let value = value_as_f64(&arguments[1]);
    inst.variable_values.insert(label, value);
    set_void(out);
    0
}

extern "C" fn player_play_raw(
    engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    argc: std::os::raw::c_int,
    argv: *const Value,
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
    // SAFETY: the trampoline guarantees `argc` valid entries.
    let arguments = unsafe { args(argv, argc) };
    let label = arguments.first().map(value_as_string).unwrap_or_default();
    let flags = arguments.get(1).map(value_as_i64).unwrap_or(0) as i32;
    match inst.play(&label, flags, &res.ctx()) {
        Ok(started) => {
            set_bool(out, started);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

extern "C" fn player_progress_raw(
    engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    argc: std::os::raw::c_int,
    argv: *const Value,
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
    // SAFETY: the trampoline guarantees `argc` valid entries.
    let arguments = unsafe { args(argv, argc) };
    let dt = arguments
        .first()
        .map(|value| match value.ty {
            tjs::VAL_REAL => value.real,
            tjs::VAL_INTEGER => value.integer as f64,
            _ => value_as_i64(value) as f64,
        })
        .unwrap_or(0.0);
    inst.progress(dt, &res.ctx());
    set_void(out);
    0
}

extern "C" fn player_is_playing_raw(
    engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    _argc: std::os::raw::c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut std::ffi::c_void,
) -> std::os::raw::c_int {
    let Some(_res) = resolve(engine) else {
        return report_error(out_error, super::helpers::NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { super::helpers::inst_ref::<PlayerInst>(instance) }) else {
        return report_error(out_error, "Player: null instance");
    };
    set_bool(out, inst.playing());
    0
}

extern "C" fn player_stop_raw(
    engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    _argc: std::os::raw::c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut std::ffi::c_void,
) -> std::os::raw::c_int {
    let Some(_res) = resolve(engine) else {
        return report_error(out_error, super::helpers::NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<PlayerInst>(instance) }) else {
        return report_error(out_error, "Player: null instance");
    };
    inst.allplaying = false;
    set_bool(out, true);
    0
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the `Player` instance class.
pub(crate) fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    register_named(ctx, "Player", false)
}

/// Register a class with the `Player` surface. `emote` additionally installs
/// the `EmotePlayer` shell members (the reference `D3DEmotePlayer` inherits
/// them).
pub(crate) fn register_named(
    ctx: &mut CompatCtx,
    class_name: &'static str,
    emote: bool,
) -> Result<(), CompatError> {
    use tjs::{NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef};

    let mut methods = Vec::new();
    let mut properties = Vec::new();

    macro_rules! method {
        ($name:literal, $f:ident) => {
            methods.push(NativeInstanceMethodDef { name: $name, f: $f })
        };
    }
    macro_rules! prop {
        ($name:literal, $get:expr, $set:expr) => {
            properties.push(NativeInstancePropertyDef {
                name: $name,
                get: $get,
                set: $set,
            })
        };
    }

    method!("Player", player_ctor);
    // `EmotePlayer` and `D3DEmotePlayer` declare their own `NCB_CONSTRUCTOR`
    // (`reference/cpp/plugins/motionplayer/main.cpp:299,588`), which ncbind
    // registers under the class name (`ncbind.hpp:1745`). A derived class
    // keeps the inherited `Player` member and gains its own class-name one,
    // so register both.
    if class_name != "Player" {
        methods.push(NativeInstanceMethodDef {
            name: class_name,
            f: player_ctor,
        });
    }
    method!("random", player_random);
    method!("initPhysics", player_init_physics);
    method!("serialize", player_serialize);
    method!("unserialize", player_unserialize);
    method!("setRotate", player_set_rotate);
    method!("setMirror", player_set_mirror);
    method!("setHairScale", player_set_hair_scale);
    method!("setPartsScale", player_set_parts_scale);
    method!("setBustScale", player_set_bust_scale);
    method!("setDrawAffineTranslateMatrix", player_set_affine);
    method!("getCameraOffset", player_get_camera_offset);
    method!("setCameraOffset", player_set_camera_offset);
    method!("modifyRoot", player_modify_root);
    method!("debugPrint", player_debug_print);
    method!("unload", player_unload);
    method!("unloadAll", player_unload_all);
    method!("isExistMotion", player_is_exist_motion);
    method!("findMotion", player_find_motion);
    method!("requireLayerId", player_require_layer_id);
    method!("releaseLayerId", player_release_layer_id);
    method!("setClearColor", player_set_clear_color);
    method!("setResizable", player_set_resizable);
    method!("removeAllTextures", player_remove_all_textures);
    method!("removeAllBg", player_remove_all_bg);
    method!("removeAllCaption", player_remove_all_caption);
    method!("registerBg", player_register_bg);
    method!("registerCaption", player_register_caption);
    method!("unloadUnusedTextures", player_unload_unused_textures);
    method!("alphaOpAdd", player_alpha_op_add);
    method!("captureCanvas", player_capture_canvas);
    method!("findSource", player_find_source);
    method!("loadSource", player_load_source);
    method!("clearCache", player_clear_cache);
    method!("setSize", player_set_size);
    method!("copyRect", player_copy_rect);
    method!("adjustGamma", player_adjust_gamma);
    method!("draw", player_draw);
    method!("frameProgress", player_frame_progress);
    method!("setFlip", player_set_flip);
    method!("setOpacity", player_set_opacity);
    method!("setVisible", player_set_visible);
    method!("setSlant", player_set_slant);
    method!("setZoom", player_set_zoom);
    method!("getLayerNames", player_get_layer_names);
    method!("releaseSyncWait", player_release_sync_wait);
    method!("calcViewParam", player_calc_view_param);
    method!("getLayerMotion", player_get_layer_motion);
    method!("getLayerGetter", player_get_layer_getter);
    method!("getLayerGetterList", player_get_layer_getter_list);
    method!("skipToSync", player_skip_to_sync);
    method!("setStereovisionCameraPosition", player_set_stereo_camera);
    method!("getVariable", player_get_variable);
    method!("countVariables", player_count_variables);
    method!("getVariableLabelAt", player_variable_label_at);
    method!("countVariableFrameAt", player_count_variable_frame);
    method!("getVariableFrameLabelAt", player_variable_frame_label_at);
    method!("getVariableFrameValueAt", player_variable_frame_value_at);
    method!("getTimelinePlaying", player_timeline_playing);
    method!("getVariableRange", player_variable_range);
    method!("getVariableFrameList", player_variable_frame_list);
    method!("countMainTimelines", player_count_main_timelines);
    method!("getMainTimelineLabelAt", player_main_timeline_at);
    method!("getMainTimelineLabelList", player_main_timeline_list);
    method!("countDiffTimelines", player_count_diff_timelines);
    method!("getDiffTimelineLabelAt", player_diff_timeline_at);
    method!("getDiffTimelineLabelList", player_diff_timeline_list);
    method!("getLoopTimeline", player_loop_timeline);
    method!("countPlayingTimelines", player_count_playing);
    method!("getPlayingTimelineLabelAt", player_playing_label_at);
    method!("getPlayingTimelineFlagsAt", player_playing_flags_at);
    method!("getTimelineTotalFrameCount", player_timeline_total_frames);
    method!("playTimeline", player_play_timeline);
    method!("stopTimeline", player_stop_timeline);
    method!("setTimelineBlendRatio", player_set_timeline_blend);
    method!("getTimelineBlendRatio", player_get_timeline_blend);
    method!("fadeInTimeline", player_fade_in_timeline);
    method!("fadeOutTimeline", player_fade_out_timeline);
    method!("getPlayingTimelineInfoList", player_timeline_info_list);
    method!("isSelectorTarget", player_is_selector_target);
    method!(
        "deactivateSelectorTarget",
        player_deactivate_selector_target
    );
    method!("getCommandList", player_command_list);
    method!("getD3DAvailable", player_get_d3d);
    method!("doAlphaMaskOperation", player_do_alpha_mask);
    method!("onFindMotion", player_on_find_motion);
    method!("motionList", player_motion_list);
    method!("emoteEdit", player_emote_edit);
    method!("setVariable", player_set_variable_raw);
    method!("play", player_play_raw);
    method!("progress", player_progress_raw);
    method!("isPlaying", player_is_playing_raw);
    method!("stop", player_stop_raw);
    if emote {
        method!("create", player_emote_create);
        method!("load", player_emote_load);
        method!("clone", player_emote_clone);
        method!("show", player_emote_show);
        method!("hide", player_emote_hide);
        method!("assignState", player_emote_assign_state);
        method!("setRot", player_set_rotate);
        method!("getRot", player_emote_get_rot);
        method!("setCoord", player_emote_set_coord);
        method!("setScale", player_emote_set_scale);
        method!("getScale", player_emote_get_scale);
        method!("setColor", player_emote_set_color);
        method!("getColor", player_emote_get_color);
        method!("startWind", player_emote_start_wind);
        method!("stopWind", player_emote_stop_wind);
        method!("setOuterForce", player_emote_set_outer_force);
        method!("getOuterForce", player_emote_get_outer_force);
        method!("skip", player_emote_skip);
        method!("pass", player_emote_pass);
        method!("addPlayCallback", player_emote_add_play_callback);
        method!("setTimeline", player_emote_set_timeline);
        method!("isTimelinePlaying", player_is_playing_raw);
        method!("isLoopTimeline", player_loop_timeline);
        method!("contains", player_emote_contains);
    }

    prop!("x", Some(player_get_x), Some(player_set_x));
    prop!("y", Some(player_get_y), Some(player_set_y));
    prop!("left", Some(player_get_x), Some(player_set_x));
    prop!("top", Some(player_get_y), Some(player_set_y));
    prop!(
        "completionType",
        Some(player_get_completion),
        Some(player_set_completion)
    );
    prop!(
        "metadata",
        Some(player_get_metadata),
        Some(player_set_metadata)
    );
    prop!("chara", Some(player_get_chara), Some(player_set_chara));
    prop!("motion", Some(player_get_motion), Some(player_set_motion));
    prop!(
        "motionKey",
        Some(player_get_motion_key),
        Some(player_set_motion_key)
    );
    prop!(
        "outline",
        Some(player_get_outline),
        Some(player_set_outline)
    );
    prop!(
        "priorDraw",
        Some(player_get_prior_draw),
        Some(player_set_prior_draw)
    );
    prop!(
        "frameLastTime",
        Some(player_get_frame_last),
        Some(player_set_frame_last)
    );
    prop!(
        "frameLoopTime",
        Some(player_get_frame_loop),
        Some(player_set_frame_loop)
    );
    prop!(
        "loopTime",
        Some(player_get_loop_time),
        Some(player_set_loop_time)
    );
    prop!(
        "processedMeshVerticesNum",
        Some(player_get_processed),
        Some(player_set_processed)
    );
    prop!("playing", Some(player_get_playing), None);
    prop!(
        "queuing",
        Some(player_get_queuing),
        Some(player_set_queuing)
    );
    prop!(
        "directEdit",
        Some(player_get_direct_edit),
        Some(player_set_direct_edit)
    );
    prop!(
        "selectorEnabled",
        Some(player_get_selector_enabled),
        Some(player_set_selector_enabled)
    );
    prop!(
        "variableKeys",
        Some(player_get_variable_keys),
        Some(player_set_variable_keys)
    );
    prop!("allplaying", Some(player_get_allplaying), None);
    prop!(
        "syncWaiting",
        Some(player_get_sync_waiting),
        Some(player_set_sync_waiting)
    );
    prop!(
        "syncActive",
        Some(player_get_sync_active),
        Some(player_set_sync_active)
    );
    prop!(
        "hasCamera",
        Some(player_get_has_camera),
        Some(player_set_has_camera)
    );
    prop!(
        "cameraActive",
        Some(player_get_camera_active),
        Some(player_set_camera_active)
    );
    prop!(
        "stereovisionActive",
        Some(player_get_stereo),
        Some(player_set_stereo)
    );
    prop!("tickCount", Some(player_get_tick), Some(player_set_tick));
    prop!("speed", Some(player_get_speed), Some(player_set_speed));
    prop!(
        "frameTickCount",
        Some(player_get_frame_tick),
        Some(player_set_frame_tick)
    );
    prop!("maskMode", Some(player_get_mask), Some(player_set_mask));
    prop!(
        "colorWeight",
        Some(player_get_color_weight),
        Some(player_set_color_weight)
    );
    prop!(
        "independentLayerInherit",
        Some(player_get_independent),
        Some(player_set_independent)
    );
    prop!("zFactor", Some(player_get_z), Some(player_set_z));
    prop!(
        "cameraTarget",
        Some(player_get_camera_target),
        Some(player_set_camera_target)
    );
    prop!(
        "cameraPosition",
        Some(player_get_camera_position),
        Some(player_set_camera_position)
    );
    prop!("cameraFOV", Some(player_get_fov), Some(player_set_fov));
    prop!(
        "cameraAlive",
        Some(player_get_camera_alive),
        Some(player_set_camera_alive)
    );
    prop!(
        "canvasCaptureEnabled",
        Some(player_get_canvas_capture),
        Some(player_set_canvas_capture)
    );
    prop!(
        "clearEnabled",
        Some(player_get_clear_enabled),
        Some(player_set_clear_enabled)
    );
    prop!(
        "hitThreshold",
        Some(player_get_hit_threshold),
        Some(player_set_hit_threshold)
    );
    prop!(
        "preview",
        Some(player_get_preview),
        Some(player_set_preview)
    );
    prop!(
        "outsideFactor",
        Some(player_get_outside),
        Some(player_set_outside)
    );
    prop!(
        "resourceManager",
        Some(player_get_resource_manager),
        Some(player_set_resource_manager)
    );
    prop!(
        "stealthChara",
        Some(player_get_stealth_chara),
        Some(player_set_stealth_chara)
    );
    prop!(
        "stealthMotion",
        Some(player_get_stealth_motion),
        Some(player_set_stealth_motion)
    );
    prop!("tags", Some(player_get_tags), Some(player_set_tags));
    prop!(
        "project",
        Some(player_get_project),
        Some(player_set_project)
    );
    prop!("useD3D", Some(player_get_use_d3d), Some(player_set_use_d3d));
    prop!(
        "meshline",
        Some(player_get_meshline),
        Some(player_set_meshline)
    );
    prop!("busy", Some(player_get_busy), None);
    if emote {
        prop!("module", Some(player_emote_get_module), None);
        prop!(
            "smoothing",
            Some(player_emote_get_smoothing),
            Some(player_emote_set_smoothing)
        );
        prop!(
            "meshDivisionRatio",
            Some(player_emote_get_mesh_division),
            Some(player_emote_set_mesh_division)
        );
        prop!(
            "queing",
            Some(player_emote_get_queing),
            Some(player_emote_set_queing)
        );
        prop!(
            "bodyScale",
            Some(player_emote_get_body),
            Some(player_emote_set_body)
        );
        prop!(
            "hairScale",
            Some(player_emote_get_hair),
            Some(player_emote_set_hair)
        );
        prop!(
            "partsScale",
            Some(player_emote_get_parts),
            Some(player_emote_set_parts)
        );
        prop!(
            "bustScale",
            Some(player_emote_get_bust),
            Some(player_emote_set_bust)
        );
        prop!(
            "visible",
            Some(player_emote_get_visible),
            Some(player_emote_set_visible)
        );
        // NB: the reference registers both a `progress` property and a
        // `progress(dt)` method; the compat member table stores one member per
        // name, so the callable method wins.
        prop!(
            "modified",
            Some(player_emote_get_modified),
            Some(player_emote_set_modified)
        );
        prop!(
            "drawvisible",
            Some(player_emote_get_draw_visible),
            Some(player_emote_set_draw_visible)
        );
        prop!(
            "drawOpacity",
            Some(player_emote_get_draw_opacity),
            Some(player_emote_set_draw_opacity)
        );
        prop!(
            "opengl",
            Some(player_emote_get_opengl),
            Some(player_emote_set_opengl)
        );
        prop!("animating", Some(player_emote_get_animating), None);
        prop!("playCallback", Some(player_emote_get_play_callback), None);
    }

    ctx.register_instance(&NativeInstanceBuilder {
        name: class_name,
        create: player_create,
        destroy: player_destroy,
        invalidate: None,
        methods,
        properties,
    })?;
    Ok(())
}

include!("player_members.rs");

#[cfg(test)]
mod tests {
    use super::FrameState;
    use super::color_bytes;
    use super::local_matrix;

    #[test]
    fn color_channels_are_scaled_from_the_normalized_animator() {
        // The animator stores normalized channels; the rasterizer takes bytes.
        // The reference keeps 0..255 floats in `rgbaBytes`
        // (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:1554-1565`).
        assert_eq!(color_bytes([1.0, 1.0, 1.0, 1.0]), [255, 255, 255, 255]);
        assert_eq!(color_bytes([0.0, 0.5, 0.25, 0.0]), [0, 128, 64, 0]);
        assert_eq!(color_bytes([2.0, -1.0, 1.0, 1.0]), [255, 0, 255, 255]);
    }

    #[test]
    fn the_node_matrix_applies_angle_then_zoom() {
        // The default `transformOrder` is flip, angle, zoom, slant, each
        // left-multiplied into the local 2x2, and the stored angle is in
        // degrees (`reference/cpp/plugins/motionplayer/
        // PlayerUpdateLayersInternal.h:267-336`).
        let state = FrameState {
            angle: 90.0,
            scale_x: 2.0,
            scale_y: 3.0,
            ..FrameState::default()
        };
        let matrix = local_matrix(&state, &[0, 1, 2, 3]);
        assert!(matrix[0].abs() < 1e-9, "m11 = cos*sx: {matrix:?}");
        assert!((matrix[1] + 2.0).abs() < 1e-9, "m12 = -sin*sx: {matrix:?}");
        assert!((matrix[2] - 3.0).abs() < 1e-9, "m21 = sin*sy: {matrix:?}");
        assert!(matrix[3].abs() < 1e-9, "m22 = cos*sy: {matrix:?}");
    }
}
