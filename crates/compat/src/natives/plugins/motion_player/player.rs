//! `Motion.Player` / top-level `Player` — the reference player core.
//!
//! Behavioural reference: `reference/cpp/plugins/motionplayer/Player.h`,
//! `PlayerCore.cpp`, `PlayerTimeline.cpp`, `PlayerVariable.cpp`,
//! `PlayerFrameProgress.cpp`, `PlayerUpdate*.cpp` and `PlayerLayerQuery.cpp`.
//!
//! The port keeps the whole script-visible surface and the playback model
//! (timeline state, variable animators, coordinate/scale/rotate/color
//! transitions, wind / outer force, hit testing, layer queries). Node-tree
//! evaluation reads the selected clip's timeline time; drawing composes the
//! L2 `engine_api::Scene` layer system: [`PlayerInst::draw`] maps each node's
//! icon pixels onto its transformed quad with `engine_api::layer_ops`,
//! decoding the container's `RL`/palettized pixel chunks itself.
#![allow(unsafe_code)]

use std::collections::BTreeSet;
use std::sync::Arc;

use tjs::{DetachedValue, RetainedValue, TjsValue, VAL_OBJECT, Value};

use super::StoredValue;
use super::adaptors::{LayerGetterInst, SeparateInst, populate_layer_getter};
use super::animator::{Animator, ColorAnimator, ease_weight};
use super::helpers::{eval_object, inst_mut, inst_ref, set_real, set_string, value_as_f64};
use super::model::{
    DEFAULT_NODE_COLORS, FrameState, IconInfo, MotionClip, MotionEvent, MotionNode, MotionSnapshot,
    TimelineState, VariableFrame, build_node_tree, build_node_tree_at, child_clip_time, child_slot,
    evaluate_frames, names_first_child, prime_timelines, scan_frame_events, step_timelines,
};
use super::resource::{MotionStore, ResourceInst};
use super::texture::Pixels;
use crate::collections::Map;
use crate::engine_api::layer_ops::{
    AffineBlit, BlitOptions, BlitSource, RectI, affine_blit, fill_mask, intersect_rect,
    operate_rect,
};
use crate::engine_api::{BitmapState, Scene, Storage};
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

/// A cropped icon source tinted by one node's four vertex colours.
///
/// The crop and the decoded atlas it was cropped from identify the source, so
/// two icons that share a crop and a colour share one tinted bitmap.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct TintKey {
    colors: [[u8; 4]; 4],
    half_alpha: bool,
    crop: RectI,
    pixels: usize,
}

/// The tinted icon sources the player draws, keyed by source, colour and the
/// decoded atlas they were cropped from. The entries are dropped when the
/// bound motion changes.
pub(crate) type TintedSources = Map<TintKey, Arc<BitmapState>>;

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
    /// The packed color weight, kept in the channel order the script does not
    /// read: the getter and the setter swap the red and blue channels.
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
    pub(crate) motions_by_key: Map<String, ResolvedMotion>,
    pub(crate) timelines: Map<String, TimelineState>,
    pub(crate) playing_timelines: Vec<String>,
    pub(crate) variable_values: Map<String, f64>,
    pub(crate) variable_animators: Map<String, Animator>,
    pub(crate) nodes: Vec<MotionNode>,
    /// Whether the node tree needs an evaluation before it is drawn.
    pub(crate) layers_dirty: bool,
    pub(crate) pending_events: Vec<MotionEvent>,
    /// The tinted icon sources the active motion draws, keyed by source and
    /// player colour.
    pub(crate) tinted: TintedSources,
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
    pub(crate) layer_ids: Map<String, i32>,
    pub(crate) layer_names: Map<i32, String>,
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
            motions_by_key: Map::default(),
            timelines: Map::default(),
            playing_timelines: Vec::new(),
            variable_values: Map::default(),
            variable_animators: Map::default(),
            nodes: Vec::new(),
            layers_dirty: true,
            pending_events: Vec::new(),
            tinted: Map::default(),
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
            layer_ids: Map::default(),
            layer_names: Map::default(),
            next_layer_id: 0,
        }
    }
}

impl PlayerInst {
    pub(crate) fn playing(&self) -> bool {
        self.allplaying
    }

    /// Recompute the playing timeline list and report whether any timeline is
    /// still playing.
    pub(crate) fn playing_now(&mut self) -> bool {
        self.prune_playing_timelines();
        self.allplaying
    }

    /// Drop timelines that stopped and recompute `allplaying`.
    pub(crate) fn prune_playing_timelines(&mut self) {
        let timelines = &self.timelines;
        self.playing_timelines
            .retain(|label| timelines.get(label).is_some_and(|state| state.playing));
        self.allplaying = !self.playing_timelines.is_empty();
    }

    /// The clip whose timeline current time evaluates the node tree.
    fn active_clip_label(&self) -> Option<&str> {
        let snapshot = self.snapshot.as_ref()?;
        if !self.motion_key.is_empty() && snapshot.clip_index.contains_key(&self.motion_key) {
            return Some(&self.motion_key);
        }
        for label in &self.playing_timelines {
            if snapshot.clip_index.contains_key(label) {
                return Some(label);
            }
        }
        let primary = if snapshot.main_timelines.is_empty() {
            &snapshot.diff_timelines
        } else {
            &snapshot.main_timelines
        };
        for label in primary {
            if snapshot.clip_index.contains_key(label) {
                return Some(label);
            }
        }
        snapshot.clips.first().map(|clip| clip.label.as_str())
    }

    /// The evaluation clock: the selected clip's timeline time, or the first
    /// playing timeline time when the selected clip has no timeline.
    pub(crate) fn eval_time(&self) -> f64 {
        if let Some(label) = self.active_clip_label()
            && let Some(state) = self.timelines.get(label)
        {
            return state.current_time;
        }
        for label in &self.playing_timelines {
            if let Some(state) = self.timelines.get(label) {
                return state.current_time;
            }
        }
        0.0
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
        self.clear_source_cache();
        self.layers_dirty = true;
    }

    /// Drop the tinted sources and the missing-source report so the next draw
    /// rebuilds them.
    fn clear_source_cache(&mut self) {
        self.tinted.clear();
        self.missing_textures.clear();
    }

    /// Drop the cached sources: the layer ids, the tinted bitmaps and the
    /// decoded pixel chunks, so the next draw reads and decodes them again.
    pub(crate) fn clear_cache(&mut self) {
        self.layer_ids.clear();
        self.layer_names.clear();
        self.tinted.clear();
        if let Some(snapshot) = self.snapshot.as_ref() {
            snapshot.pixels.clear();
        }
    }

    /// Activate a resolved motion and rebuild the timeline, node and variable
    /// state it owns.
    fn activate_motion(
        &mut self,
        resolved: &ResolvedMotion,
        ctx: &CompatCtx,
    ) -> Result<(), String> {
        self.clear_source_cache();
        self.snapshot = Some(resolved.snapshot.clone());
        self.timelines.clear();
        self.playing_timelines.clear();
        prime_timelines(&resolved.snapshot, &mut self.timelines);
        let clip = self.select_active_clip(&resolved.snapshot);
        self.nodes = build_node_tree(&resolved.snapshot, clip);
        self.layers_dirty = true;
        // A motion activates behind the frame gate: the next progress advances
        // no clock and only presents the first pose.
        self.queuing = true;
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
            self.clear_source_cache();
            self.layers_dirty = true;
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
        self.clear_source_cache();
        self.layers_dirty = true;
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
    ///
    /// `target` is the retained id of the script object that receives the
    /// `onAction` and `onSync` callbacks; a null id means there is none.
    pub(crate) fn progress(&mut self, dt_ms: f64, ctx: &CompatCtx, target: tjs::Tjs2ValueId) {
        self.pending_events.clear();
        if !self.speed {
            return;
        }
        // A delta outside the accepted range is dropped rather than clamped to
        // the bound, so one bad tick cannot skip the animation ahead.
        let dt_ms = if (0.0..=60000.0).contains(&dt_ms) {
            dt_ms
        } else {
            0.0
        };
        let frames = dt_ms * 60.0 / 1000.0;
        self.frame_last_time = frames;
        self.busy = false;
        if self.queuing {
            // A motion that was just activated records the delta but leaves
            // every clock alone; the layer update that follows clears the gate.
            self.present_pose();
            self.dispatch_events(ctx, target);
            return;
        }
        self.frame_tick_count += frames;
        self.coord.x.step(frames as f32);
        self.coord.y.step(frames as f32);
        self.scale.step(frames as f32);
        self.rotate.step(frames as f32);
        if self.wind.active {
            self.wind.phase += frames;
        }
        self.advance_timelines(frames);
        self.update_layers();
        self.step_animators(frames as f32);
        self.dispatch_events(ctx, target);
    }

    /// Present the current pose without advancing any clock.
    ///
    /// A motion that was just activated sits behind the frame gate: the tick
    /// that follows it only shows the first pose, and the layer update that
    /// runs here releases the gate.
    fn present_pose(&mut self) {
        self.prune_playing_timelines();
        self.sync_active = self.sync_waiting && self.allplaying;
        self.update_layers();
    }

    /// The playback position as a `0..=1` ratio, or `1.0` once nothing plays.
    pub(crate) fn progress_ratio(&self) -> f64 {
        let mut progress = 0.0f64;
        let mut any_playing = false;
        for state in self.timelines.values() {
            any_playing = any_playing || state.playing;
            if state.total_frames > 0.0 {
                progress = progress.max((state.current_time / state.total_frames).clamp(0.0, 1.0));
            } else if !state.playing {
                progress = progress.max(1.0);
            }
        }
        if !any_playing {
            return 1.0;
        }
        progress
    }

    /// Advance everything by `frames` frame units without dispatching events.
    pub(crate) fn frame_progress(&mut self, frames: f64) {
        if !self.speed {
            return;
        }
        self.frame_last_time = frames;
        if self.queuing {
            self.present_pose();
            return;
        }
        self.frame_tick_count += frames;
        self.advance_timelines(frames);
        self.update_layers();
    }

    fn advance_timelines(&mut self, frames: f64) {
        let previous: Vec<(String, f64)> = self
            .playing_timelines
            .iter()
            .filter_map(|label| {
                self.timelines
                    .get(label)
                    .map(|state| (label.clone(), state.current_time))
            })
            .collect();
        if let Some(snapshot) = self.snapshot.clone() {
            let events = step_timelines(&snapshot, &mut self.timelines, frames);
            self.pending_events.extend(events);
            for (label, previous) in previous {
                let current = self.timelines.get(&label).map(|state| state.current_time);
                if let Some(current) = current {
                    let events = scan_frame_events(&snapshot.event_tracks, previous, current);
                    self.pending_events.extend(events);
                }
            }
        }
        self.prune_playing_timelines();
        self.sync_active = self.sync_waiting && self.allplaying;
    }

    pub(crate) fn step_animators(&mut self, dt: f32) {
        for animator in self.variable_animators.values_mut() {
            animator.step(dt);
        }
    }

    /// The clock of the node at `index`, in its own clip's time space.
    ///
    /// A node of the selected clip runs on that clip's clock. A layer a child
    /// motion expanded runs on the child clip's clock: the owner's clock
    /// measured from the keyframe that named the clip, plus that keyframe's
    /// `motion.timeOffset`, bounded by the child clip's own length.
    fn node_clock(&self, index: usize, time: f64, clocks: &[f64]) -> f64 {
        let node = &self.nodes[index];
        let Some(owner) = node.child_owner else {
            return (time - node.time_offset).max(0.0);
        };
        // The child's offset carries the keyframe that named the clip and that
        // keyframe's motion time offset.
        let start = node.time_offset - self.nodes[owner].time_offset;
        let local = (clocks[owner] - start).max(0.0);
        let Some(clip) = node
            .child_clip
            .as_deref()
            .and_then(|label| self.snapshot.as_deref().and_then(|s| s.clip(label)))
        else {
            return local;
        };
        child_clip_time(clip, local)
    }

    /// The clock of every node, in its own clip's time space.
    fn node_clocks(&self, time: f64) -> Vec<f64> {
        let mut clocks = Vec::with_capacity(self.nodes.len());
        for (index, _) in self.nodes.iter().enumerate() {
            let clock = self.node_clock(index, time, &clocks);
            clocks.push(clock);
        }
        clocks
    }

    /// Evaluate the node tree at the selected clip's timeline time.
    pub(crate) fn update_layers(&mut self) {
        if self.nodes.is_empty() {
            self.layers_dirty = false;
            return;
        }
        let time = self.eval_time();
        // A child clip instance follows the keyframe that is active now, so
        // its clock offset is refreshed before the layers are evaluated.
        if self.sync_child_instances(time) {
            let snapshot = self.snapshot.clone();
            if let Some(snapshot) = snapshot {
                let clip = self.select_active_clip(&snapshot);
                self.nodes = build_node_tree_at(&snapshot, clip, time);
                self.sync_child_instances(time);
            }
        }
        // Evaluate each node's local state. A layer expanded from a child motion
        // runs on that clip's own clock: the parent's clock measured from the
        // keyframe that named the clip and bounded by the child clip's length.
        let clocks = self.node_clocks(time);
        for (index, &local_time) in clocks.iter().enumerate() {
            let state = evaluate_frames(
                &self.nodes[index].frames,
                local_time,
                self.nodes[index].coordinate_mode,
            );
            let node = &mut self.nodes[index];
            node.local = state;
            node.draw_flag = true;
        }

        // The root viewport state is applied before the children inherit it,
        // so a visibility or opacity change takes effect in the same frame.
        let root = &mut self.nodes[0];
        root.accumulated.x = root.local.x + self.coord.x.value() as f64 + self.root_x;
        root.accumulated.y = root.local.y + self.coord.y.value() as f64 + self.root_y;
        root.accumulated.ox = root.local.ox;
        root.accumulated.oy = root.local.oy;
        root.accumulated.active = self.visible;
        root.accumulated.opacity = self.opacity;
        root.accumulated.angle = self.rotate.value() as f64;
        root.accumulated.scale_x = self.scale.value() as f64;
        root.accumulated.scale_y = self.scale.value() as f64;
        root.matrix = local_matrix(&root.accumulated, &root.transform_order);
        root.draw_flag = self.visible;
        let snapshot = self.snapshot.clone();
        let snapshot = snapshot.as_deref();
        // Accumulate the parent transform into world coordinates: the child's
        // position is the parent matrix applied to its local offset, and its
        // matrix is the parent's composed with its own.
        for index in 1..self.nodes.len() {
            let parent = self.nodes[index].parent;
            let mut local = self.nodes[index].local.clone();
            let parent_state = self.nodes[parent].accumulated.clone();
            let parent_matrix = self.nodes[parent].matrix;
            let transform_order = self.nodes[index].transform_order;
            let local_2x2 = local_matrix(&local, &transform_order);
            let mut world_x =
                parent_matrix[0] * local.x + parent_matrix[1] * local.y + parent_state.x;
            let mut world_y =
                parent_matrix[2] * local.x + parent_matrix[3] * local.y + parent_state.y;
            let mut world_z = parent_state.z + local.z;
            // A child clip is placed at its motion layer's origin corner.
            if self.nodes[index].child_owner == Some(parent) {
                let origin_x = parent_state.ox;
                let origin_y = parent_state.oy;
                world_x -= parent_matrix[1] * origin_y + origin_x * parent_matrix[0];
                world_y -= origin_y * parent_matrix[3] + origin_x * parent_matrix[2];
            }
            // A child of a mesh node is warped through the parent's patch in
            // the parent's own clip space, before its transform is composed.
            let mut mesh_angle = 0.0;
            let mut mesh_scale = 1.0;
            if self.nodes[parent].mesh_type == 1 {
                let parent_geometry = icon_geometry(snapshot, &self.nodes[parent].local.src);
                if let Some((delta_angle, scale)) = mesh_deform(
                    &self.nodes[parent],
                    parent_geometry,
                    &mut world_x,
                    &mut world_y,
                    &mut world_z,
                    self.nodes[index].coordinate_mode,
                ) {
                    mesh_angle = delta_angle;
                    mesh_scale = scale;
                }
            }
            if mesh_angle != 0.0 || mesh_scale != 1.0 {
                local.angle += mesh_angle;
                local.scale_x *= mesh_scale;
                local.scale_y *= mesh_scale;
            }
            let local_2x2 = if mesh_angle != 0.0 || mesh_scale != 1.0 {
                local_matrix(&local, &transform_order)
            } else {
                local_2x2
            };
            let child_active = match self.nodes[index].child_owner {
                Some(owner) => self.nodes[owner].child_slot.as_ref().is_some_and(|slot| {
                    self.nodes[index].child_clip.as_deref() == Some(slot.clip.as_str())
                }),
                None => true,
            };
            let node = &mut self.nodes[index];
            node.accumulated = local.clone();
            node.accumulated.x = world_x;
            node.accumulated.y = world_y;
            node.accumulated.z = world_z;
            // The visibility is inherited: a node draws only when every
            // ancestor does.
            node.accumulated.active = parent_state.active && local.active;
            node.accumulated.opacity = parent_state.opacity * local.opacity;
            node.accumulated.angle = parent_state.angle + local.angle;
            node.accumulated.slant_x = parent_state.slant_x + local.slant_x;
            node.accumulated.slant_y = parent_state.slant_y + local.slant_y;
            node.accumulated.flip_x = parent_state.flip_x != local.flip_x;
            node.accumulated.flip_y = parent_state.flip_y != local.flip_y;
            node.accumulated.scale_x = parent_state.scale_x * local.scale_x;
            node.accumulated.scale_y = parent_state.scale_y * local.scale_y;
            node.matrix = [
                parent_matrix[0] * local_2x2[0] + parent_matrix[1] * local_2x2[2],
                parent_matrix[0] * local_2x2[1] + parent_matrix[1] * local_2x2[3],
                parent_matrix[2] * local_2x2[0] + parent_matrix[3] * local_2x2[2],
                parent_matrix[2] * local_2x2[1] + parent_matrix[3] * local_2x2[3],
            ];
            // A layer expanded from a child motion exists only from the
            // parent keyframe that named the clip, so it stays hidden before
            // that moment.
            node.draw_flag = parent_state.active
                && local.active
                && parent_state.opacity > 0.0
                && child_active
                && time >= node.visible_from;
        }
        self.refresh_geometry(snapshot);
        let vertices = self.nodes.iter().filter(|node| node.draw_flag).count();
        self.processed_mesh_vertices_num = vertices as i32;
        // The gate a freshly activated motion holds is released by the first
        // layer update.
        self.queuing = false;
        self.layers_dirty = false;
    }

    /// Refresh every type-3 node's child clip instance clock, and report
    /// whether a keyframe selected a child clip the tree was not built for.
    fn sync_child_instances(&mut self, time: f64) -> bool {
        let mut rebuild = false;
        // The clocks are walked in node order: a node's owner comes first, so
        // a type-3 node inside a child instance reads the clock its owner has
        // already been given.
        let mut clocks = Vec::with_capacity(self.nodes.len());
        let mut index = 0;
        while index < self.nodes.len() {
            let clock = self.node_clock(index, time, &clocks);
            clocks.push(clock);
            if self.nodes[index].node_type == 3
                && let Some(slot) = child_slot(&self.nodes[index].frames, clock)
            {
                self.nodes[index].child_slot = Some(slot.clone());
                let child_offset = self.nodes[index].time_offset + slot.start - slot.offset;
                // The clip a type-3 node names first exists from the player's
                // start; a clip that replaces it later starts at the keyframe
                // that named it.
                let child_start = if names_first_child(&self.nodes[index].frames, &slot) {
                    self.nodes[index].visible_from
                } else {
                    self.nodes[index].visible_from + slot.start
                };
                let known = self
                    .snapshot
                    .as_deref()
                    .is_some_and(|snapshot| snapshot.clip(&slot.clip).is_some());
                let mut found = false;
                for child in self.nodes.iter_mut() {
                    if child.child_owner != Some(index) {
                        continue;
                    }
                    if child.child_clip.as_deref() != Some(slot.clip.as_str()) {
                        rebuild = true;
                        continue;
                    }
                    found = true;
                    child.time_offset = child_offset;
                    child.visible_from = child_start;
                }
                if !found && known {
                    rebuild = true;
                }
            }
            index += 1;
        }
        rebuild
    }

    /// Rebuild the mesh, vertex and clip geometry of every node.
    fn refresh_geometry(&mut self, snapshot: Option<&MotionSnapshot>) {
        for index in 1..self.nodes.len() {
            let parent = self.nodes[index].parent;
            let parent_clip = self.nodes[parent].parent_clip;
            self.nodes[index].parent_clip = parent_clip;
            let geometry = icon_geometry(snapshot, &self.nodes[index].local.src);
            refresh_node_geometry(&mut self.nodes, index, geometry, self.z_factor);
            refresh_shape_bounds(&mut self.nodes, index, self.z_factor);
        }
    }

    /// Dispatch the queued action and sync events to the script object.
    ///
    /// `target` is the retained id of the receiver. A null id means there is
    /// no receiver, and an id that names no live object makes the member
    /// lookup fail instead of touching the object.
    pub(crate) fn dispatch_events(&mut self, ctx: &CompatCtx, target: tjs::Tjs2ValueId) {
        if self.pending_events.is_empty() || target.is_null() {
            return;
        }
        let events = std::mem::take(&mut self.pending_events);
        for event in events {
            let (name, arguments) = if event.kind == 0 {
                (
                    "onAction",
                    vec![
                        TjsValue::String(event.param1),
                        TjsValue::String(event.param2),
                    ],
                )
            } else {
                ("onSync", Vec::new())
            };
            // A class without the callback stays a no-op; a present callback
            // that raises is reported.
            // SAFETY: the binding is live and `target` is a retained id.
            if unsafe { ctx.engine() }.member(target, name).is_err() {
                continue;
            }
            // SAFETY: the binding is live and `target` is a retained id.
            let called = unsafe { ctx.engine() }.call_member(target, name, &arguments);
            if let Err(error) = called {
                log::warn!("Motion.Player: {name} callback failed: {error}");
            }
        }
    }

    pub(crate) fn get_variable_value(&self, label: &str) -> f64 {
        if let Some(value) = self.variable_values.get(label) {
            return *value;
        }
        self.snapshot
            .as_deref()
            .map_or(0.0, |snapshot| default_variable(snapshot, label))
    }

    /// The keyframes of the variable at `index`, or an empty slice.
    pub(crate) fn variable_frames_at(&self, index: usize) -> &[VariableFrame] {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return &[];
        };
        let Some(label) = snapshot.variable_labels.get(index) else {
            return &[];
        };
        snapshot
            .variable_frames
            .get(label.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default()
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
        // The clip the play call names is the one the player shows, even when
        // the label names no timeline: the active clip is selected before the
        // node state is rebuilt.
        if let Some(clip) = self.select_active_clip(&snapshot) {
            self.loop_time = clip.loop_time;
            self.frame_last_time = clip.total_frames;
            self.nodes = build_node_tree_at(&snapshot, Some(clip), self.eval_time());
            self.layers_dirty = true;
            self.ensure_root_offsets();
        }
        // A play that does not chain re-arms the frame gate, so the first
        // progress after it presents the new pose without advancing.
        if (flags & 2) == 0 {
            self.queuing = true;
        }
        self.prune_playing_timelines();
        Ok(started)
    }

    /// The clip the player shows.
    ///
    /// The bound motion key wins when it names a clip, because a play call
    /// commits its label there before the node state is rebuilt; then the
    /// playing timelines, the module's primary labels, and finally the first
    /// clip.
    pub(crate) fn select_active_clip<'a>(
        &self,
        snapshot: &'a MotionSnapshot,
    ) -> Option<&'a MotionClip> {
        if !self.motion_key.is_empty()
            && let Some(clip) = snapshot.clip(&self.motion_key)
        {
            return Some(clip);
        }
        for label in &self.playing_timelines {
            if let Some(clip) = snapshot.clip(label) {
                return Some(clip);
            }
        }
        let primary = if snapshot.main_timelines.is_empty() {
            &snapshot.diff_timelines
        } else {
            &snapshot.main_timelines
        };
        for item in primary {
            if let Some(clip) = snapshot.clip(item) {
                return Some(clip);
            }
        }
        snapshot.clips.first()
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

    /// The labels of the layers this player owns.
    ///
    /// A label that several nodes share is reported once, and a layer without
    /// a label contributes none; the labels come out in sorted order.
    pub(crate) fn layer_labels(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .nodes
            .iter()
            .filter(|node| node.own && !node.label.is_empty())
            .map(|node| node.label.clone())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// The node a layer label names.
    ///
    /// The map keeps the last node that wrote the label, so a label that
    /// several nodes share answers with the last one.
    pub(crate) fn layer_node(&self, name: &str) -> Option<&MotionNode> {
        self.nodes
            .iter()
            .rev()
            .find(|node| node.own && node.label == name)
    }

    /// The layer getter the script receives: a `Motion.LayerGetter`
    /// instance filled with the node's state.
    pub(crate) fn layer_getter_object(
        &self,
        ctx: &CompatCtx,
        node: &MotionNode,
    ) -> Result<DetachedValue, String> {
        let getter = eval_object(ctx, "new Motion.LayerGetter()")?;
        // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `layer_getter_object`.
        let engine = unsafe { ctx.engine() };
        let Some(payload) = engine
            .native_instance_payload(getter.raw_id(), "LayerGetter")
            .map_err(|error| error.to_string())?
        else {
            return Err("motionplayer: the layer getter carries no payload".to_string());
        };
        // SAFETY: the payload is the `LayerGetterInst` the class constructor
        // created, and the object stays alive for the rest of this function.
        let payload = unsafe { &mut *(payload.as_ptr() as *mut LayerGetterInst) };
        let (mtx, vtx, color, origin) = self.layer_getter_state(node);
        populate_layer_getter(payload, node, ctx, origin, mtx, vtx, color)?;
        Ok(getter)
    }

    /// The matrix, the vertex list, the colour and the origin the layer
    /// getter reports for a node.
    ///
    /// The matrix is the node's accumulated transform and its position. The
    /// vertex list is the quad the node draws: its origin corner and the
    /// three corners the node's transform produces. The colour is the node's
    /// own four bytes.
    fn layer_getter_state(&self, node: &MotionNode) -> ([f64; 6], [f64; 8], [i64; 4], (f64, f64)) {
        let matrix = node.matrix;
        let mtx = [
            matrix[0],
            matrix[1],
            matrix[2],
            matrix[3],
            node.accumulated.x,
            node.accumulated.y,
        ];
        let icon = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.icons.get(&node.local.src));
        let (origin_x, origin_y) = icon.map_or((0.0, 0.0), |icon| (icon.origin_x, icon.origin_y));
        let color = node.accumulated.colors[0].map(i64::from);
        // A node without a source draws no box, so it reports no corners.
        let Some(icon) = icon else {
            return (mtx, [0.0; 8], color, (origin_x, origin_y));
        };
        let (width, height) = (icon.width, icon.height);
        // The origin corner of the node's box: the position less the box's
        // origin offset carried through the transform.
        let total_x = origin_x + node.accumulated.ox;
        let total_y = origin_y + node.accumulated.oy;
        let corner_x = node.accumulated.x - (matrix[1] * total_y + total_x * matrix[0]);
        let corner_y = node.accumulated.y - (total_y * matrix[3] + total_x * matrix[2]);
        // A mesh node reports the four corners of its deformed grid.
        let vtx = match grid_corners(node) {
            Some(corners) => corners,
            None => [
                corner_x,
                corner_y,
                corner_x + matrix[0] * width,
                corner_y + matrix[2] * width,
                corner_x + matrix[0] * width + matrix[1] * height,
                corner_y + matrix[2] * width + matrix[3] * height,
                corner_x + matrix[1] * height,
                corner_y + matrix[3] * height,
            ],
        };
        (mtx, vtx, color, (origin_x, origin_y))
    }

    /// Whether the point `(x, y)` hits the layer named `label`.
    ///
    /// The label is looked up in the layer map, which keeps the last node that
    /// wrote it, and the node's shape geometry answers the point. Only a shape
    /// node carries that geometry: it is the node's position, its clip origin
    /// and its transform.
    pub(crate) fn hit_test(&self, label: &str, x: f64, y: f64) -> bool {
        if label.is_empty() {
            return false;
        }
        // The layer map keeps the last node that wrote a label.
        let Some(node) = self.nodes.iter().rev().find(|node| node.label == label) else {
            return false;
        };
        if node.node_type != 1 {
            return false;
        }
        let half = node.accumulated.scale_x.abs() * 8.0;
        match node.shape {
            // A circle of the node's zoom.
            1 => {
                let radius = half;
                let dx = x - node.accumulated.x;
                let dy = y - node.accumulated.y;
                dx * dx + dy * dy <= radius * radius
            }
            // A rectangle of the node's zoom.
            2 => {
                let half_w = half;
                let half_h = node.accumulated.scale_y.abs() * 8.0;
                x >= node.accumulated.x - half_w
                    && x < node.accumulated.x + half_w
                    && y >= node.accumulated.y - half_h
                    && y < node.accumulated.y + half_h
            }
            // A quad the node's transform maps.
            3 => {
                let m = node.matrix;
                let origin_x = node.accumulated.ox;
                let origin_y = node.accumulated.oy;
                let offset_x = origin_x * m[0] + origin_y * m[1];
                let offset_y = origin_x * m[2] + origin_y * m[3];
                let px = node.accumulated.x;
                let py = node.accumulated.y;
                let (ax, bx) = (m[0] * -8.0, m[1] * -8.0);
                let (cx, dx) = (m[0] * 8.0, m[1] * 8.0);
                let (ay, by) = (m[2] * -8.0, m[3] * -8.0);
                let (cy, dy) = (m[2] * 8.0, m[3] * 8.0);
                let corners = [
                    (px + ax + bx - offset_x, py + ay + by - offset_y),
                    (px + cx + bx - offset_x, py + cy + by - offset_y),
                    (px + cx + dx - offset_x, py + cy + dy - offset_y),
                    (px + ax + dx - offset_x, py + ay + dy - offset_y),
                ];
                let cross = |a: (f64, f64), b: (f64, f64), p: (f64, f64)| {
                    (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0)
                };
                let winding = if cross(corners[0], corners[1], corners[2]) >= 0.0 {
                    1.0
                } else {
                    -1.0
                };
                let point = (x, y);
                winding * cross(corners[0], corners[1], point) >= 0.0
                    && winding * cross(corners[1], corners[2], point) >= 0.0
                    && winding * cross(corners[2], corners[3], point) >= 0.0
                    && winding * cross(corners[3], corners[0], point) >= 0.0
            }
            // A point never hits.
            _ => false,
        }
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
        // The progress path evaluates the tree; a draw evaluates it only when
        // the tree changed since the last evaluation.
        if self.layers_dirty {
            self.update_layers();
        }
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
        // SAFETY: the host keeps the attached storage alive for the binding.
        let storage = unsafe { ctx.services() }.storage();
        // SAFETY: the host keeps the attached storage alive for the binding.
        let storage = storage.map(|storage| unsafe { &*storage });
        self.draw_into_scene(scene, layer_id, storage);
        Ok(())
    }

    /// Clear the target image and composite the current pose into it in place.
    fn draw_into_scene(&mut self, scene: &mut Scene, layer_id: u32, storage: Option<&Storage>) {
        let Some(bitmap_id) = scene.layer(layer_id).and_then(|layer| layer.bitmap) else {
            return;
        };
        // A pending image is waited for before the target is cleared, so the
        // pose composites onto the pixels the bitmap declares.
        if !scene.resolve_bitmap(bitmap_id) {
            return;
        }
        let Some(bitmap) = scene.bitmap_mut(bitmap_id) else {
            return;
        };
        if bitmap.width == 0 || bitmap.height == 0 {
            return;
        }
        let declared_bytes = (bitmap.width as usize)
            .checked_mul(bitmap.height as usize)
            .and_then(|pixels| pixels.checked_mul(4));
        if declared_bytes != Some(bitmap.rgba.len()) {
            log::error!(
                "Motion.Player.draw: the {}x{} target bitmap holds {} bytes of pixels instead of \
                 its declared size; nothing is drawn",
                bitmap.width,
                bitmap.height,
                bitmap.rgba.len()
            );
            return;
        }
        let snapshot = self.snapshot.clone();
        let color = color_bytes(self.color.bytes);
        let affine = self.affine;
        let nodes = &self.nodes;
        let mut tinted = std::mem::take(&mut self.tinted);
        // A draw leaves the target image in place unless the player enables an
        // explicit clear; the pixels outside the drawn items survive.
        if self.clear_enabled {
            let clear = clear_fill(self.clear_enabled, self.clear_color);
            for pixel in bitmap.rgba.chunks_exact_mut(4) {
                pixel.copy_from_slice(&clear);
            }
        }
        let missing = composite_pose(
            bitmap,
            nodes,
            snapshot.as_deref(),
            storage,
            DrawSettings {
                color,
                affine,
                preview: self.preview,
                mask_mode: self.mask_mode,
            },
            &mut tinted,
        );
        self.tinted = tinted;
        bitmap.mark_dirty();
        // The pose changed the target's pixels, not the layer tree: setting
        // `imageModified` must not invalidate the cached layer subtree or the
        // whole render scene is rebuilt on every motion frame.
        scene.mark_layer_image_modified(layer_id);
        for src in missing {
            if self.missing_textures.insert(src.clone()) {
                log::warn!("Motion.Player.draw: no texture for {src}");
            }
        }
    }
}

/// The state one pose is painted with.
#[derive(Clone, Copy)]
struct DrawSettings {
    color: [u8; 4],
    affine: [f64; 6],
    /// Preview playback does not splice the child-motion players into the
    /// parent's item list.
    preview: bool,
    /// The player's `maskMode`: `0` threshold-stencils a mask, any other value
    /// blends the mask alpha arithmetically.
    mask_mode: i32,
}

/// The per-draw state shared by the node painters.
struct DrawFrame<'a> {
    snapshot: Option<&'a MotionSnapshot>,
    storage: Option<&'a Storage>,
    settings: DrawSettings,
    tinted: &'a mut TintedSources,
    missing: Vec<String>,
}

/// Paint the pose over `bitmap` in node order, returning the icon sources
/// that had no readable texture.
fn composite_pose(
    bitmap: &mut BitmapState,
    nodes: &[MotionNode],
    snapshot: Option<&MotionSnapshot>,
    storage: Option<&Storage>,
    settings: DrawSettings,
    tinted: &mut TintedSources,
) -> Vec<String> {
    let mut frame = DrawFrame {
        snapshot,
        storage,
        settings,
        tinted,
        missing: Vec::new(),
    };
    let bounds = (0, 0, bitmap.width as i32, bitmap.height as i32);
    for index in 1..nodes.len() {
        if settings.preview && nodes[index].child_owner.is_some() {
            continue;
        }
        if nearest_group(nodes, index).is_some() {
            continue;
        }
        if is_composite_group(&nodes[index]) {
            draw_group(bitmap, (0, 0), bounds, nodes, index, &mut frame, 1.0);
            continue;
        }
        if node_drawable(&nodes[index]) {
            let opacity = nodes[index].accumulated.opacity;
            let mode = blend_operation_mode(nodes[index].accumulated.blend_mode);
            draw_clipped_item(bitmap, (0, 0), nodes, index, &mut frame, opacity, mode);
        }
    }
    std::mem::take(&mut frame.missing)
}

/// The target fill: transparent unless the player enables an explicit clear
/// color, which is stored as `0xAARRGGBB`.
fn clear_fill(clear_enabled: bool, color: i32) -> [u8; 4] {
    if !clear_enabled {
        return [0, 0, 0, 0];
    }
    let value = color as u32;
    [
        (value >> 16) as u8,
        (value >> 8) as u8,
        value as u8,
        (value >> 24) as u8,
    ]
}

/// The operation mode a node's blend mode resolves to.
///
/// The low four bits of the node's blend mode select the operation: `1` is
/// additive, `2` and `5` are subtractive, `3` is multiplicative, `4` is
/// screen, and every other value composites source-over.
fn blend_operation_mode(raw: i32) -> i64 {
    match raw & 0x0F {
        1 => 14,
        2 | 5 => 15,
        3 => 16,
        4 => 17,
        _ => 2,
    }
}

/// Whether a node has pixels to paint: a visible shape, or an object or
/// composite group that names an icon source and still composes any opacity.
fn node_drawable(node: &MotionNode) -> bool {
    node.draw_flag
        && node.accumulated.opacity != 0.0
        && (node.shape.is_positive()
            || ((node.node_type == 0 || node.node_type == 12) && !node.local.src.is_empty()))
}

/// Whether a node is a stencil-composite group (`type == 12`, stencil bit 2).
fn is_composite_group(node: &MotionNode) -> bool {
    node.node_type == 12 && (node.stencil_type & 4) != 0
}

/// The nearest composite group that contains `index`.
fn nearest_group(nodes: &[MotionNode], index: usize) -> Option<usize> {
    let mut current = nodes[index].parent;
    loop {
        if is_composite_group(&nodes[current]) {
            return Some(current);
        }
        if current == 0 {
            return None;
        }
        current = nodes[current].parent;
    }
}

/// The four corners of a node's deformed mesh grid: top-left, top-right,
/// bottom-right and bottom-left.
fn grid_corners(node: &MotionNode) -> Option<[f64; 8]> {
    let [div_x, div_y] = node.mesh_div;
    if div_x < 2 || div_y < 2 {
        return None;
    }
    let div_x = usize::try_from(div_x).ok()?;
    let div_y = usize::try_from(div_y).ok()?;
    if node.mesh_points.len() < div_x.checked_mul(div_y)?.checked_mul(2)? {
        return None;
    }
    let point = |index: usize| {
        (
            f64::from(node.mesh_points[index * 2]),
            f64::from(node.mesh_points[index * 2 + 1]),
        )
    };
    let corners = [
        point(0),
        point(div_x - 1),
        point(div_x * div_y - 1),
        point((div_y - 1) * div_x),
    ];
    Some([
        corners[0].0,
        corners[0].1,
        corners[1].0,
        corners[1].1,
        corners[2].0,
        corners[2].1,
        corners[3].0,
        corners[3].1,
    ])
}

/// The device-space paint box of a node's own source, floored and ceiled to
/// whole pixels.
fn node_paint_box(node: &MotionNode, frame: &DrawFrame<'_>) -> Option<(f64, f64, f64, f64)> {
    let corners: [(f64, f64); 4] = if node.shape.is_positive() {
        let matrix = node.matrix;
        let corner = |local_x: f64, local_y: f64| {
            (
                node.accumulated.x + matrix[0] * local_x + matrix[1] * local_y,
                node.accumulated.y + matrix[2] * local_x + matrix[3] * local_y,
            )
        };
        [
            corner(-32.0, -32.0),
            corner(32.0, -32.0),
            corner(32.0, 32.0),
            corner(-32.0, 32.0),
        ]
    } else if node.local.src.is_empty()
        || frame
            .snapshot
            .and_then(|snapshot| snapshot.icons.get(&node.local.src))
            .is_none()
    {
        return None;
    } else if let Some(vtx) = grid_corners(node) {
        [
            (vtx[0], vtx[1]),
            (vtx[2], vtx[3]),
            (vtx[4], vtx[5]),
            (vtx[6], vtx[7]),
        ]
    } else {
        let v = node.vertices;
        [(v[0], v[1]), (v[2], v[3]), (v[4], v[5]), (v[6], v[7])]
    };
    let mut bounds = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for (x, y) in corners {
        let (x, y) = draw_point(frame.settings.affine, x, y);
        bounds.0 = bounds.0.min(x);
        bounds.1 = bounds.1.min(y);
        bounds.2 = bounds.2.max(x);
        bounds.3 = bounds.3.max(y);
    }
    if !bounds.0.is_finite() || !bounds.1.is_finite() {
        return None;
    }
    Some((
        bounds.0.floor(),
        bounds.1.floor(),
        bounds.2.ceil(),
        bounds.3.ceil(),
    ))
}

/// The device-space rect a node is clipped to: the nearest enclosing type-7
/// shape's bounds.
fn node_viewport(
    nodes: &[MotionNode],
    node: &MotionNode,
    frame: &DrawFrame<'_>,
) -> Option<(f64, f64, f64, f64)> {
    let clip = node.parent_clip?;
    let bounds = nodes.get(clip)?.shape_aabb;
    if bounds[2] < bounds[0] || bounds[3] < bounds[1] {
        return None;
    }
    let corners = [
        (bounds[0], bounds[1]),
        (bounds[2], bounds[1]),
        (bounds[2], bounds[3]),
        (bounds[0], bounds[3]),
    ];
    let mut out = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for (x, y) in corners {
        let (x, y) = draw_point(frame.settings.affine, x, y);
        out.0 = out.0.min(x);
        out.1 = out.1.min(y);
        out.2 = out.2.max(x);
        out.3 = out.3.max(y);
    }
    Some((out.0.floor(), out.1.floor(), out.2.ceil(), out.3.ceil()))
}

/// Union two paint boxes; an invalid box yields the other.
fn union_paint_box(
    a: Option<(f64, f64, f64, f64)>,
    b: Option<(f64, f64, f64, f64)>,
) -> Option<(f64, f64, f64, f64)> {
    match (a, b) {
        (None, other) | (other, None) => other,
        (Some(a), Some(b)) => Some((a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))),
    }
}

/// The paint box of a composite group: its own source plus every item it
/// contains, following nested groups.
fn group_paint_box(
    nodes: &[MotionNode],
    group: usize,
    frame: &DrawFrame<'_>,
) -> Option<(f64, f64, f64, f64)> {
    let mut paint_box = node_paint_box(&nodes[group], frame);
    for index in 0..nodes.len() {
        if index == group || nearest_group(nodes, index) != Some(group) {
            continue;
        }
        let node = &nodes[index];
        if frame.settings.preview && node.child_owner.is_some() {
            continue;
        }
        if is_composite_group(node) {
            paint_box = union_paint_box(paint_box, group_paint_box(nodes, index, frame));
        } else if node_drawable(node) {
            paint_box = union_paint_box(paint_box, node_paint_box(node, frame));
        }
    }
    paint_box
}

/// Composite one composite group into `target`.
///
/// The group's own source and every item it contains are drawn into an
/// offscreen bitmap the size of the group's paint box clipped to its viewport;
/// the group's blend mode and accumulated opacity are applied once at the
/// composite, and a child's opacity is divided by the group's so the group
/// factor is not applied twice.
fn draw_group(
    target: &mut BitmapState,
    target_origin: (i32, i32),
    target_bounds: (i32, i32, i32, i32),
    nodes: &[MotionNode],
    group: usize,
    frame: &mut DrawFrame<'_>,
    parent_opacity: f64,
) {
    let Some(paint_box) = group_paint_box(nodes, group, frame) else {
        return;
    };
    let viewport = node_viewport(nodes, &nodes[group], frame);
    let clip = match viewport {
        Some(viewport) => (
            paint_box.0.max(viewport.0),
            paint_box.1.max(viewport.1),
            paint_box.2.min(viewport.2),
            paint_box.3.min(viewport.3),
        ),
        None => paint_box,
    };
    let clip = (
        clip.0.max(f64::from(target_bounds.0)),
        clip.1.max(f64::from(target_bounds.1)),
        clip.2.min(f64::from(target_bounds.2)),
        clip.3.min(f64::from(target_bounds.3)),
    );
    let left = clip.0 as i32;
    let top = clip.1 as i32;
    let right = clip.2 as i32;
    let bottom = clip.3 as i32;
    if right <= left || bottom <= top {
        return;
    }
    let Ok(width) = u32::try_from(right - left) else {
        return;
    };
    let Ok(height) = u32::try_from(bottom - top) else {
        return;
    };
    let mut offscreen = BitmapState::new(
        width,
        height,
        vec![0u8; width as usize * height as usize * 4],
    );
    let origin = (left, top);
    let offset = (-origin.0, -origin.1);
    let group_opacity = nodes[group].accumulated.opacity.clamp(0.0, 1.0);
    if node_drawable(&nodes[group]) {
        // The group's own source is painted source-over; the group's blend
        // mode is applied when the completed group is composited.
        draw_node(&mut offscreen, nodes, group, frame, 1.0, 2, offset);
    }
    for index in 0..nodes.len() {
        if index == group || nearest_group(nodes, index) != Some(group) {
            continue;
        }
        let node = &nodes[index];
        if frame.settings.preview && node.child_owner.is_some() {
            continue;
        }
        if is_composite_group(node) {
            draw_group(
                &mut offscreen,
                origin,
                (left, top, right, bottom),
                nodes,
                index,
                frame,
                group_opacity,
            );
            continue;
        }
        if !node_drawable(node) {
            continue;
        }
        let ratio = if group_opacity > 0.0 {
            node.accumulated.opacity / group_opacity
        } else {
            0.0
        };
        draw_clipped_item(
            &mut offscreen,
            origin,
            nodes,
            index,
            frame,
            ratio.clamp(0.0, 1.0),
            blend_operation_mode(node.accumulated.blend_mode),
        );
    }
    // A stencil group's mask layers change the composed group's alpha in the
    // mask's region once, before the group is composited.
    if !nodes[group].mask_layers.is_empty() {
        let masks: Vec<usize> = nodes[group]
            .mask_layers
            .iter()
            .filter_map(|label| nodes.iter().rposition(|node| &node.label == label))
            .collect();
        let group_flags = nodes[group].stencil_type;
        for mask_index in masks {
            let Some(mask_box) = node_drawn_box(&nodes[mask_index], frame) else {
                continue;
            };
            let mut mask_bitmap = BitmapState::new(
                width,
                height,
                vec![0u8; width as usize * height as usize * 4],
            );
            draw_node(&mut mask_bitmap, nodes, mask_index, frame, 1.0, 2, offset);
            let rect = (
                mask_box.0 as i32 - origin.0,
                mask_box.1 as i32 - origin.1,
                mask_box.2 as i32 - origin.0,
                mask_box.3 as i32 - origin.1,
            );
            apply_alpha_mask(
                &mut offscreen,
                &mask_bitmap,
                group_flags,
                frame.settings.mask_mode,
                rect,
            );
        }
    }
    let opa = ((nodes[group].accumulated.opacity.clamp(0.0, 1.0) / parent_opacity.max(1e-9))
        .clamp(0.0, 1.0)
        * 255.0)
        .round() as u8;
    if opa == 0 {
        return;
    }
    let mode = blend_operation_mode(nodes[group].accumulated.blend_mode);
    operate_rect(
        target,
        origin.0 - target_origin.0,
        origin.1 - target_origin.1,
        BlitSource {
            bitmap: &offscreen,
            rect: (0, 0, right - left, bottom - top),
        },
        BlitOptions {
            mode,
            opa,
            treat_as_opaque: false,
            hold_alpha: false,
        },
    );
}

/// Apply a mask layer's alpha to a composed group: `item_flags` selects the
/// operation, a threshold mask mode cuts at 64, and a composite group also
/// clears the alpha the mask does not cover.
fn apply_alpha_mask(
    dst: &mut BitmapState,
    mask: &BitmapState,
    item_flags: i32,
    mask_mode: i32,
    requested: (i32, i32, i32, i32),
) {
    let (dx0, dy0, dx1, dy1) = requested;
    let Some((x0, y0, x1, y1)) = intersect_rect(
        (dx0, dy0, dx1, dy1),
        (0, 0, dst.width as i32, dst.height as i32),
    ) else {
        return;
    };
    let Some((ox0, oy0, ox1, oy1)) = intersect_rect(
        (x0, y0, x1, y1),
        (0, 0, mask.width as i32, mask.height as i32),
    ) else {
        return;
    };
    if (item_flags & 3) == 1 {
        // The mask defines the only region the group keeps; the bands of the
        // requested rectangle the mask does not reach are cleared.
        fill_mask(dst, (dx0, dy0, dx1, oy0), 0);
        fill_mask(dst, (dx0, oy1.max(dy0), dx1, dy1), 0);
        fill_mask(dst, (dx0, oy0, ox0, oy1), 0);
        fill_mask(dst, (ox1, oy0, dx1, oy1), 0);
    }
    let threshold_mask = mask_mode == 0;
    let width = dst.width as usize;
    for y in oy0..oy1 {
        for x in ox0..ox1 {
            let src_index = ((y as usize * mask.width as usize) + x as usize) * 4 + 3;
            let Some(src_alpha) = mask.rgba.get(src_index).copied() else {
                continue;
            };
            let dst_index = ((y as usize * width) + x as usize) * 4 + 3;
            let Some(target_alpha) = dst.rgba.get_mut(dst_index) else {
                continue;
            };
            let src_alpha = i32::from(src_alpha);
            let dst_alpha = i32::from(*target_alpha);
            let updated = match item_flags {
                1 => {
                    if threshold_mask {
                        if src_alpha < 64 { 0 } else { dst_alpha }
                    } else {
                        dst_alpha * src_alpha / 255
                    }
                }
                2 => {
                    if threshold_mask {
                        if src_alpha >= 64 { 0 } else { dst_alpha }
                    } else {
                        (255 - src_alpha) * dst_alpha / 255
                    }
                }
                5 | 6 => {
                    if threshold_mask {
                        if src_alpha >= 64 { 255 } else { dst_alpha }
                    } else {
                        src_alpha + (255 - src_alpha) * dst_alpha / 255
                    }
                }
                _ => dst_alpha,
            };
            *target_alpha = updated.clamp(0, 255) as u8;
        }
    }
    dst.mark_dirty();
}

/// Draw one item, clipped to its paint box intersected with the viewport of
/// the nearest enclosing shape. A clip that trims the item renders it into an
/// offscreen the size of the clip and composites that once.
fn draw_clipped_item(
    out: &mut BitmapState,
    out_origin: (i32, i32),
    nodes: &[MotionNode],
    index: usize,
    frame: &mut DrawFrame<'_>,
    opacity: f64,
    mode: i64,
) {
    let offset = (-out_origin.0, -out_origin.1);
    let node = &nodes[index];
    let Some(viewport) = node_viewport(nodes, node, frame) else {
        draw_node(out, nodes, index, frame, opacity, mode, offset);
        return;
    };
    let Some(paint) = node_paint_box(node, frame) else {
        return;
    };
    let clip = (
        paint.0.max(viewport.0),
        paint.1.max(viewport.1),
        paint.2.min(viewport.2),
        paint.3.min(viewport.3),
    );
    let left = clip.0 as i32;
    let top = clip.1 as i32;
    let right = clip.2 as i32;
    let bottom = clip.3 as i32;
    if right <= left || bottom <= top {
        return;
    }
    if paint.0 >= clip.0 && paint.1 >= clip.1 && paint.2 <= clip.2 && paint.3 <= clip.3 {
        draw_node(out, nodes, index, frame, opacity, mode, offset);
        return;
    }
    let Ok(width) = u32::try_from(right - left) else {
        return;
    };
    let Ok(height) = u32::try_from(bottom - top) else {
        return;
    };
    let opa = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
    if opa == 0 {
        return;
    }
    let mut offscreen = BitmapState::new(
        width,
        height,
        vec![0u8; width as usize * height as usize * 4],
    );
    draw_node(&mut offscreen, nodes, index, frame, 1.0, 2, (-left, -top));
    operate_rect(
        out,
        left - out_origin.0,
        top - out_origin.1,
        BlitSource {
            bitmap: &offscreen,
            rect: (0, 0, right - left, bottom - top),
        },
        BlitOptions {
            mode,
            opa,
            treat_as_opaque: false,
            hold_alpha: false,
        },
    );
}

/// The world-space top-left corner of the icon quad a node draws.
fn icon_corner(node: &MotionNode, icon: &IconInfo) -> (f64, f64) {
    let matrix = node.matrix;
    let origin_x = icon.origin_x + node.accumulated.ox;
    let origin_y = icon.origin_y + node.accumulated.oy;
    (
        node.accumulated.x - (matrix[1] * origin_y + origin_x * matrix[0]),
        node.accumulated.y - (origin_y * matrix[3] + origin_x * matrix[2]),
    )
}

/// The device-space box the node's own source is drawn into.
///
/// The drawn box uses the same corner as the quad painter, so a mask samples
/// the pixels the mask layer actually paints.
fn node_drawn_box(node: &MotionNode, frame: &DrawFrame<'_>) -> Option<(f64, f64, f64, f64)> {
    let matrix = node.matrix;
    let corners: [(f64, f64); 4] = if node.shape.is_positive() {
        let corner = |local_x: f64, local_y: f64| {
            (
                node.accumulated.x + matrix[0] * local_x + matrix[1] * local_y,
                node.accumulated.y + matrix[2] * local_x + matrix[3] * local_y,
            )
        };
        [
            corner(-32.0, -32.0),
            corner(32.0, -32.0),
            corner(32.0, 32.0),
            corner(-32.0, 32.0),
        ]
    } else {
        let icon = frame.snapshot?.icons.get(&node.local.src)?;
        let (x, y) = icon_corner(node, icon);
        let (width, height) = (icon.width, icon.height);
        let corner = |local_x: f64, local_y: f64| {
            (
                x + matrix[0] * local_x + matrix[1] * local_y,
                y + matrix[2] * local_x + matrix[3] * local_y,
            )
        };
        [
            corner(0.0, 0.0),
            corner(width, 0.0),
            corner(width, height),
            corner(0.0, height),
        ]
    };
    let mut out = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for (x, y) in corners {
        let (x, y) = draw_point(frame.settings.affine, x, y);
        out.0 = out.0.min(x);
        out.1 = out.1.min(y);
        out.2 = out.2.max(x);
        out.3 = out.3.max(y);
    }
    Some((out.0.floor(), out.1.floor(), out.2.ceil(), out.3.ceil()))
}

/// Draw one node: the icon texture it names, mapped onto the quad its
/// accumulated transform describes.
///
/// `offset` is the destination-local origin (the negated clip origin) added to
/// every point, so the same painter fills a full target and an offscreen clip.
/// A shape node draws the placeholder quad instead, and a node whose icon has
/// no readable texture draws nothing and is reported through `missing`.
fn draw_node(
    bitmap: &mut BitmapState,
    nodes: &[MotionNode],
    index: usize,
    frame: &mut DrawFrame<'_>,
    opacity: f64,
    mode: i64,
    offset: (i32, i32),
) {
    let node = &nodes[index];
    let icon = frame
        .snapshot
        .and_then(|snapshot| snapshot.icons.get(&node.local.src));
    let (Some(snapshot), Some(icon), Some(storage)) = (frame.snapshot, icon, frame.storage) else {
        if node.shape > 0 {
            fill_placeholder_quad_offset(
                bitmap,
                node,
                compose_tint(node.accumulated.colors, frame.settings.color)[0],
                frame.settings.affine,
                opacity,
                offset,
            );
        } else if node.local.src.starts_with("src/") {
            // Only a `src/<group>/<name>` source names an icon; the layout and
            // blank sources of container and mask nodes draw nothing.
            frame.missing.push(node.local.src.clone());
        }
        return;
    };
    let Some((pixels, crop)) = icon_pixels(snapshot, storage, icon) else {
        frame.missing.push(node.local.src.clone());
        return;
    };
    let width = crop.2 - crop.0;
    let height = crop.3 - crop.1;
    if width <= 0 || height <= 0 {
        return;
    }
    let Some(source) = tinted_source(
        frame,
        node.accumulated.colors,
        node.accumulated.blend_mode,
        &pixels,
        crop,
    ) else {
        return;
    };
    let matrix = node.matrix;
    // The icon origin is offset in world space: the matrix applied to the
    // origin is subtracted from the node position, then the quad is laid out
    // from that corner.
    let (corner_x, corner_y) = icon_corner(node, icon);
    let corner = |local_x: f64, local_y: f64| {
        let x = corner_x + matrix[0] * local_x + matrix[1] * local_y;
        let y = corner_y + matrix[2] * local_x + matrix[3] * local_y;
        let (x, y) = draw_point(frame.settings.affine, x, y);
        (x + f64::from(offset.0), y + f64::from(offset.1))
    };
    let opa = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
    // A mesh node draws one affine cell per grid quad, so the texture follows
    // the deformed grid inside its outline as well as on it.
    let [div_x, div_y] = node.mesh_div;
    let cells = match (usize::try_from(div_x), usize::try_from(div_y)) {
        (Ok(div_x), Ok(div_y)) if div_x >= 2 && div_y >= 2 => {
            div_x.checked_mul(div_y).map(|count| (div_x, div_y, count))
        }
        _ => None,
    };
    if let Some((div_x, div_y, cell_count)) = cells
        && node.mesh_points.len() >= cell_count.saturating_mul(2)
        && grid_corners(node).is_some()
    {
        let point = |gx: usize, gy: usize| {
            let index = (gy * div_x + gx) * 2;
            let (x, y) = draw_point(
                frame.settings.affine,
                f64::from(node.mesh_points[index]),
                f64::from(node.mesh_points[index + 1]),
            );
            (x + f64::from(offset.0), y + f64::from(offset.1))
        };
        let source_edge = |index: usize, length: i32, divisions: usize| -> i32 {
            (index as f64 * f64::from(length) / divisions as f64).round() as i32
        };
        for gy in 0..div_y - 1 {
            let top = source_edge(gy, height, div_y - 1);
            let bottom = source_edge(gy + 1, height, div_y - 1).max(top + 1);
            for gx in 0..div_x - 1 {
                let left = source_edge(gx, width, div_x - 1);
                let right = source_edge(gx + 1, width, div_x - 1).max(left + 1);
                affine_blit(
                    bitmap,
                    AffineBlit {
                        corners: [point(gx, gy), point(gx + 1, gy), point(gx, gy + 1)],
                        stretch_type: 0,
                        clear: false,
                        clear_color: [0, 0, 0, 0],
                    },
                    BlitSource {
                        bitmap: &source,
                        rect: (left, top, right.min(width), bottom.min(height)),
                    },
                    BlitOptions {
                        mode,
                        opa,
                        treat_as_opaque: false,
                        hold_alpha: false,
                    },
                );
            }
        }
        return;
    }
    let corners = [
        corner(0.0, 0.0),
        corner(f64::from(width), 0.0),
        corner(0.0, f64::from(height)),
    ];
    draw_icon(bitmap, &source, width, height, corners, opa, mode);
}

/// The node's four vertex colours composed with the player's diffuse colour.
///
/// The default half-gray and an opaque-white set mean "no tint" and leave the
/// source untouched; every other set multiplies the source by the
/// interpolated corner colour.
fn compose_tint(node: [[u8; 4]; 4], diffuse: [u8; 4]) -> [[u8; 4]; 4] {
    let neutral = node == DEFAULT_NODE_COLORS || node.iter().all(|corner| *corner == [0xFF; 4]);
    let mut colors = if neutral { [[0xFF; 4]; 4] } else { node };
    if diffuse != [0xFF; 4] {
        for corner in &mut colors {
            for (channel, scale) in corner.iter_mut().zip(diffuse) {
                *channel = (u32::from(*channel) * u32::from(scale) / 255) as u8;
            }
        }
    }
    colors
}

/// The cropped icon source tinted by the node colours, from the cache or
/// freshly built.
fn tinted_source(
    frame: &mut DrawFrame<'_>,
    node_colors: [[u8; 4]; 4],
    blend_mode: i32,
    pixels: &Arc<Pixels>,
    crop: RectI,
) -> Option<Arc<BitmapState>> {
    // The node's own colour decides whether the per-node half-alpha divisor
    // applies; the default half-gray and opaque white have no node tint.
    let neutral =
        node_colors == DEFAULT_NODE_COLORS || node_colors.iter().all(|corner| *corner == [0xFF; 4]);
    let colors = compose_tint(node_colors, frame.settings.color);
    let half_alpha = !neutral && (blend_mode & 0xF0) == 0x10;
    let key = TintKey {
        colors,
        half_alpha,
        crop,
        pixels: Arc::as_ptr(pixels) as usize,
    };
    if let Some(source) = frame.tinted.get(&key) {
        return Some(source.clone());
    }
    let source = Arc::new(tint_crop(pixels, crop, colors, half_alpha)?);
    frame.tinted.insert(key, source.clone());
    Some(source)
}

/// Crop `pixels` to `crop` and scale each channel by the bilinearly
/// interpolated corner colour.
fn tint_crop(
    pixels: &Pixels,
    crop: RectI,
    colors: [[u8; 4]; 4],
    half_alpha: bool,
) -> Option<BitmapState> {
    let width = u32::try_from(crop.2.checked_sub(crop.0)?).ok()?;
    let height = u32::try_from(crop.3.checked_sub(crop.1)?).ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    let width_usize = usize::try_from(width).ok()?;
    let height_usize = usize::try_from(height).ok()?;
    let row_bytes = width_usize.checked_mul(4)?;
    let source_width = usize::try_from(pixels.width).ok()?;
    let source_left = usize::try_from(crop.0).ok()?;
    let divisor = if half_alpha { 128u32 } else { 255u32 };
    let span_x = i32::try_from(width).ok()?.saturating_sub(1).max(1);
    let span_y = i32::try_from(height).ok()?.saturating_sub(1).max(1);
    let lerp = |from: u8, to: u8, step: i32, span: i32| -> u32 {
        let from = i32::from(from);
        let to = i32::from(to);
        (from + step * (to - from) / span) as u32
    };
    let mut rgba = Vec::with_capacity(row_bytes.checked_mul(height_usize)?);
    for y in crop.1..crop.3 {
        let row = usize::try_from(y).ok()?.checked_mul(source_width)?;
        let start = row.checked_add(source_left)?.checked_mul(4)?;
        let row = pixels.rgba.get(start..start.checked_add(row_bytes)?)?;
        let row_step = y - crop.1;
        let left = [
            lerp(colors[0][0], colors[3][0], row_step, span_y),
            lerp(colors[0][1], colors[3][1], row_step, span_y),
            lerp(colors[0][2], colors[3][2], row_step, span_y),
            lerp(colors[0][3], colors[3][3], row_step, span_y),
        ];
        let right = [
            lerp(colors[1][0], colors[2][0], row_step, span_y),
            lerp(colors[1][1], colors[2][1], row_step, span_y),
            lerp(colors[1][2], colors[2][2], row_step, span_y),
            lerp(colors[1][3], colors[2][3], row_step, span_y),
        ];
        for (index, pixel) in row.chunks_exact(4).enumerate() {
            let column = i32::try_from(index).ok()?;
            rgba.extend_from_slice(&[
                ((u32::from(pixel[0]) * lerp(left[0] as u8, right[0] as u8, column, span_x))
                    / divisor)
                    .min(255) as u8,
                ((u32::from(pixel[1]) * lerp(left[1] as u8, right[1] as u8, column, span_x))
                    / divisor)
                    .min(255) as u8,
                ((u32::from(pixel[2]) * lerp(left[2] as u8, right[2] as u8, column, span_x))
                    / divisor)
                    .min(255) as u8,
                ((u32::from(pixel[3]) * lerp(left[3] as u8, right[3] as u8, column, span_x))
                    / divisor)
                    .min(255) as u8,
            ]);
        }
    }
    Some(BitmapState::new(width, height, rgba))
}

/// Composite a cropped icon onto its quad.
///
/// An axis-aligned, unit-scale, whole-pixel placement maps source texels onto
/// destination pixels one to one, so it takes the direct blit path. Every
/// other placement keeps the inverse-mapped affine resampler.
fn draw_icon(
    bitmap: &mut BitmapState,
    source: &BitmapState,
    width: i32,
    height: i32,
    corners: [(f64, f64); 3],
    opa: u8,
    mode: i64,
) {
    let rect = (0, 0, width, height);
    if let Some((x, y)) = unit_axis_aligned_origin(corners, width, height) {
        // The unit axis-aligned placement blends with the node's operation
        // mode, so the same kernel as the affine pass applies.
        operate_rect(
            bitmap,
            x,
            y,
            BlitSource {
                bitmap: source,
                rect,
            },
            BlitOptions {
                mode,
                opa,
                treat_as_opaque: false,
                hold_alpha: false,
            },
        );
        return;
    }
    affine_blit(
        bitmap,
        AffineBlit {
            corners,
            // The nearest texel of the source is sampled.
            stretch_type: 0,
            clear: false,
            clear_color: [0, 0, 0, 0],
        },
        BlitSource {
            bitmap: source,
            rect,
        },
        BlitOptions {
            mode,
            opa,
            treat_as_opaque: false,
            hold_alpha: false,
        },
    );
}

/// The destination origin of a unit-scale, axis-aligned, whole-pixel
/// placement, or `None` for a placement the affine resampler must handle.
fn unit_axis_aligned_origin(
    corners: [(f64, f64); 3],
    width: i32,
    height: i32,
) -> Option<(i32, i32)> {
    if width <= 0 || height <= 0 {
        return None;
    }
    let [origin, right, down] = corners;
    // The quad spans exactly its own size, runs along the axes and starts on a
    // whole pixel.
    if right.0 - origin.0 != f64::from(width)
        || down.1 - origin.1 != f64::from(height)
        || right.1 != origin.1
        || down.0 != origin.0
        || origin.0.fract() != 0.0
        || origin.1.fract() != 0.0
    {
        return None;
    }
    let (x, y) = (origin.0, origin.1);
    if !(f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&x)
        || !(f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&y)
    {
        return None;
    }
    Some((x as i32, y as i32))
}

/// Map a node point through the player's draw affine.
fn draw_point(affine: [f64; 6], x: f64, y: f64) -> (f64, f64) {
    (
        affine[0] * x + affine[2] * y + affine[4],
        affine[1] * x + affine[3] * y + affine[5],
    )
}

/// Draw the fixed-size solid quad a shape node approximates to.
fn fill_placeholder_quad_offset(
    bitmap: &mut BitmapState,
    node: &MotionNode,
    color: [u8; 4],
    affine: [f64; 6],
    opacity: f64,
    offset: (i32, i32),
) {
    let matrix = node.matrix;
    let corner = |local_x: f64, local_y: f64| {
        let (x, y) = draw_point(
            affine,
            node.accumulated.x + matrix[0] * local_x + matrix[1] * local_y,
            node.accumulated.y + matrix[2] * local_x + matrix[3] * local_y,
        );
        (x + f64::from(offset.0), y + f64::from(offset.1))
    };
    let points = [
        corner(-32.0, -32.0),
        corner(32.0, -32.0),
        corner(32.0, 32.0),
        corner(-32.0, 32.0),
    ];
    let alpha = (f64::from(color[3]) * opacity.clamp(0.0, 1.0)) as u8;
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
            && let Some(atlas) = snapshot.pixels.get_or_decode(
                storage,
                &atlas_path,
                &palette_path,
                atlas_width,
                atlas_height,
                texture.compressed,
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
    let pixels = snapshot.pixels.get_or_decode(
        storage,
        &icon_path,
        &palette_path,
        width,
        height,
        icon.compressed,
    )?;
    Some((pixels, (0, 0, width as i32, height as i32)))
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
/// The stored angle is in degrees. A step id outside `0..=3` is skipped.
fn local_matrix(state: &FrameState, order: &[i32; 4]) -> [f64; 4] {
    let (mut m11, mut m12, mut m21, mut m22) = (1.0f64, 0.0f64, 0.0f64, 1.0f64);
    for &step in order {
        match step {
            0 => {
                // The flip mirrors the node: it negates one axis of the
                // transform built so far.
                if state.flip_x {
                    m11 = -m11;
                    m12 = -m12;
                }
                if state.flip_y {
                    m21 = -m21;
                    m22 = -m22;
                }
            }
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
            _ => {}
        }
    }
    [m11, m12, m21, m22]
}

/// The byte channels a normalized color animator hands the rasterizer.
fn color_bytes(channels: [f32; 4]) -> [u8; 4] {
    channels.map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// The canvas width, height and origin anchor of the icon a source names.
fn icon_geometry(snapshot: Option<&MotionSnapshot>, src: &str) -> Option<(f64, f64, f64, f64)> {
    let icon = snapshot?.icons.get(src)?;
    Some((icon.width, icon.height, icon.origin_x, icon.origin_y))
}

/// Evaluate a 4×4 bezier patch over the first sixteen `(x, y)` control
/// points of `points`.
fn eval_bezier_patch(points: &[f32], u: f32, v: f32) -> Option<(f32, f32)> {
    if points.len() < 32 {
        return None;
    }
    let su = 1.0 - u;
    let sv = 1.0 - v;
    let bu = [su * su * su, 3.0 * su * su * u, 3.0 * su * u * u, u * u * u];
    let bv = [sv * sv * sv, 3.0 * sv * sv * v, 3.0 * sv * v * v, v * v * v];
    let mut x = 0.0f32;
    let mut y = 0.0f32;
    for index in 0..16 {
        let weight = bv[index >> 2] * bu[index & 3];
        x += points[index * 2] * weight;
        y += points[index * 2 + 1] * weight;
    }
    Some((x, y))
}

/// Warp a child's position through its parent's bezier patch.
///
/// The child position is normalized into the parent's clip space, evaluated
/// through the parent's patch, and mapped back; the patch's gradient can also
/// contribute an angle and a scale when the parent's sync flags ask for it.
/// Returns the angle delta in degrees and the scale factor.
fn mesh_deform(
    parent: &MotionNode,
    geometry: Option<(f64, f64, f64, f64)>,
    x: &mut f64,
    y: &mut f64,
    z: &mut f64,
    coordinate_mode: i32,
) -> Option<(f64, f64)> {
    if parent.mesh_type != 1
        || (parent.mesh_flags & 1) == 0
        || !parent.accumulated.active
        || !parent.has_source
        || parent.mesh_points.len() < 32
    {
        return None;
    }
    let (width, height, origin_x, origin_y) = geometry?;
    let width = if width > 0.0 { width } else { 1.0 };
    let height = if height > 0.0 { height } else { 1.0 };
    let total_ox = parent.local.ox + origin_x;
    let total_oy = parent.local.oy + origin_y;
    let secondary = if coordinate_mode != 0 { *z } else { *y };
    let norm_x = ((*x + total_ox) / width) as f32;
    let norm_y = ((secondary + total_oy) / height) as f32;
    let (def_x, def_y) = eval_bezier_patch(&parent.mesh_points, norm_x, norm_y)?;
    *x = f64::from(def_x) * width - total_ox;
    if coordinate_mode != 0 {
        *z = f64::from(def_y) * height - total_oy;
    } else {
        *y = f64::from(def_y) * height - total_oy;
    }
    let mut angle = 0.0;
    if (parent.mesh_flags & 2) != 0 && (parent.inherit_flags & 0x10) != 0 {
        let eps = 0.0001f32;
        let points = &parent.mesh_points;
        let (x1, y1) = eval_bezier_patch(points, norm_x - eps, norm_y).unwrap_or((0.0, 0.0));
        let (x2, y2) = eval_bezier_patch(points, norm_x + eps, norm_y).unwrap_or((0.0, 0.0));
        let (x3, y3) = eval_bezier_patch(points, norm_x, norm_y - eps).unwrap_or((0.0, 0.0));
        let (x4, y4) = eval_bezier_patch(points, norm_x, norm_y + eps).unwrap_or((0.0, 0.0));
        let a1 = f64::from(y3 - y4).atan2(f64::from(x4 - x3));
        let a2 = f64::from(y2 - y1).atan2(f64::from(x2 - x1));
        angle = (a1 + a2) * 0.5 * 360.0 / std::f64::consts::TAU;
    }
    let mut scale = 1.0;
    if (parent.mesh_flags & 4) != 0 && (parent.inherit_flags & 0x60) != 0 {
        let eps = 0.0001f32;
        let points = &parent.mesh_points;
        let (x1, y1) = eval_bezier_patch(points, norm_x - eps, norm_y).unwrap_or((0.0, 0.0));
        let (x2, y2) = eval_bezier_patch(points, norm_x + eps, norm_y).unwrap_or((0.0, 0.0));
        let (x3, y3) = eval_bezier_patch(points, norm_x, norm_y - eps).unwrap_or((0.0, 0.0));
        let (x4, y4) = eval_bezier_patch(points, norm_x, norm_y + eps).unwrap_or((0.0, 0.0));
        let dx1 = f64::from(x2 - x1);
        let dy1 = f64::from(y2 - y1);
        let area1 = (dx1 * f64::from(y4 - y1) - dy1 * f64::from(x4 - x1)).abs() * 0.5;
        let area2 = (dx1 * f64::from(y3 - y1) - dy1 * f64::from(x3 - x1)).abs() * 0.5;
        scale = (area1 + area2 + area2 + area1).sqrt() / 0.0002;
    }
    Some((angle, scale))
}

/// Rebuild one node's mesh grid, corner vertices and shape bounds.
fn refresh_node_geometry(
    nodes: &mut [MotionNode],
    index: usize,
    geometry: Option<(f64, f64, f64, f64)>,
    z_factor: f64,
) {
    let Some((cw, ch, origin_x, origin_y)) = geometry else {
        return;
    };
    if cw <= 0.0 || ch <= 0.0 {
        return;
    }
    let matrix = nodes[index].matrix;
    let (m11, m12, m21, m22) = (matrix[0], matrix[1], matrix[2], matrix[3]);
    let pos_y = nodes[index].accumulated.y + nodes[index].accumulated.z * z_factor;
    let total_ox = nodes[index].accumulated.ox + origin_x;
    let total_oy = nodes[index].accumulated.oy + origin_y;
    let org_x = nodes[index].accumulated.x - (m12 * total_oy + total_ox * m11);
    let org_y = pos_y - (total_oy * m22 + total_ox * m21);
    let mut vertices = [
        org_x,
        org_y,
        org_x + m11 * cw,
        org_y + m21 * cw,
        org_x + m11 * cw + m12 * ch,
        org_y + m21 * cw + m22 * ch,
        org_x + m12 * ch,
        org_y + m22 * ch,
    ];
    let previous = std::mem::take(&mut nodes[index].mesh_points);
    let mut points = Vec::new();
    let mut div = [0, 0];
    let mesh_type = nodes[index].mesh_type;
    if mesh_type == 1 || mesh_type == 2 {
        let mw11 = m11 * cw;
        let mw12 = m12 * ch;
        let mw21 = m21 * cw;
        let mw22 = m22 * ch;
        let det = mw11 * mw22 - mw12 * mw21;
        if det.abs() > 1e-10 {
            nodes[index].mesh_inv = [mw22 / det, -(mw12 / det), -(mw21 / det), mw11 / det];
            nodes[index].mesh_inv_offset = [-(org_x as f32), -(org_y as f32)];
        }
        let mut div_total = nodes[index].mesh_division;
        if div_total > 50 {
            div_total = 50;
        }
        if div_total < 1 {
            div_total = 4;
        }
        let div_x = ((f64::from(div_total) * cw / (cw + ch)) as i32) + 1;
        let div_y = div_total - div_x + 2;
        div = [div_x, div_y];
        let Ok(count) = usize::try_from(div_x.saturating_mul(div_y)) else {
            nodes[index].mesh_points = previous;
            return;
        };
        points.reserve(count * 2);
        for gy in 0..div_y {
            let tv = if div_y > 1 {
                f64::from(gy) / f64::from(div_y - 1)
            } else {
                0.0
            };
            let row_base_x = org_x + (m12 * ch) * tv;
            let row_base_y = org_y + (m22 * ch) * tv;
            let row_end_x = row_base_x + m11 * cw;
            let row_end_y = row_base_y + m21 * cw;
            for gx in 0..div_x {
                let tu = if div_x > 1 {
                    f64::from(gx) / f64::from(div_x - 1)
                } else {
                    0.0
                };
                points.push((row_base_x * (1.0 - tu) + row_end_x * tu) as f32);
                points.push((row_base_y * (1.0 - tu) + row_end_y * tu) as f32);
            }
        }
        // The previous frame's grid is the warp field the new grid is drawn
        // through. The patch is indexed by the node's normalized box
        // coordinates, so a grid point is mapped through the inverse of the
        // box matrix before the patch is evaluated, the same mapping the
        // ancestor cascade uses. The point's pixel coordinates would put the
        // patch parameters far outside `0..=1`, where the Bernstein weights
        // grow past the precision of the sum and the evaluation loses every
        // digit of its result.
        if previous.len() >= 32 && det.abs() > 1e-10 {
            for pair in points.chunks_exact_mut(2) {
                let tx = f64::from(pair[0]) - org_x;
                let ty = f64::from(pair[1]) - org_y;
                let u = ((mw22 * tx - mw12 * ty) / det) as f32;
                let v = ((mw11 * ty - mw21 * tx) / det) as f32;
                if let Some((x, y)) = eval_bezier_patch(&previous, u, v) {
                    pair[0] = x;
                    pair[1] = y;
                }
            }
        }
        // Warp the whole grid through every mesh-enabled ancestor, then move
        // the origin by the same cascade.
        let mut chain = Vec::new();
        let mut ancestor = nodes[index].parent;
        while ancestor > 0 {
            if nodes[ancestor].mesh_points.len() >= 32 {
                chain.push((
                    nodes[ancestor].mesh_points.clone(),
                    nodes[ancestor].mesh_inv,
                    nodes[ancestor].mesh_inv_offset,
                ));
            }
            ancestor = nodes[ancestor].parent;
        }
        let mut cascade_x = org_x as f32;
        let mut cascade_y = org_y as f32;
        for (mesh, inverse, offset) in chain {
            for pair in points.chunks_exact_mut(2) {
                let tx = pair[0] + offset[0];
                let ty = pair[1] + offset[1];
                let ix = (inverse[0] * f64::from(tx) + inverse[1] * f64::from(ty)) as f32;
                let iy = (inverse[2] * f64::from(tx) + inverse[3] * f64::from(ty)) as f32;
                if let Some((x, y)) = eval_bezier_patch(&mesh, ix, iy) {
                    pair[0] = x;
                    pair[1] = y;
                }
            }
            let tx = cascade_x + offset[0];
            let ty = cascade_y + offset[1];
            let ix = (inverse[0] * f64::from(tx) + inverse[1] * f64::from(ty)) as f32;
            let iy = (inverse[2] * f64::from(tx) + inverse[3] * f64::from(ty)) as f32;
            if let Some((x, y)) = eval_bezier_patch(&mesh, ix, iy) {
                cascade_x = x;
                cascade_y = y;
            }
        }
        if cascade_x != org_x as f32 || cascade_y != org_y as f32 {
            let dx = cascade_x - org_x as f32;
            let dy = cascade_y - org_y as f32;
            for pair in points.chunks_exact_mut(2) {
                pair[0] += dx;
                pair[1] += dy;
            }
            vertices[0] = f64::from(cascade_x);
            vertices[1] = f64::from(cascade_y);
            vertices[2] = vertices[0] + m11 * cw;
            vertices[3] = vertices[1] + m21 * cw;
            vertices[4] = vertices[0] + m11 * cw + m12 * ch;
            vertices[5] = vertices[1] + m21 * cw + m22 * ch;
            vertices[6] = vertices[0] + m12 * ch;
            vertices[7] = vertices[1] + m22 * ch;
        }
    }
    nodes[index].mesh_points = points;
    nodes[index].mesh_div = div;
    nodes[index].vertices = vertices;
}

/// Rebuild the axis-aligned bounds of a type-7 shape node and make it the clip
/// of its descendants.
fn refresh_shape_bounds(nodes: &mut [MotionNode], index: usize, z_factor: f64) {
    if nodes[index].node_type != 7 || !nodes[index].accumulated.active {
        return;
    }
    let matrix = nodes[index].matrix;
    let (m11, m12, m21, m22) = (matrix[0], matrix[1], matrix[2], matrix[3]);
    let ox = nodes[index].local.ox;
    let oy = nodes[index].local.oy;
    let oox = ox * m11 + oy * m12;
    let ooy = ox * m21 + oy * m22;
    let px = nodes[index].accumulated.x;
    let py = nodes[index].accumulated.y + nodes[index].accumulated.z * z_factor;
    let mut bounds = [
        px - m11 * 16.0 - m12 * 16.0 - oox,
        py - m21 * 16.0 - m22 * 16.0 - ooy,
        px + m11 * 16.0 + m12 * 16.0 - oox,
        py + m21 * 16.0 + m22 * 16.0 - ooy,
    ];
    if bounds[0] > bounds[2] {
        bounds.swap(0, 2);
    }
    if bounds[1] > bounds[3] {
        bounds.swap(1, 3);
    }
    if let Some(clip) = nodes[index].parent_clip
        && let Some(parent) = nodes.get(clip)
    {
        let parent_bounds = parent.shape_aabb;
        if parent_bounds[2] >= parent_bounds[0] && parent_bounds[3] >= parent_bounds[1] {
            bounds[0] = bounds[0].max(parent_bounds[0]);
            bounds[1] = bounds[1].max(parent_bounds[1]);
            bounds[2] = bounds[2].min(parent_bounds[2]);
            bounds[3] = bounds[3].min(parent_bounds[3]);
        }
    }
    nodes[index].shape_aabb = bounds;
    nodes[index].parent_clip = Some(index);
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

/// Advance the player by `progress`'s optional `dt` argument and return the
/// playback ratio.
fn step_progress(
    engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    argc: std::os::raw::c_int,
    argv: *const Value,
    out_error: *mut *mut std::ffi::c_char,
    objthis: *mut std::ffi::c_void,
) -> Result<f64, std::os::raw::c_int> {
    let Some(res) = resolve(engine) else {
        return Err(report_error(out_error, super::helpers::NO_BINDING));
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<PlayerInst>(instance) }) else {
        return Err(report_error(out_error, "Player: null instance"));
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
    // The receiver of the action and sync callbacks is the script object the
    // call was made on. The VM hands it over as a raw object pointer, which
    // is retained for the duration of the call so the dispatch can address
    // it by its retained id.
    let receiver = res.engine().retain_object_detached(objthis).ok();
    let target = receiver
        .as_ref()
        .map(DetachedValue::raw_id)
        .unwrap_or(tjs::Tjs2ValueId::null());
    inst.progress(dt, &res.ctx(), target);
    Ok(inst.progress_ratio())
}

extern "C" fn player_progress_raw(
    engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    argc: std::os::raw::c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    objthis: *mut std::ffi::c_void,
) -> std::os::raw::c_int {
    // `Player.progress` reports the playback position after the step.
    match step_progress(engine, instance, argc, argv, out_error, objthis) {
        Ok(ratio) => {
            set_real(out, ratio);
            0
        }
        Err(code) => code,
    }
}

extern "C" fn player_emote_progress_raw(
    engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    argc: std::os::raw::c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    objthis: *mut std::ffi::c_void,
) -> std::os::raw::c_int {
    // The emote player discards the result of `progress(dt)`.
    match step_progress(engine, instance, argc, argv, out_error, objthis) {
        Ok(_) => {
            set_void(out);
            0
        }
        Err(code) => code,
    }
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
    let Some(inst) = (unsafe { super::helpers::inst_mut::<PlayerInst>(instance) }) else {
        return report_error(out_error, "Player: null instance");
    };
    set_bool(out, inst.playing_now());
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
    // Every class registers `progress(dt)`; only `Player` returns the playback
    // ratio, the emote classes discard it.
    if emote {
        method!("progress", player_emote_progress_raw);
    } else {
        method!("progress", player_progress_raw);
    }
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
    use super::{
        BitmapState, DEFAULT_NODE_COLORS, DrawSettings, MotionNode, MotionSnapshot, PlayerInst,
        build_node_tree, composite_pose, draw_icon, draw_point, evaluate_frames, node_drawable,
        prime_timelines, unit_axis_aligned_origin,
    };
    use crate::engine_api::Storage;
    use crate::engine_api::layer_ops::{AffineBlit, BlitOptions, BlitSource, affine_blit};
    use crate::natives::plugins::psb_file::container::{PsbDocument, PsbValue};
    use crate::{CompatCtx, CompatServices};
    use engine::Scene;
    use fs::FileSystem;
    use tjs::Tjs2Engine;

    fn dict(entries: Vec<(&str, PsbValue)>) -> PsbValue {
        PsbValue::Dictionary(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    }

    fn num(value: i64) -> PsbValue {
        PsbValue::Int(value)
    }

    fn text(value: &str) -> PsbValue {
        PsbValue::String(value.to_string())
    }

    fn content(x: i64, y: i64) -> PsbValue {
        dict(vec![("coord", PsbValue::List(vec![num(x), num(y)]))])
    }

    fn frame(time: i64, frame_type: i64, content: Option<PsbValue>) -> PsbValue {
        let mut entries = vec![("time", num(time)), ("type", num(frame_type))];
        if let Some(content) = content {
            entries.push(("content", content));
        }
        dict(entries)
    }

    /// Two clips: a 120-frame `intro` that moves its shape to (600,600), and a
    /// one-frame `idle` whose shape rests at (300,300). Each clip's last entry
    /// has no content and is only a terminator.
    fn snapshot() -> MotionSnapshot {
        let intro = dict(vec![
            ("label", text("intro_shape")),
            ("type", num(0)),
            ("shape", num(3)),
            (
                "frameList",
                PsbValue::List(vec![
                    frame(0, 3, Some(content(100, 100))),
                    frame(60, 2, Some(content(600, 600))),
                    frame(120, 0, None),
                ]),
            ),
        ]);
        let idle = dict(vec![
            ("label", text("idle_shape")),
            ("type", num(0)),
            ("shape", num(3)),
            (
                "frameList",
                PsbValue::List(vec![frame(0, 0, Some(content(300, 300)))]),
            ),
        ]);
        let intro_clip = dict(vec![
            ("lastTime", num(120)),
            ("layer", PsbValue::List(vec![intro])),
        ]);
        let idle_clip = dict(vec![
            ("lastTime", num(1)),
            ("layer", PsbValue::List(vec![idle])),
        ]);
        let motions = dict(vec![("intro", intro_clip), ("idle", idle_clip)]);
        let object = dict(vec![("test", dict(vec![("motion", motions)]))]);
        let root = dict(vec![("object", object)]);
        MotionSnapshot::from_psb(
            PsbDocument {
                root,
                resources: Vec::new(),
            },
            "test.psb",
        )
    }

    fn player(snapshot: &MotionSnapshot) -> PlayerInst {
        let mut player = PlayerInst {
            snapshot: Some(std::sync::Arc::new(snapshot.clone())),
            motion_key: "intro".to_string(),
            ..PlayerInst::default()
        };
        prime_timelines(snapshot, &mut player.timelines);
        player.nodes = build_node_tree(snapshot, snapshot.clip("intro"));
        player
    }

    fn context(engine: &Tjs2Engine, services: &mut CompatServices) -> CompatCtx {
        // SAFETY: both values outlive the returned context.
        unsafe {
            CompatCtx::from_raw(
                engine as *const Tjs2Engine as usize,
                services as *mut CompatServices as usize,
            )
        }
    }

    fn composite(player: &mut PlayerInst) -> BitmapState {
        let mut bitmap = BitmapState::new(800, 1080, vec![0u8; 800 * 1080 * 4]);
        let snapshot = player.snapshot.clone();
        let color = color_bytes(player.color.bytes);
        let affine = player.affine;
        let missing = composite_pose(
            &mut bitmap,
            &player.nodes,
            snapshot.as_deref(),
            None,
            DrawSettings {
                color,
                affine,
                preview: player.preview,
                mask_mode: player.mask_mode,
            },
            &mut player.tinted,
        );
        assert!(missing.is_empty(), "synthetic shapes need no texture");
        bitmap
    }

    fn alpha(bitmap: &BitmapState, x: usize, y: usize) -> u8 {
        bitmap.rgba[(y * 800 + x) * 4 + 3]
    }

    /// Start a clip and run the gated first tick, which presents the pose
    /// without advancing any clock
    /// (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:637-642`).
    fn start(player: &mut PlayerInst, label: &str, ctx: &CompatCtx) {
        player.play(label, 0, ctx).expect("start the clip");
        player.progress(0.0, ctx, tjs::Tjs2ValueId::null());
    }

    #[test]
    fn a_short_clip_after_a_long_one_replays_from_its_first_frame() {
        let snapshot = snapshot();
        let _lock = crate::native_util::test_vm_lock();
        let engine = Tjs2Engine::new().expect("create engine");
        let mut services = CompatServices::new();
        let ctx = context(&engine, &mut services);
        let mut player = player(&snapshot);
        start(&mut player, "intro", &ctx);
        player.progress(2000.0, &ctx, tjs::Tjs2ValueId::null());
        assert!(!player.playing_now(), "the one-shot intro stopped");
        assert!(player.playing_timelines.is_empty());
        start(&mut player, "idle", &ctx);
        assert!(player.playing_now(), "the idle timeline is playing");
        // The idle clip is one frame long; the intro reached its 120-frame
        // end. The evaluation clock is the idle timeline's own zero
        // (`reference/cpp/plugins/motionplayer/PlayerFrameProgress.cpp:869-871`).
        let bitmap = composite(&mut player);
        assert_eq!(alpha(&bitmap, 300, 300), 255, "idle t=0 shape is drawn");
        assert_eq!(alpha(&bitmap, 600, 600), 0, "the intro shape is gone");
    }

    /// A queued activation swallows the tick that follows it: the timeline does
    /// not advance until the gate is released
    /// (`reference/cpp/plugins/motionplayer/PlayerFrameProgress.cpp:819-826`).
    ///
    /// A delta outside the accepted window is dropped, so one huge tick cannot
    /// skip the animation ahead
    /// (`reference/cpp/plugins/motionplayer/PlayerFrameProgress.cpp:1002-1004`).
    #[test]
    fn the_queuing_gate_holds_the_first_tick_and_drops_an_out_of_range_delta() {
        let snapshot = snapshot();
        let _lock = crate::native_util::test_vm_lock();
        let engine = Tjs2Engine::new().expect("create engine");
        let mut services = CompatServices::new();
        let ctx = context(&engine, &mut services);
        let mut player = player(&snapshot);
        player.play("intro", 0, &ctx).expect("start intro");
        assert!(player.queuing, "a fresh play holds the gate");
        player.progress(500.0, &ctx, tjs::Tjs2ValueId::null());
        assert!(!player.queuing, "the layer update releases the gate");
        assert_eq!(player.timelines["intro"].current_time, 0.0);
        assert!((player.nodes[1].local.x - 100.0).abs() < 1e-9);
        player.progress(500.0, &ctx, tjs::Tjs2ValueId::null());
        assert!(
            (player.nodes[1].local.x - 100.0).abs() > 1.0,
            "the second tick advances"
        );
        let advanced = player.timelines["intro"].current_time;
        assert!(advanced > 0.0);
        assert!((player.progress_ratio() - advanced / 120.0).abs() < 1e-12);
        player.progress(1.0e9, &ctx, tjs::Tjs2ValueId::null());
        assert_eq!(player.timelines["intro"].current_time, advanced);
        player.progress(-1000.0, &ctx, tjs::Tjs2ValueId::null());
        assert_eq!(player.timelines["intro"].current_time, advanced);
    }

    #[test]
    fn replaying_a_clip_resets_its_evaluation_time() {
        let snapshot = snapshot();
        let _lock = crate::native_util::test_vm_lock();
        let engine = Tjs2Engine::new().expect("create engine");
        let mut services = CompatServices::new();
        let ctx = context(&engine, &mut services);
        let mut player = player(&snapshot);
        start(&mut player, "intro", &ctx);
        player.progress(50.0 * 1000.0 / 60.0, &ctx, tjs::Tjs2ValueId::null());
        assert!(
            (player.nodes[1].local.x - 100.0).abs() > 100.0,
            "50 frames move the shape"
        );
        player.play("intro", 0, &ctx).expect("replay intro");
        // The replayed clip is gated again, so its first tick only presents it.
        player.progress(0.0, &ctx, tjs::Tjs2ValueId::null());
        // `playTimeline` resets `state.currentTime` before evaluation
        // (`reference/cpp/plugins/motionplayer/PlayerTimeline.cpp:238`,
        // `:396-403`).
        assert!((player.nodes[1].local.x - 100.0).abs() < 1e-9);
        let bitmap = composite(&mut player);
        assert_eq!(alpha(&bitmap, 100, 100), 255);
        assert_eq!(alpha(&bitmap, 600, 600), 0);
    }

    /// A scaled placement samples the nearest texel, not a bilinear blend
    /// (`reference/cpp/plugins/motionplayer/PlayerRenderExecute.cpp:455-456`).
    #[test]
    fn a_scaled_placement_samples_the_nearest_texel() {
        let source = BitmapState::new(
            2,
            2,
            vec![
                10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255,
            ],
        );
        // The 2x2 source is placed on a 4x4 quad at (1, 1), so every source
        // texel covers a 2x2 block of the destination.
        let corners = [(1.0, 1.0), (5.0, 1.0), (1.0, 5.0)];
        let mut bitmap = BitmapState::new(8, 8, vec![0u8; 8 * 8 * 4]);
        draw_icon(&mut bitmap, &source, 2, 2, corners, 255, 2);
        let texel = |x: usize, y: usize| {
            let offset = (y * 8 + x) * 4;
            [
                bitmap.rgba[offset],
                bitmap.rgba[offset + 1],
                bitmap.rgba[offset + 2],
                bitmap.rgba[offset + 3],
            ]
        };
        assert_eq!(texel(1, 1), [10, 20, 30, 255], "the first texel");
        assert_eq!(texel(2, 1), [10, 20, 30, 255], "the first texel is 2 wide");
        assert_eq!(texel(3, 1), [40, 50, 60, 255], "the second texel");
        assert_eq!(texel(1, 3), [70, 80, 90, 255], "the third texel");
        assert_eq!(texel(3, 3), [100, 110, 120, 255], "the fourth texel");
        // The quad covers four by four pixels, so the texel next to it is
        // untouched.
        assert_eq!(texel(5, 1), [0, 0, 0, 0]);
    }

    /// A child motion exists from the player's start, so it draws its first
    /// pose before the keyframe that named it
    /// (`reference/cpp/plugins/motionplayer/PlayerUpdateChildMotion.cpp:171-213`).
    #[test]
    fn a_child_motion_starts_at_its_parent_keyframe() {
        let parent_layer = dict(vec![
            ("label", text("main")),
            ("type", num(3)),
            (
                "frameList",
                PsbValue::List(vec![dict(vec![
                    ("time", num(40)),
                    ("type", num(2)),
                    ("content", dict(vec![("src", text("motion/test/child"))])),
                ])]),
            ),
        ]);
        let child_layer = dict(vec![
            ("label", text("child_shape")),
            ("type", num(0)),
            ("shape", num(3)),
            (
                "frameList",
                PsbValue::List(vec![frame(0, 2, Some(content(10, 20)))]),
            ),
        ]);
        let parent_clip = dict(vec![
            ("lastTime", num(120)),
            ("layer", PsbValue::List(vec![parent_layer])),
        ]);
        let child_clip = dict(vec![
            ("lastTime", num(60)),
            ("layer", PsbValue::List(vec![child_layer])),
        ]);
        let object = dict(vec![(
            "test",
            dict(vec![(
                "motion",
                dict(vec![("parent", parent_clip), ("child", child_clip)]),
            )]),
        )]);
        let snapshot = MotionSnapshot::from_psb(
            PsbDocument {
                root: dict(vec![("object", object)]),
                resources: Vec::new(),
            },
            "child.psb",
        );
        let mut player = PlayerInst {
            snapshot: Some(std::sync::Arc::new(snapshot.clone())),
            motion_key: "parent".to_string(),
            ..PlayerInst::default()
        };
        prime_timelines(&snapshot, &mut player.timelines);
        player.nodes = build_node_tree(&snapshot, snapshot.clip("parent"));
        assert_eq!(player.nodes.len(), 3, "the child clip is expanded");
        // The child exists from the player's start: its clock is zero before
        // the keyframe that named it, so it draws its first pose right away.
        player
            .timelines
            .get_mut("parent")
            .expect("the timeline")
            .current_time = 0.0;
        player.update_layers();
        assert!(
            player.nodes[2].draw_flag,
            "the child draws from the player's start"
        );
        // From the keyframe on it keeps drawing.
        player
            .timelines
            .get_mut("parent")
            .expect("the timeline")
            .current_time = 40.0;
        player.update_layers();
        assert!(
            player.nodes[2].draw_flag,
            "the child draws from its keyframe"
        );
    }

    /// A child motion's clock is bounded by the child clip's own length: the
    /// child stops at its clip's end instead of running past it
    /// (`reference/cpp/plugins/motionplayer/PlayerUpdateChildMotion.cpp:171-213`,
    /// `reference/cpp/plugins/motionplayer/PlayerCore.cpp:578-581`).
    #[test]
    fn a_child_motion_stops_at_the_end_of_its_own_clip() {
        let parent_layer = dict(vec![
            ("label", text("main")),
            ("type", num(3)),
            (
                "frameList",
                PsbValue::List(vec![dict(vec![
                    ("time", num(40)),
                    ("type", num(2)),
                    ("content", dict(vec![("src", text("motion/test/child"))])),
                ])]),
            ),
        ]);
        // The child clip is shorter than its own last keyframe, so a clock
        // that ran past the clip's end would show the later keyframe.
        let child_layer = dict(vec![
            ("label", text("child_layer")),
            ("type", num(0)),
            (
                "frameList",
                PsbValue::List(vec![
                    dict(vec![
                        ("time", num(0)),
                        ("type", num(2)),
                        (
                            "content",
                            dict(vec![
                                ("src", text("src/test/child")),
                                ("coord", PsbValue::List(vec![num(10), num(0)])),
                            ]),
                        ),
                    ]),
                    dict(vec![
                        ("time", num(8)),
                        ("type", num(2)),
                        (
                            "content",
                            dict(vec![
                                ("src", text("src/test/child")),
                                ("coord", PsbValue::List(vec![num(30), num(0)])),
                            ]),
                        ),
                    ]),
                    dict(vec![("time", num(9)), ("type", num(0))]),
                ]),
            ),
        ]);
        let parent_clip = dict(vec![
            ("lastTime", num(120)),
            ("layer", PsbValue::List(vec![parent_layer])),
        ]);
        let child_clip = dict(vec![
            ("lastTime", num(4)),
            ("layer", PsbValue::List(vec![child_layer])),
        ]);
        let object = dict(vec![(
            "test",
            dict(vec![(
                "motion",
                dict(vec![("parent", parent_clip), ("child", child_clip)]),
            )]),
        )]);
        let snapshot = MotionSnapshot::from_psb(
            PsbDocument {
                root: dict(vec![("object", object)]),
                resources: Vec::new(),
            },
            "child.psb",
        );
        let mut player = PlayerInst {
            snapshot: Some(std::sync::Arc::new(snapshot.clone())),
            motion_key: "parent".to_string(),
            ..PlayerInst::default()
        };
        prime_timelines(&snapshot, &mut player.timelines);
        player.nodes = build_node_tree(&snapshot, snapshot.clip("parent"));
        assert_eq!(player.nodes.len(), 3, "the child clip is expanded");
        // The child's clock starts at the keyframe that named the clip.
        player
            .timelines
            .get_mut("parent")
            .expect("the timeline")
            .current_time = 40.0;
        player.update_layers();
        assert_eq!(
            player.nodes[2].local.x, 10.0,
            "the child starts at its keyframe"
        );
        // Past the child clip's end the clock stops there, so the child keeps
        // the pose its own length allows.
        player
            .timelines
            .get_mut("parent")
            .expect("the timeline")
            .current_time = 100.0;
        player.update_layers();
        assert_eq!(
            player.nodes[2].local.x, 10.0,
            "the child stops at the end of its own clip"
        );
    }

    /// A child clip that loops restarts at its loop time instead of stopping
    /// at its end (`reference/cpp/plugins/motionplayer/RuntimeSupport.h:445-458`).
    #[test]
    fn a_looping_child_motion_wraps_into_its_own_clip() {
        let parent_layer = dict(vec![
            ("label", text("main")),
            ("type", num(3)),
            (
                "frameList",
                PsbValue::List(vec![dict(vec![
                    ("time", num(40)),
                    ("type", num(2)),
                    ("content", dict(vec![("src", text("motion/test/child"))])),
                ])]),
            ),
        ]);
        let child_layer = dict(vec![
            ("label", text("child_layer")),
            ("type", num(0)),
            (
                "frameList",
                PsbValue::List(vec![
                    dict(vec![
                        ("time", num(0)),
                        ("type", num(2)),
                        (
                            "content",
                            dict(vec![
                                ("src", text("src/test/child")),
                                ("coord", PsbValue::List(vec![num(10), num(0)])),
                            ]),
                        ),
                    ]),
                    dict(vec![
                        ("time", num(8)),
                        ("type", num(2)),
                        (
                            "content",
                            dict(vec![
                                ("src", text("src/test/child")),
                                ("coord", PsbValue::List(vec![num(30), num(0)])),
                            ]),
                        ),
                    ]),
                    dict(vec![("time", num(9)), ("type", num(0))]),
                ]),
            ),
        ]);
        let parent_clip = dict(vec![
            ("lastTime", num(120)),
            ("layer", PsbValue::List(vec![parent_layer])),
        ]);
        let child_clip = dict(vec![
            ("lastTime", num(10)),
            ("loopTime", num(4)),
            ("layer", PsbValue::List(vec![child_layer])),
        ]);
        let object = dict(vec![(
            "test",
            dict(vec![(
                "motion",
                dict(vec![("parent", parent_clip), ("child", child_clip)]),
            )]),
        )]);
        let snapshot = MotionSnapshot::from_psb(
            PsbDocument {
                root: dict(vec![("object", object)]),
                resources: Vec::new(),
            },
            "child.psb",
        );
        let mut player = PlayerInst {
            snapshot: Some(std::sync::Arc::new(snapshot.clone())),
            motion_key: "parent".to_string(),
            ..PlayerInst::default()
        };
        prime_timelines(&snapshot, &mut player.timelines);
        player.nodes = build_node_tree(&snapshot, snapshot.clip("parent"));
        // A parent clock of 100 puts the child 60 frames past its keyframe;
        // the child clip wraps that into its own length and lands on its
        // first keyframe instead of its last one.
        player
            .timelines
            .get_mut("parent")
            .expect("the timeline")
            .current_time = 100.0;
        player.update_layers();
        assert_eq!(
            player.nodes[2].local.x, 10.0,
            "the looping child wraps into its own clip"
        );
    }

    /// The clip a type-3 node names first exists from the player's start: its
    /// clock is zero before the keyframe that named it, so the child draws its
    /// first pose right away, and the clock stays bounded by the child clip's
    /// own length afterwards
    /// (`reference/cpp/plugins/motionplayer/PlayerUpdateChildMotion.cpp:171-213`).
    #[test]
    fn a_child_motion_draws_before_the_keyframe_that_named_it() {
        let parent_layer = dict(vec![
            ("label", text("main")),
            ("type", num(3)),
            (
                "frameList",
                PsbValue::List(vec![dict(vec![
                    ("time", num(40)),
                    ("type", num(2)),
                    ("content", dict(vec![("src", text("motion/test/child"))])),
                ])]),
            ),
        ]);
        // The child clip is shorter than its own last keyframe, so a clock
        // that ran past the clip's end would show the later keyframe.
        let child_layer = dict(vec![
            ("label", text("child_layer")),
            ("type", num(0)),
            (
                "frameList",
                PsbValue::List(vec![
                    dict(vec![
                        ("time", num(0)),
                        ("type", num(2)),
                        (
                            "content",
                            dict(vec![
                                ("src", text("src/test/child")),
                                ("coord", PsbValue::List(vec![num(10), num(0)])),
                            ]),
                        ),
                    ]),
                    dict(vec![
                        ("time", num(8)),
                        ("type", num(2)),
                        (
                            "content",
                            dict(vec![
                                ("src", text("src/test/child")),
                                ("coord", PsbValue::List(vec![num(30), num(0)])),
                            ]),
                        ),
                    ]),
                    dict(vec![("time", num(9)), ("type", num(0))]),
                ]),
            ),
        ]);
        let parent_clip = dict(vec![
            ("lastTime", num(120)),
            ("layer", PsbValue::List(vec![parent_layer])),
        ]);
        let child_clip = dict(vec![
            ("lastTime", num(4)),
            ("layer", PsbValue::List(vec![child_layer])),
        ]);
        let object = dict(vec![(
            "test",
            dict(vec![(
                "motion",
                dict(vec![("parent", parent_clip), ("child", child_clip)]),
            )]),
        )]);
        let snapshot = MotionSnapshot::from_psb(
            PsbDocument {
                root: dict(vec![("object", object)]),
                resources: Vec::new(),
            },
            "child.psb",
        );
        let mut player = PlayerInst {
            snapshot: Some(std::sync::Arc::new(snapshot.clone())),
            motion_key: "parent".to_string(),
            ..PlayerInst::default()
        };
        prime_timelines(&snapshot, &mut player.timelines);
        player.nodes = build_node_tree(&snapshot, snapshot.clip("parent"));
        assert_eq!(player.nodes.len(), 3, "the child clip is expanded");
        // The parent's start is before the keyframe that named the child, and
        // the child's clock is zero there.
        player
            .timelines
            .get_mut("parent")
            .expect("the timeline")
            .current_time = 0.0;
        player.update_layers();
        assert!(
            player.nodes[2].draw_flag,
            "the child draws before the keyframe that named it"
        );
        assert_eq!(
            player.nodes[2].local.x, 10.0,
            "the child shows its own first frame"
        );
        // Past the child clip's end the clock stops there, so the child keeps
        // the pose its own length allows.
        player
            .timelines
            .get_mut("parent")
            .expect("the timeline")
            .current_time = 100.0;
        player.update_layers();
        assert_eq!(
            player.nodes[2].local.x, 10.0,
            "the child stops at the end of its own clip"
        );
    }

    /// The action and sync keyframes reach the `onAction` and `onSync`
    /// callbacks when the player progresses
    /// (`reference/cpp/plugins/motionplayer/PlayerFrameProgress.cpp:872-914`).
    #[test]
    fn the_action_and_sync_callbacks_are_dispatched() {
        let snapshot = event_snapshot();
        let _lock = crate::native_util::test_vm_lock();
        let engine = Tjs2Engine::new().expect("create engine");
        let mut services = CompatServices::new();
        let ctx = context(&engine, &mut services);
        engine
            .exec_script(
                "global.events = \"\";\n\
                 class Handler {\n\
                 function Handler() {}\n\
                 function onAction(action, label) { global.events += \"A:\" + action + \":\" + label + \";\"; }\n\
                 function onSync() { global.events += \"S;\"; }\n\
                 }\n\
                 global.handler = new Handler();",
                "motion-player-test",
            )
            .expect("define the handler");
        let handler =
            super::super::helpers::eval_object(&ctx, "global.handler").expect("the handler object");
        let mut player = PlayerInst {
            snapshot: Some(std::sync::Arc::new(snapshot.clone())),
            motion_key: "active".to_string(),
            ..PlayerInst::default()
        };
        prime_timelines(&snapshot, &mut player.timelines);
        player.nodes = build_node_tree(&snapshot, snapshot.clip("active"));
        player
            .play("active", 0, &ctx)
            .expect("start the active clip");
        // The receiver is addressed by its retained id, the same handle the
        // native callback hands the dispatch.
        let receiver = handler.raw_id();
        // The first tick releases the activation gate, the second crosses the
        // keyframes.
        player.progress(0.0, &ctx, receiver);
        player.progress(500.0, &ctx, receiver);
        // The handler recorded the action and the sync.
        // SAFETY: the VM is single-threaded and the handler is alive.
        let events = unsafe { ctx.engine() }
            .eval("global.events", "motion-player-test")
            .expect("the recorded events");
        let events = match events {
            tjs::TjsValue::String(text) => text,
            other => panic!("the events are {other:?}"),
        };
        assert!(
            events.contains("A:wave:active_layer;"),
            "the action callback ran: {events}"
        );
        assert!(events.contains("S;"), "the sync callback ran: {events}");
    }

    /// A dispatch to a receiver that is not a live object is dropped, not
    /// dereferenced: the member lookup fails and the events are consumed
    /// (`reference/cpp/plugins/motionplayer/PlayerFrameProgress.cpp:872-914`).
    #[test]
    fn a_dispatch_to_a_dead_receiver_is_dropped() {
        let snapshot = event_snapshot();
        let _lock = crate::native_util::test_vm_lock();
        let engine = Tjs2Engine::new().expect("create engine");
        let mut services = CompatServices::new();
        let ctx = context(&engine, &mut services);
        let mut player = PlayerInst {
            snapshot: Some(std::sync::Arc::new(snapshot.clone())),
            motion_key: "active".to_string(),
            ..PlayerInst::default()
        };
        prime_timelines(&snapshot, &mut player.timelines);
        player.nodes = build_node_tree(&snapshot, snapshot.clip("active"));
        player
            .play("active", 0, &ctx)
            .expect("start the active clip");
        // A retained id that names no live object: the engine never hands
        // one out this high, so the lookup fails instead of touching memory.
        let dead = tjs::Tjs2ValueId::from_raw(0x1234_usize as *mut std::ffi::c_void);
        player.progress(0.0, &ctx, dead);
        player.progress(500.0, &ctx, dead);
        // The dispatch consumed the events without calling the receiver.
        assert!(
            player.pending_events.is_empty(),
            "the queued events are dropped"
        );
    }

    /// The packed color keeps the red channel in its low byte, and the color
    /// weight is kept in the swapped order
    /// (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:24-27`, `:144-152`,
    /// `:1555-1565`).
    #[test]
    fn a_packed_color_keeps_its_channel_order() {
        let color = super::ColorAnimator::from_packed(0x0000_00FF);
        assert_eq!(color.bytes[0], 1.0, "the low byte is the red channel");
        assert_eq!(color.bytes[1], 0.0, "the green channel");
        assert_eq!(color.bytes[2], 0.0, "the blue channel");
        assert_eq!(color.bytes[3], 0.0, "the alpha channel");
        // The blue channel is the third byte.
        let color = super::ColorAnimator::from_packed(0x00FF_0000);
        assert_eq!(color.bytes[0], 0.0, "the red channel");
        assert_eq!(color.bytes[2], 1.0, "the blue channel");
        // The weight swap exchanges the red and blue channels.
        assert_eq!(
            super::super::animator::swap_packed_rb(0x0011_2233),
            0x0033_2211
        );
        assert_eq!(
            super::super::animator::swap_packed_rb(super::super::animator::swap_packed_rb(
                0x0011_2233
            )),
            0x0011_2233
        );
        // A red tint keeps the red channel and drops the other two.
        let pixels = super::Pixels {
            width: 1,
            height: 1,
            rgba: vec![10, 20, 30, 255],
        };
        let tinted = super::tint_crop(&pixels, (0, 0, 1, 1), [[255, 0, 0, 255]; 4], false)
            .expect("the tint covers the texel");
        assert_eq!(tinted.rgba, vec![10, 0, 0, 255]);
    }

    /// A shape node answers the hit test with its own geometry: a rectangle
    /// of the node's zoom, a circle of the node's zoom or the quad its
    /// transform maps
    /// (`reference/cpp/plugins/motionplayer/PlayerLayerQuery.cpp:217-274`,
    /// `:12-21`, `PlayerUpdateGeometry.cpp:921-979`).
    #[test]
    fn a_shape_node_answers_the_hit_test_with_its_geometry() {
        let mut player = PlayerInst::default();
        let mut node = MotionNode::new(0, "shape".to_string());
        node.node_type = 1;
        node.shape = 2;
        node.draw_flag = true;
        node.accumulated.x = 100.0;
        node.accumulated.y = 50.0;
        node.accumulated.scale_x = 1.0;
        node.accumulated.scale_y = 1.0;
        player.nodes = vec![node];
        // A rectangle of 16 by 16 pixels around the node.
        assert!(player.hit_test("shape", 100.0, 50.0));
        assert!(player.hit_test("shape", 107.0, 50.0));
        assert!(!player.hit_test("shape", 109.0, 50.0));
        assert!(!player.hit_test("shape", 100.0, 60.0));
        // A label that names no node never hits.
        assert!(!player.hit_test("other", 100.0, 50.0));
        // An empty label never hits.
        assert!(!player.hit_test("", 100.0, 50.0));
    }

    /// The `title_bg` logo transition an authored clip carries on its t=40
    /// frame: the curve control points ease the zoom back from 1.1x and fade
    /// the icon in over t=40..75
    /// (`reference/cpp/plugins/motionplayer/PlayerInternal.h:1306-1404`).
    fn logo_curve() -> super::super::model::BezierCurve {
        super::super::model::BezierCurve {
            x: vec![0.0, 0.3, 0.7, 1.0],
            y: vec![0.0, 0.2, 0.4, 1.0],
        }
    }

    /// A snapshot with an `active` clip and an `inactive` clip that both carry
    /// action/sync keyframes. Only the active clip is in the node tree.
    fn event_snapshot() -> MotionSnapshot {
        let eventful = |label: &str, time: i64, action: &str| {
            dict(vec![
                ("label", text(label)),
                ("type", num(0)),
                (
                    "frameList",
                    PsbValue::List(vec![
                        frame(0, 2, Some(content(0, 0))),
                        dict(vec![
                            ("time", num(time)),
                            ("type", num(2)),
                            (
                                "content",
                                dict(vec![
                                    ("coord", PsbValue::List(vec![num(0), num(0)])),
                                    ("action", text(action)),
                                    ("sync", num(1)),
                                ]),
                            ),
                        ]),
                        dict(vec![("time", num(20)), ("type", num(0))]),
                    ]),
                ),
            ])
        };
        let active = dict(vec![
            ("lastTime", num(20)),
            (
                "layer",
                PsbValue::List(vec![eventful("active_layer", 10, "wave")]),
            ),
        ]);
        let inactive = dict(vec![
            ("lastTime", num(20)),
            (
                "layer",
                PsbValue::List(vec![eventful("inactive_layer", 12, "blink")]),
            ),
        ]);
        let root = dict(vec![(
            "object",
            dict(vec![(
                "test",
                dict(vec![(
                    "motion",
                    dict(vec![("active", active), ("inactive", inactive)]),
                )]),
            )]),
        )]);
        MotionSnapshot::from_psb(
            PsbDocument {
                root,
                resources: Vec::new(),
            },
            "events.psb",
        )
    }

    /// `progress` queues the action and sync keyframes of every clip layer,
    /// including a clip that is not in the active node tree, for the
    /// `onAction`/`onSync` dispatch that follows it
    /// (`reference/cpp/plugins/motionplayer/RuntimeSupport.cpp:1814-1920`,
    /// `PlayerFrameProgress.cpp:872-914`).
    #[test]
    fn action_and_sync_keyframes_reach_the_dispatch_queue() {
        let snapshot = event_snapshot();
        let _lock = crate::native_util::test_vm_lock();
        let engine = Tjs2Engine::new().expect("create engine");
        let mut services = CompatServices::new();
        let ctx = context(&engine, &mut services);
        let mut player = PlayerInst {
            snapshot: Some(std::sync::Arc::new(snapshot.clone())),
            motion_key: "active".to_string(),
            ..PlayerInst::default()
        };
        prime_timelines(&snapshot, &mut player.timelines);
        player.nodes = build_node_tree(&snapshot, snapshot.clip("active"));
        player
            .play("active", 0, &ctx)
            .expect("start the active clip");
        // The first tick releases the activation gate without advancing.
        player.progress(0.0, &ctx, tjs::Tjs2ValueId::null());
        player.progress(500.0, &ctx, tjs::Tjs2ValueId::null());
        assert_eq!(
            player
                .pending_events
                .iter()
                .map(|event| (event.kind, event.param1.as_str(), event.param2.as_str()))
                .collect::<Vec<_>>(),
            vec![
                // The one-shot timeline stopping reports its own sync first.
                (1, "active", ""),
                (0, "wave", "active_layer"),
                (1, "active_layer", ""),
                (0, "blink", "inactive_layer"),
                (1, "inactive_layer", ""),
            ],
            "both clips queue their keyframes"
        );
    }

    fn logo_frames() -> Vec<super::super::model::Frame> {
        use super::super::model::Frame;
        vec![
            Frame {
                time: 0.0,
                frame_type: 0,
                ..Frame::default()
            },
            Frame {
                time: 40.0,
                frame_type: 3,
                has_content: true,
                src: "src/title/logo".to_string(),
                x: -705.0,
                y: -388.0,
                scale_x: 1.1,
                scale_y: 1.1,
                opacity: 0.0,
                ccc: logo_curve(),
                zcc: logo_curve(),
                ..Frame::default()
            },
            Frame {
                time: 75.0,
                frame_type: 3,
                has_content: true,
                src: "src/title/logo".to_string(),
                x: -705.0,
                y: -388.0,
                ..Frame::default()
            },
            Frame {
                time: 120.0,
                frame_type: 0,
                ..Frame::default()
            },
        ]
    }

    fn logo_corner(state: &FrameState) -> (f64, f64) {
        let origin_x = 219.0 + state.ox;
        let origin_y = 92.0 + state.oy;
        draw_point(
            [1.0, 0.0, 0.0, 1.0, 960.0, 540.0],
            state.x - origin_x,
            state.y - origin_y,
        )
    }

    #[test]
    fn the_logo_holds_its_last_content_corner_to_the_end() {
        // 960 + (-705) - 219 = 36, 540 + (-388) - 92 = 60. The trailing
        // contentless frame is not a keyframe
        // (`reference/cpp/plugins/motionplayer/PlayerUpdateLayerEval.cpp:282-311`).
        for time in [75.0, 97.0, 119.9, 120.0, 1_000_000.0] {
            let state = evaluate_frames(&logo_frames(), time, 0);
            assert_eq!(logo_corner(&state), (36.0, 60.0), "t={time}");
            assert_eq!(state.src, "src/title/logo", "t={time}");
        }
        let normal = vec![
            super::super::model::Frame {
                time: 0.0,
                frame_type: 3,
                has_content: true,
                src: "src/title/logo".to_string(),
                x: -705.0,
                y: -388.0,
                ..super::super::model::Frame::default()
            },
            super::super::model::Frame {
                time: 1.0,
                frame_type: 0,
                ..super::super::model::Frame::default()
            },
        ];
        for time in [0.0, 100.0] {
            let state = evaluate_frames(&normal, time, 0);
            assert!(state.active, "t={time} holds the idle content frame");
            assert_eq!(logo_corner(&state), (36.0, 60.0), "t={time}");
            assert_eq!(state.src, "src/title/logo", "t={time}");
        }
    }

    /// Midway through the logo transition the eased ratio, not the linear one,
    /// drives the zoom, and the opacity is the 1/255 quantized value.
    #[test]
    fn the_logo_transition_follows_its_curves() {
        // Cubic bezier over the reference's shared-endpoint segment layout:
        // B(t) = (1-t)^3 P0 + 3(1-t)^2 t P1 + 3(1-t) t^2 P2 + t^3 P3
        // (`reference/cpp/plugins/motionplayer/PlayerInternal.h:701-734`).
        let y = [0.0, 0.2, 0.4, 1.0];
        let t = 0.5;
        let u = 1.0 - t;
        let eased =
            u * u * u * y[0] + 3.0 * u * u * t * y[1] + 3.0 * u * t * t * y[2] + t * t * t * y[3];
        let state = evaluate_frames(&logo_frames(), 40.0 + t * 35.0, 0);
        let expected_scale = 1.1 + (1.0 - 1.1) * eased;
        assert!(
            (state.scale_x - expected_scale).abs() < 1e-9,
            "scale {} != {expected_scale}",
            state.scale_x
        );
        assert!(
            (state.scale_x - 1.05).abs() > 1e-3,
            "the authored curve is not linear"
        );
        // 0 -> 255 in the 0..255 domain at t=0.5 is 127.5, rounded away from
        // zero (`reference/cpp/plugins/motionplayer/PlayerInternal.h:1334-1347`).
        assert!((state.opacity - 128.0 / 255.0).abs() < 1e-12);
    }

    /// A rotated and scaled parent moves its child by the parent matrix, not by
    /// the child's own translation alone
    /// (`reference/cpp/plugins/motionplayer/PlayerUpdateLayerEval.cpp:908-960`).
    #[test]
    fn a_child_inherits_the_parent_matrix() {
        let transformed = dict(vec![
            ("coord", PsbValue::List(vec![num(100), num(100)])),
            ("angle", num(90)),
            ("zx", num(2)),
            ("zy", num(2)),
        ]);
        let child = dict(vec![
            ("label", text("child")),
            ("type", num(0)),
            ("shape", num(3)),
            (
                "frameList",
                PsbValue::List(vec![frame(0, 2, Some(content(10, 0)))]),
            ),
        ]);
        let parent = dict(vec![
            ("label", text("parent")),
            ("type", num(0)),
            ("shape", num(3)),
            (
                "frameList",
                PsbValue::List(vec![frame(0, 2, Some(transformed))]),
            ),
            ("children", PsbValue::List(vec![child])),
        ]);
        let clip = dict(vec![
            ("lastTime", num(1)),
            ("layer", PsbValue::List(vec![parent])),
        ]);
        let root = dict(vec![(
            "object",
            dict(vec![(
                "test",
                dict(vec![("motion", dict(vec![("clip", clip)]))]),
            )]),
        )]);
        let snapshot = MotionSnapshot::from_psb(
            PsbDocument {
                root,
                resources: Vec::new(),
            },
            "matrix.psb",
        );
        let mut player = PlayerInst {
            snapshot: Some(std::sync::Arc::new(snapshot.clone())),
            motion_key: "clip".to_string(),
            ..PlayerInst::default()
        };
        prime_timelines(&snapshot, &mut player.timelines);
        player.nodes = build_node_tree(&snapshot, snapshot.clip("clip"));
        player.update_layers();
        let parent = &player.nodes[1];
        let child = &player.nodes[2];
        // The default order applies angle then zoom, so a 90 degree rotation at
        // 2x maps local x onto +y.
        assert!((parent.matrix[0]).abs() < 1e-9, "{:?}", parent.matrix);
        assert!((parent.matrix[1] + 2.0).abs() < 1e-9, "{:?}", parent.matrix);
        assert!((parent.matrix[2] - 2.0).abs() < 1e-9, "{:?}", parent.matrix);
        assert!((parent.matrix[3]).abs() < 1e-9, "{:?}", parent.matrix);
        // (10,0) under that matrix is (0,20) from the parent origin.
        assert!(
            (child.accumulated.x - 100.0).abs() < 1e-9,
            "{}",
            child.accumulated.x
        );
        assert!(
            (child.accumulated.y - 120.0).abs() < 1e-9,
            "{}",
            child.accumulated.y
        );
        assert_eq!(child.matrix, parent.matrix);
    }

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

    #[test]
    fn the_draw_affine_maps_the_node_canvas_into_the_layer() {
        // The title sets `setDrawAffineTranslateMatrix(1,0,0,1,960,540)` to
        // center the 1920x1080 canvas in the 1920x1440 layer
        // (`reference/cpp/plugins/motionplayer/PlayerDrawDispatch.cpp:11-61`).
        let affine = [1.0, 0.0, 0.0, 1.0, 960.0, 540.0];
        assert_eq!(super::draw_point(affine, -960.0, -540.0), (0.0, 0.0));
        assert_eq!(super::draw_point(affine, 960.0, 540.0), (1920.0, 1080.0));
    }

    #[test]
    fn a_unit_axis_aligned_placement_matches_the_affine_path_exactly() {
        // The reference resumes every leaf with `stNearest`
        // (`reference/cpp/plugins/motionplayer/PlayerRenderExecute.cpp:456-472`,
        // `:934-940`). A whole-pixel unit placement maps one source texel to
        // one destination pixel, so the direct blit and the affine sampler
        // must agree pixel for pixel.
        let source = BitmapState::new(
            2,
            2,
            vec![
                10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255,
            ],
        );
        let corners = [(2.0, 3.0), (4.0, 3.0), (2.0, 5.0)];
        assert_eq!(unit_axis_aligned_origin(corners, 2, 2), Some((2, 3)));
        let mut fast = BitmapState::new(6, 6, vec![0u8; 6 * 6 * 4]);
        let mut affine = BitmapState::new(6, 6, vec![0u8; 6 * 6 * 4]);
        draw_icon(&mut fast, &source, 2, 2, corners, 255, 2);
        affine_blit(
            &mut affine,
            AffineBlit {
                corners,
                stretch_type: 0,
                clear: false,
                clear_color: [0, 0, 0, 0],
            },
            BlitSource {
                bitmap: &source,
                rect: (0, 0, 2, 2),
            },
            BlitOptions {
                mode: 2,
                opa: 255,
                treat_as_opaque: false,
                hold_alpha: false,
            },
        );
        assert_eq!(fast.rgba, affine.rgba);
        let texel = |bitmap: &BitmapState, x: usize, y: usize| {
            let offset = (y * 6 + x) * 4;
            [
                bitmap.rgba[offset],
                bitmap.rgba[offset + 1],
                bitmap.rgba[offset + 2],
                bitmap.rgba[offset + 3],
            ]
        };
        assert_eq!(texel(&fast, 2, 3), [10, 20, 30, 255]);
        assert_eq!(texel(&fast, 3, 4), [100, 110, 120, 255]);
        // A rotation or a fractional origin is not a whole-pixel placement.
        assert_eq!(
            unit_axis_aligned_origin([(2.0, 3.0), (3.0, 4.0), (1.0, 4.0)], 2, 2),
            None
        );
        assert_eq!(
            unit_axis_aligned_origin([(2.5, 3.0), (4.5, 3.0), (2.5, 5.0)], 2, 2),
            None
        );
    }

    /// A label that several nodes share is reported once, and the lookup
    /// answers with the last node that wrote it
    /// (`reference/cpp/plugins/motionplayer/PlayerLayerQuery.cpp:101-116`,
    /// `:159-175`).
    #[test]
    fn a_shared_label_is_reported_once() {
        let mut player = PlayerInst::default();
        let mut first = MotionNode::new(0, "body".to_string());
        first.accumulated.x = 1.0;
        let mut second = MotionNode::new(0, "body".to_string());
        second.accumulated.x = 5.0;
        let other = MotionNode::new(0, "leg".to_string());
        player.nodes = vec![first, second, other];
        // The label map holds one entry per label.
        assert_eq!(
            player.layer_labels(),
            vec!["body".to_string(), "leg".to_string()]
        );
        // The map keeps the last node that wrote the label.
        let node = player.layer_node("body").expect("the label is known");
        assert_eq!(node.accumulated.x, 5.0);
        assert!(player.layer_node("missing").is_none());
    }

    /// The layer getter list holds the player's own nodes but the root,
    /// which has no layer of its own
    /// (`reference/cpp/plugins/motionplayer/PlayerLayerQuery.cpp:177-195`).
    #[test]
    fn the_layer_getter_list_skips_the_root() {
        let mut player = PlayerInst::default();
        let root = MotionNode::new(0, String::new());
        let child = MotionNode::new(0, "body".to_string());
        player.nodes = vec![root, child];
        let nodes: Vec<&MotionNode> = player
            .nodes
            .iter()
            .enumerate()
            .filter(|(index, node)| *index != 0 && node.own)
            .map(|(_, node)| node)
            .collect();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].label, "body");
    }

    /// A storage with one opaque texel and the snapshot that names it.
    fn icon_fixture() -> (Storage, MotionSnapshot) {
        let dir = std::env::temp_dir();
        let storage = Storage::mount_archives(
            FileSystem::host(dir.clone()),
            dir.clone(),
            std::iter::empty::<&str>(),
        )
        .expect("mount storage");
        storage.register_media(
            "psb://cache.psb/source/group/icon/art/pixel".to_string(),
            vec![10, 20, 30, 255],
        );
        let icon = dict(vec![("width", num(1)), ("height", num(1))]);
        let group = dict(vec![("icon", dict(vec![("art", icon)]))]);
        let root = dict(vec![("source", dict(vec![("group", group)]))]);
        let snapshot = MotionSnapshot::from_psb(
            PsbDocument {
                root,
                resources: Vec::new(),
            },
            "cache.psb",
        );
        (storage, snapshot)
    }

    /// A node's blend mode selects the operation the affine blit uses, so a
    /// non-default mode draws differently from source-over
    /// (`reference/cpp/plugins/motionplayer/PlayerRenderInternal.cpp:422-446`).
    #[test]
    fn a_node_blend_mode_selects_the_draw_operation() {
        let (storage, snapshot) = icon_fixture();
        let draw_with = |blend_mode: i32| {
            let mut node = MotionNode::new(0, "art".to_string());
            node.draw_flag = true;
            node.local.src = "src/group/art".to_string();
            node.accumulated.opacity = 1.0;
            node.accumulated.blend_mode = blend_mode;
            let mut player = PlayerInst {
                nodes: vec![MotionNode::new(0, String::new()), node],
                ..PlayerInst::default()
            };
            let color = color_bytes(player.color.bytes);
            let affine = player.affine;
            let mut bitmap = BitmapState::new(4, 4, vec![0u8; 4 * 4 * 4]);
            let missing = composite_pose(
                &mut bitmap,
                &player.nodes,
                Some(&snapshot),
                Some(&storage),
                DrawSettings {
                    color,
                    affine,
                    preview: player.preview,
                    mask_mode: player.mask_mode,
                },
                &mut player.tinted,
            );
            assert!(missing.is_empty());
            bitmap
        };
        // The default blend mode composites source-over, so the texel keeps
        // its colour.
        let over = draw_with(16);
        // The multiplicative mode darkens the texel to black.
        let multiplied = draw_with(3);
        assert_eq!(over.rgba[0], 30, "the source-over texel is the decoded one");
        assert_eq!(over.rgba[3], 255);
        assert_eq!(
            &multiplied.rgba[..4],
            &[0u8, 0, 0, 255],
            "the multiplicative mode darkens the texel"
        );
        assert_ne!(over.rgba, multiplied.rgba);
    }

    /// A node's own four vertex colours tint its icon; the reference applies
    /// them to the source bitmap and skips the default half-gray and an
    /// opaque-white set (`reference/cpp/plugins/motionplayer/SourceCache.cpp:44-55`,
    /// `:82-150`). The `0x10` blend bit divides by 128 instead of 255
    /// (`SourceCache.cpp:719-729`).
    #[test]
    fn a_node_colour_tints_its_own_icon() {
        let (storage, snapshot) = icon_fixture();
        let draw_with = |colors: [[u8; 4]; 4], blend_mode: i32| {
            let mut node = MotionNode::new(0, "art".to_string());
            node.draw_flag = true;
            node.local.src = "src/group/art".to_string();
            node.accumulated.opacity = 1.0;
            node.accumulated.blend_mode = blend_mode;
            node.accumulated.colors = colors;
            let mut player = PlayerInst {
                nodes: vec![MotionNode::new(0, String::new()), node],
                ..PlayerInst::default()
            };
            let color = color_bytes(player.color.bytes);
            let affine = player.affine;
            let mut bitmap = BitmapState::new(4, 4, vec![0u8; 4 * 4 * 4]);
            let missing = composite_pose(
                &mut bitmap,
                &player.nodes,
                Some(&snapshot),
                Some(&storage),
                DrawSettings {
                    color,
                    affine,
                    preview: player.preview,
                    mask_mode: player.mask_mode,
                },
                &mut player.tinted,
            );
            assert!(missing.is_empty());
            [
                bitmap.rgba[0],
                bitmap.rgba[1],
                bitmap.rgba[2],
                bitmap.rgba[3],
            ]
        };
        // The decoded texel is (30, 20, 10, 255): a red node colour keeps
        // only red.
        assert_eq!(draw_with([[255, 0, 0, 255]; 4], 0), [30, 0, 0, 255]);
        // The default half-gray and opaque white mean "no tint".
        assert_eq!(draw_with(DEFAULT_NODE_COLORS, 0), [30, 20, 10, 255]);
        assert_eq!(draw_with(DEFAULT_NODE_COLORS, 16), [30, 20, 10, 255]);
        assert_eq!(draw_with([[255, 255, 255, 255]; 4], 0), [30, 20, 10, 255]);
        // The 0x10 blend bit halves the tint divisor, so alpha 128 reaches 255.
        assert_eq!(draw_with([[255, 255, 255, 128]; 4], 0), [30, 20, 10, 128]);
        assert_eq!(draw_with([[255, 255, 255, 128]; 4], 16), [59, 39, 19, 255]);
    }

    /// A half transparent node keeps the source alpha under a non-default
    /// blend mode; the mode changes the colour channels, not the coverage
    /// (`reference/cpp/plugins/motionplayer/PlayerRenderInternal.cpp:422-446`).
    #[test]
    fn a_half_transparent_node_keeps_its_coverage_under_a_blend_mode() {
        let (storage, snapshot) = icon_fixture();
        let draw_with = |blend_mode: i32, opacity: f64| {
            let mut node = MotionNode::new(0, "art".to_string());
            node.draw_flag = true;
            node.local.src = "src/group/art".to_string();
            node.accumulated.opacity = opacity;
            node.accumulated.blend_mode = blend_mode;
            let mut player = PlayerInst {
                nodes: vec![MotionNode::new(0, String::new()), node],
                ..PlayerInst::default()
            };
            let color = color_bytes(player.color.bytes);
            let affine = player.affine;
            let mut bitmap = BitmapState::new(4, 4, vec![0u8; 4 * 4 * 4]);
            let missing = composite_pose(
                &mut bitmap,
                &player.nodes,
                Some(&snapshot),
                Some(&storage),
                DrawSettings {
                    color,
                    affine,
                    preview: player.preview,
                    mask_mode: player.mask_mode,
                },
                &mut player.tinted,
            );
            assert!(missing.is_empty());
            bitmap
        };
        // The source-over draw keeps the texel's colour and the coverage of
        // the node's opacity.
        let over = draw_with(16, 0.5);
        assert_eq!(&over.rgba[..4], &[30u8, 20, 10, 128][..]);
        // The multiplicative mode darkens the texel to black but leaves the
        // coverage alone.
        let multiplied = draw_with(3, 0.5);
        assert_eq!(&multiplied.rgba[..4], &[0u8, 0, 0, 128][..]);
    }

    /// The clear drops the decoded sources, so the next draw reads the chunk
    /// again (`reference/cpp/plugins/motionplayer/PlayerRender.cpp:57-62`,
    /// `SourceCache.cpp:610-622`).
    #[test]
    fn clearing_the_cache_reads_the_chunk_again() {
        let dir = std::env::temp_dir();
        let storage = Storage::mount_archives(
            FileSystem::host(dir.clone()),
            dir.clone(),
            std::iter::empty::<&str>(),
        )
        .expect("mount storage");
        storage.register_media(
            "psb://cache.psb/source/group/icon/art/pixel".to_string(),
            vec![10, 20, 30, 255],
        );
        let icon = dict(vec![("width", num(2)), ("height", num(2))]);
        let group = dict(vec![("icon", dict(vec![("art", icon)]))]);
        let root = dict(vec![("source", dict(vec![("group", group)]))]);
        let snapshot = MotionSnapshot::from_psb(
            PsbDocument {
                root,
                resources: Vec::new(),
            },
            "cache.psb",
        );
        let mut node = MotionNode::new(0, "art".to_string());
        node.draw_flag = true;
        node.local.src = "src/group/art".to_string();
        node.accumulated.opacity = 1.0;
        let mut player = PlayerInst {
            snapshot: Some(std::sync::Arc::new(snapshot)),
            nodes: vec![MotionNode::new(0, String::new()), node],
            ..PlayerInst::default()
        };
        let mut bitmap = BitmapState::new(4, 4, vec![0u8; 4 * 4 * 4]);
        let draw = |player: &mut PlayerInst, bitmap: &mut BitmapState| {
            let snapshot = player.snapshot.clone();
            let color = color_bytes(player.color.bytes);
            let affine = player.affine;
            composite_pose(
                bitmap,
                &player.nodes,
                snapshot.as_deref(),
                Some(&storage),
                DrawSettings {
                    color,
                    affine,
                    preview: player.preview,
                    mask_mode: player.mask_mode,
                },
                &mut player.tinted,
            )
        };
        draw(&mut player, &mut bitmap);
        let read = storage.read_bytes();
        assert!(read > 0, "the chunk was read");
        // The second draw reuses the decoded chunk.
        draw(&mut player, &mut bitmap);
        assert_eq!(storage.read_bytes(), read, "the chunk stays cached");
        // The clear drops the decoded chunk, so the next draw reads it again.
        player.clear_cache();
        assert!(player.tinted.is_empty(), "the tinted source is dropped");
        draw(&mut player, &mut bitmap);
        assert_eq!(storage.read_bytes(), read * 2, "the chunk is read again");
    }

    #[test]
    fn tinted_sources_are_cached_by_source_and_colour() {
        // The source cache keys an entry by its source and packed colours and
        // reuses it until the cache is cleared
        // (`reference/cpp/plugins/motionplayer/SourceCache.cpp:497-552`,
        // `:644-735`).
        let dir = std::env::temp_dir();
        let storage = Storage::mount_archives(
            FileSystem::host(dir.clone()),
            dir.clone(),
            std::iter::empty::<&str>(),
        )
        .expect("mount storage");
        storage.register_media(
            "psb://cache.psb/source/group/icon/art/pixel".to_string(),
            vec![10, 20, 30, 255],
        );
        let icon = dict(vec![("width", num(2)), ("height", num(2))]);
        let group = dict(vec![("icon", dict(vec![("art", icon)]))]);
        let root = dict(vec![("source", dict(vec![("group", group)]))]);
        let snapshot = MotionSnapshot::from_psb(
            PsbDocument {
                root,
                resources: Vec::new(),
            },
            "cache.psb",
        );
        let mut node = MotionNode::new(0, "art".to_string());
        node.draw_flag = true;
        node.local.src = "src/group/art".to_string();
        node.accumulated.opacity = 1.0;
        let mut player = PlayerInst {
            nodes: vec![MotionNode::new(0, String::new()), node],
            ..PlayerInst::default()
        };
        player.color.bytes = [1.0, 128.0 / 255.0, 64.0 / 255.0, 1.0];
        let color = color_bytes(player.color.bytes);
        let mut bitmap = BitmapState::new(4, 4, vec![0u8; 4 * 4 * 4]);
        let missing = composite_pose(
            &mut bitmap,
            &player.nodes,
            Some(&snapshot),
            Some(&storage),
            DrawSettings {
                color,
                affine: player.affine,
                preview: player.preview,
                mask_mode: player.mask_mode,
            },
            &mut player.tinted,
        );
        assert!(missing.is_empty());
        assert_eq!(&bitmap.rgba[..4], &[30u8, 10, 2, 255][..]);
        assert_eq!(player.tinted.len(), 1);
        let cached = player
            .tinted
            .values()
            .next()
            .expect("the tinted source is cached")
            .clone();
        let missing = composite_pose(
            &mut bitmap,
            &player.nodes,
            Some(&snapshot),
            Some(&storage),
            DrawSettings {
                color,
                affine: player.affine,
                preview: player.preview,
                mask_mode: player.mask_mode,
            },
            &mut player.tinted,
        );
        assert!(missing.is_empty());
        assert_eq!(player.tinted.len(), 1, "the second draw reuses the entry");
        let reused = player
            .tinted
            .values()
            .next()
            .expect("the tinted source is cached");
        assert!(std::sync::Arc::ptr_eq(&cached, reused));
        player.clear_source_cache();
        assert!(player.tinted.is_empty(), "the cache has a clear path");
    }

    #[test]
    fn a_zero_opacity_or_invisible_node_is_not_prepared_for_drawing() {
        // A render item copies the accumulated opacity and is skipped when
        // its draw flag is clear
        // (`reference/cpp/plugins/motionplayer/PlayerRenderItems.cpp:554-560`,
        // `:600`).
        let mut node = MotionNode::new(0, "shape".to_string());
        node.draw_flag = true;
        node.shape = 3;
        node.accumulated.opacity = 0.0;
        assert!(!node_drawable(&node));
        node.accumulated.opacity = 1.0;
        node.draw_flag = false;
        assert!(!node_drawable(&node));
        node.draw_flag = true;
        assert!(node_drawable(&node));
    }

    #[test]
    fn drawing_composites_into_the_existing_target_pixels() {
        // The reference issues `AffineCopy` into the target layer instead of
        // building a frame copy
        // (`reference/cpp/plugins/motionplayer/PlayerRenderExecute.cpp:456-472`).
        let snapshot = snapshot();
        let _lock = crate::native_util::test_vm_lock();
        let engine = Tjs2Engine::new().expect("create engine");
        let mut services = CompatServices::new();
        let ctx = context(&engine, &mut services);
        let mut player = player(&snapshot);
        start(&mut player, "intro", &ctx);
        let mut scene = Scene::default();
        let window = scene.add_window("motion", (800, 1080));
        let layer = scene.add_layer(window, None);
        let bitmap = scene.add_bitmap(800, 1080, vec![0u8; 800 * 1080 * 4]);
        scene.layer_mut(layer).expect("target layer").bitmap = Some(bitmap);
        let allocated = scene.bitmap(bitmap).expect("target bitmap").rgba.as_ptr() as usize;
        player.draw_into_scene(&mut scene, layer, None);
        assert_eq!(
            scene.bitmap(bitmap).expect("target bitmap").rgba.as_ptr() as usize,
            allocated,
            "the draw reuses the target allocation"
        );
        assert_eq!(
            alpha(scene.bitmap(bitmap).expect("target bitmap"), 100, 100),
            255
        );
        assert!(scene.layer(layer).expect("target layer").image_modified);
        player.draw_into_scene(&mut scene, layer, None);
        assert_eq!(
            scene.bitmap(bitmap).expect("target bitmap").rgba.as_ptr() as usize,
            allocated
        );
    }

    /// A play call shows the clip it names, even when the name is no
    /// timeline: the reference selects the active clip from the motion key
    /// before it rebuilds the node state
    /// (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:1174-1233`,
    /// `:646-652`).
    #[test]
    fn a_play_call_shows_the_clip_it_names() {
        let snapshot = snapshot();
        let _lock = crate::native_util::test_vm_lock();
        let engine = Tjs2Engine::new().expect("create engine");
        let mut services = CompatServices::new();
        let ctx = context(&engine, &mut services);
        let mut player = player(&snapshot);
        // The module opens on its first clip.
        assert_eq!(player.nodes[1].label, "intro_shape");
        // The named clip replaces it.
        player.play("idle", 0, &ctx).expect("play the idle clip");
        assert_eq!(player.nodes[1].label, "idle_shape");
    }

    /// Six clips whose first two labels are `back_black` and `back_white`. A
    /// play call with no label must show the clip the player's motion key
    /// names, not the first clip in timeline order
    /// (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:1175-1232`).
    fn six_clip_snapshot() -> MotionSnapshot {
        let names = [
            "back_black",
            "back_white",
            "icon25",
            "icon26",
            "icon27",
            "icon28",
        ];
        let mut motions: Vec<(&str, PsbValue)> = Vec::new();
        for (index, name) in names.iter().enumerate() {
            let x = 100 + 100 * index as i64;
            let layer = dict(vec![
                ("label", text(name)),
                ("type", num(0)),
                ("shape", num(3)),
                (
                    "frameList",
                    PsbValue::List(vec![
                        frame(0, 2, Some(content(x, 100))),
                        dict(vec![("time", num(10)), ("type", num(0))]),
                    ]),
                ),
            ]);
            let clip = dict(vec![
                ("lastTime", num(10)),
                ("layer", PsbValue::List(vec![layer])),
            ]);
            motions.push((*name, clip));
        }
        let object = dict(vec![("chara", dict(vec![("motion", dict(motions))]))]);
        MotionSnapshot::from_psb(
            PsbDocument {
                root: dict(vec![("object", object)]),
                resources: Vec::new(),
            },
            "six.psb",
        )
    }

    fn set_timeline_time(player: &mut PlayerInst, label: &str, time: f64) {
        player
            .timelines
            .get_mut(label)
            .expect("the timeline is primed")
            .current_time = time;
    }

    #[test]
    fn the_motion_key_selects_its_clip_when_play_has_no_label() {
        let snapshot = six_clip_snapshot();
        let _lock = crate::native_util::test_vm_lock();
        let engine = Tjs2Engine::new().expect("create engine");
        let mut services = CompatServices::new();
        let ctx = context(&engine, &mut services);
        let mut player = PlayerInst {
            snapshot: Some(std::sync::Arc::new(snapshot.clone())),
            motion_key: "back_white".to_string(),
            ..PlayerInst::default()
        };
        prime_timelines(&snapshot, &mut player.timelines);
        player.nodes = build_node_tree(&snapshot, None);
        // The module opens on its first clip; the motion key names the second.
        assert_eq!(player.nodes[1].label, "back_black");
        player.play("", 0, &ctx).expect("play the keyed clip");
        player.update_layers();
        assert_eq!(player.nodes[1].label, "back_white");
        let bitmap = composite(&mut player);
        assert_eq!(alpha(&bitmap, 200, 100), 255, "back_white is drawn");
        assert_eq!(alpha(&bitmap, 100, 100), 0, "back_black is not");
        // The bound key follows a named play call.
        player.play("icon27", 0, &ctx).expect("play icon27");
        player.update_layers();
        assert_eq!(player.nodes[1].label, "icon27");
    }

    /// A child motion runs as a separate clip instance whose clock comes from
    /// the keyframe that is active now: a later keyframe that names the same
    /// clip restarts it
    /// (`reference/cpp/plugins/motionplayer/PlayerUpdateChildMotion.cpp:172-181`,
    /// `PlayerUpdateLayerEval.cpp:253`).
    #[test]
    fn a_child_clip_restarts_at_each_keyframe_that_names_it() {
        let motion_frame = |time: i64, offset: i64| {
            dict(vec![
                ("time", num(time)),
                ("type", num(3)),
                (
                    "content",
                    dict(vec![
                        ("mask", num(0x80001)),
                        ("src", text("motion/chara/child")),
                        ("ox", num(4)),
                        ("oy", num(-4)),
                        (
                            "motion",
                            dict(vec![("mask", num(0)), ("timeOffset", num(offset))]),
                        ),
                    ]),
                ),
            ])
        };
        let parent = dict(vec![
            ("label", text("main")),
            ("type", num(3)),
            (
                "frameList",
                PsbValue::List(vec![
                    dict(vec![("time", num(0)), ("type", num(0))]),
                    motion_frame(10, 0),
                    motion_frame(50, 5),
                    dict(vec![("time", num(90)), ("type", num(0))]),
                    dict(vec![("time", num(100)), ("type", num(0))]),
                ]),
            ),
        ]);
        let child = dict(vec![
            ("label", text("child")),
            ("type", num(0)),
            ("shape", num(3)),
            (
                "frameList",
                PsbValue::List(vec![
                    frame(0, 3, Some(content(0, 0))),
                    frame(100, 3, Some(content(100, 0))),
                    dict(vec![("time", num(200)), ("type", num(0))]),
                ]),
            ),
        ]);
        let clip = dict(vec![
            ("lastTime", num(100)),
            ("layer", PsbValue::List(vec![parent])),
        ]);
        let child_clip = dict(vec![
            ("lastTime", num(200)),
            ("layer", PsbValue::List(vec![child])),
        ]);
        let object = dict(vec![(
            "chara",
            dict(vec![(
                "motion",
                dict(vec![("main", clip), ("child", child_clip)]),
            )]),
        )]);
        let snapshot = MotionSnapshot::from_psb(
            PsbDocument {
                root: dict(vec![("object", object)]),
                resources: Vec::new(),
            },
            "child.psb",
        );
        let mut player = PlayerInst {
            snapshot: Some(std::sync::Arc::new(snapshot.clone())),
            motion_key: "main".to_string(),
            ..PlayerInst::default()
        };
        prime_timelines(&snapshot, &mut player.timelines);
        player.nodes = build_node_tree(&snapshot, snapshot.clip("main"));
        let child_index = player
            .nodes
            .iter()
            .position(|node| node.label == "child")
            .expect("the child clip is expanded");
        // Parent clock 30: the t=10 keyframe named the child, so it runs at
        // 30 - 10.
        set_timeline_time(&mut player, "main", 30.0);
        player.update_layers();
        assert_eq!(player.nodes[child_index].time_offset, 10.0);
        assert!((player.nodes[child_index].local.x - 20.0).abs() < 1e-9);
        // The child sits at the motion layer's origin corner ((4, -4) put
        // through the identity matrix).
        assert!((player.nodes[child_index].accumulated.x - 16.0).abs() < 1e-9);
        assert!((player.nodes[child_index].accumulated.y - 4.0).abs() < 1e-9);
        // Parent clock 60: the t=50 keyframe names the same clip again with a
        // time offset of 5, so the child restarts at 60 - 50 + 5.
        set_timeline_time(&mut player, "main", 60.0);
        player.update_layers();
        assert_eq!(player.nodes[child_index].time_offset, 45.0);
        assert!((player.nodes[child_index].local.x - 15.0).abs() < 1e-9);
        assert!(player.nodes[child_index].draw_flag);
        // Past the clip end the child is gone.
        set_timeline_time(&mut player, "main", 100.0);
        player.update_layers();
        assert!(!player.nodes[child_index].draw_flag);
    }

    fn group_clip(children: Vec<PsbValue>, extra: Vec<PsbValue>) -> MotionSnapshot {
        let group = dict(vec![
            ("label", text("group")),
            ("type", num(12)),
            ("stencilType", num(5)),
            (
                "frameList",
                PsbValue::List(vec![
                    dict(vec![
                        ("time", num(0)),
                        ("type", num(2)),
                        (
                            "content",
                            dict(vec![
                                ("opa", num(128)),
                                ("coord", PsbValue::List(vec![num(0), num(0)])),
                            ]),
                        ),
                    ]),
                    dict(vec![("time", num(10)), ("type", num(0))]),
                ]),
            ),
            ("children", PsbValue::List(children)),
        ]);
        let mut layers = vec![group];
        layers.extend(extra);
        let clip = dict(vec![
            ("lastTime", num(10)),
            ("layer", PsbValue::List(layers)),
        ]);
        let object = dict(vec![(
            "chara",
            dict(vec![("motion", dict(vec![("main", clip)]))]),
        )]);
        MotionSnapshot::from_psb(
            PsbDocument {
                root: dict(vec![("object", object)]),
                resources: Vec::new(),
            },
            "group.psb",
        )
    }

    fn shape_layer(label: &str, x: i64, y: i64) -> PsbValue {
        dict(vec![
            ("label", text(label)),
            ("type", num(0)),
            ("shape", num(3)),
            (
                "frameList",
                PsbValue::List(vec![
                    frame(0, 2, Some(content(x, y))),
                    dict(vec![("time", num(10)), ("type", num(0))]),
                ]),
            ),
        ])
    }

    fn group_player(snapshot: &MotionSnapshot) -> PlayerInst {
        let mut player = PlayerInst {
            snapshot: Some(std::sync::Arc::new(snapshot.clone())),
            motion_key: "main".to_string(),
            ..PlayerInst::default()
        };
        prime_timelines(snapshot, &mut player.timelines);
        player.nodes = build_node_tree(snapshot, snapshot.clip("main"));
        player.update_layers();
        player
    }

    /// A composite group's opacity is applied once, when the group is
    /// composited, not again on every child that already inherited it
    /// (`reference/cpp/plugins/motionplayer/PlayerRenderExecute.cpp:816-850`).
    #[test]
    fn a_composite_group_applies_its_opacity_once() {
        let snapshot = group_clip(
            vec![shape_layer("inner", 100, 100)],
            vec![shape_layer("outer", 400, 400)],
        );
        let mut player = group_player(&snapshot);
        let bitmap = composite(&mut player);
        assert_eq!(
            alpha(&bitmap, 100, 100),
            128,
            "the group opacity applies once"
        );
        assert_eq!(alpha(&bitmap, 400, 400), 255, "the sibling keeps its alpha");
    }

    /// A composite group is clipped to its paint box intersected with the
    /// nearest shape's viewport
    /// (`reference/cpp/plugins/motionplayer/PlayerRenderExecute.cpp:597-631`,
    /// `PlayerRenderItems.cpp:877-905`).
    #[test]
    fn a_composite_group_clips_to_its_viewport() {
        let viewport = dict(vec![
            ("label", text("clip")),
            ("type", num(7)),
            (
                "frameList",
                PsbValue::List(vec![
                    frame(0, 2, Some(content(100, 100))),
                    dict(vec![("time", num(10)), ("type", num(0))]),
                ]),
            ),
            (
                "children",
                PsbValue::List(vec![dict(vec![
                    ("label", text("group")),
                    ("type", num(12)),
                    ("stencilType", num(5)),
                    (
                        "frameList",
                        PsbValue::List(vec![
                            dict(vec![
                                ("time", num(0)),
                                ("type", num(2)),
                                (
                                    "content",
                                    dict(vec![
                                        ("opa", num(128)),
                                        ("coord", PsbValue::List(vec![num(0), num(0)])),
                                    ]),
                                ),
                            ]),
                            dict(vec![("time", num(10)), ("type", num(0))]),
                        ]),
                    ),
                    (
                        "children",
                        PsbValue::List(vec![
                            shape_layer("inner", 0, 0),
                            shape_layer("outside", 200, 200),
                        ]),
                    ),
                ])]),
            ),
        ]);
        let clip = dict(vec![
            ("lastTime", num(10)),
            ("layer", PsbValue::List(vec![viewport])),
        ]);
        let object = dict(vec![(
            "chara",
            dict(vec![("motion", dict(vec![("main", clip)]))]),
        )]);
        let snapshot = MotionSnapshot::from_psb(
            PsbDocument {
                root: dict(vec![("object", object)]),
                resources: Vec::new(),
            },
            "group_clip.psb",
        );
        let mut player = group_player(&snapshot);
        let bitmap = composite(&mut player);
        assert_eq!(
            alpha(&bitmap, 100, 100),
            128,
            "the child inside the viewport"
        );
        assert_eq!(alpha(&bitmap, 300, 300), 0, "the child outside is clipped");
    }

    fn mesh_snapshot() -> MotionSnapshot {
        let icon = dict(vec![
            ("width", num(100)),
            ("height", num(100)),
            ("originX", num(16)),
            ("originY", num(16)),
        ]);
        let group = dict(vec![("icon", dict(vec![("art", icon)]))]);
        let mesh = dict(vec![
            ("label", text("mesh")),
            ("type", num(0)),
            ("meshTransform", num(1)),
            ("meshDivision", num(10)),
            ("meshSyncChildMask", num(1)),
            ("inheritMask", num(0x7FC)),
            (
                "frameList",
                PsbValue::List(vec![
                    frame(
                        0,
                        2,
                        Some(dict(vec![
                            ("src", text("src/g/art")),
                            ("coord", PsbValue::List(vec![num(0), num(0)])),
                        ])),
                    ),
                    dict(vec![("time", num(10)), ("type", num(0))]),
                ]),
            ),
            (
                "children",
                PsbValue::List(vec![dict(vec![
                    ("label", text("child")),
                    ("type", num(0)),
                    (
                        "frameList",
                        PsbValue::List(vec![
                            frame(
                                0,
                                2,
                                Some(dict(vec![
                                    ("src", text("src/g/art")),
                                    ("coord", PsbValue::List(vec![num(5), num(7)])),
                                ])),
                            ),
                            dict(vec![("time", num(10)), ("type", num(0))]),
                        ]),
                    ),
                ])]),
            ),
        ]);
        let clip = dict(vec![
            ("lastTime", num(10)),
            ("layer", PsbValue::List(vec![mesh])),
        ]);
        let object = dict(vec![(
            "chara",
            dict(vec![("motion", dict(vec![("main", clip)]))]),
        )]);
        MotionSnapshot::from_psb(
            PsbDocument {
                root: dict(vec![
                    ("source", dict(vec![("g", group)])),
                    ("object", object),
                ]),
                resources: Vec::new(),
            },
            "mesh.psb",
        )
    }

    /// A mesh node builds its grid from the subdivision level, warps it
    /// through the previous frame's patch and reports the deformed grid
    /// corners through `vtx`; a child is warped through the parent's patch
    /// (`reference/cpp/plugins/motionplayer/PlayerUpdateGeometry.cpp:290-577`,
    /// `PlayerUpdateLayersInternal.h:356-441`).
    #[test]
    fn a_mesh_node_builds_its_grid_and_deforms_its_child() {
        let snapshot = mesh_snapshot();
        let mut player = PlayerInst {
            snapshot: Some(std::sync::Arc::new(snapshot.clone())),
            motion_key: "main".to_string(),
            ..PlayerInst::default()
        };
        prime_timelines(&snapshot, &mut player.timelines);
        player.nodes = build_node_tree(&snapshot, snapshot.clip("main"));
        let mesh_index = player
            .nodes
            .iter()
            .position(|node| node.label == "mesh")
            .expect("the mesh layer");
        let child_index = player
            .nodes
            .iter()
            .position(|node| node.label == "child")
            .expect("the child layer");
        // Seed the previous frame's patch with a constant point: the next
        // grid and the child position both warp to it.
        player.nodes[mesh_index].mesh_points = vec![0.5; 32];
        player.update_layers();
        let mesh = &player.nodes[mesh_index];
        assert_eq!(mesh.mesh_div, [6, 6]);
        assert_eq!(mesh.mesh_points.len(), 6 * 6 * 2);
        let vtx = super::grid_corners(mesh).expect("the grid has corners");
        for value in vtx {
            assert!(
                (value - 0.5).abs() < 1e-2,
                "vtx reports the deformed corners: {vtx:?}"
            );
        }
        assert_eq!(player.layer_getter_state(mesh).1, vtx);
        // The child was at (5, 7); the parent patch maps its normalized
        // position to 0.5 of the 100-pixel clip.
        let child = &player.nodes[child_index];
        let expected = 0.5 * 100.0 - 16.0;
        assert!((child.accumulated.x - expected).abs() < 1e-2);
        assert!((child.accumulated.y - expected).abs() < 1e-2);
    }

    /// A stencil group's `stencilCompositeMaskLayerList` layers mask the
    /// composed group before it is composited: a threshold mask cuts at 64 and
    /// an alpha mask blends the alpha arithmetically
    /// (`reference/cpp/plugins/motionplayer/PlayerRenderExecute.cpp:839-845`,
    /// `PlayerRenderInternal.cpp:623-750`).
    #[test]
    fn a_stencil_group_applies_its_mask_layers() {
        let dir = std::env::temp_dir();
        let storage = Storage::mount_archives(
            FileSystem::host(dir.clone()),
            dir.clone(),
            std::iter::empty::<&str>(),
        )
        .expect("mount storage");
        storage.register_media(
            "psb://mask.psb/source/content/icon/art/pixel".to_string(),
            [200, 100, 50, 128].repeat(4),
        );
        storage.register_media(
            "psb://mask.psb/source/mask/icon/art/pixel".to_string(),
            [0, 0, 0, 128].repeat(4),
        );
        let icon = || dict(vec![("width", num(2)), ("height", num(2))]);
        let source = dict(vec![
            ("content", dict(vec![("icon", dict(vec![("art", icon())]))])),
            ("mask", dict(vec![("icon", dict(vec![("art", icon())]))])),
        ]);
        let content_layer = dict(vec![
            ("label", text("content")),
            ("type", num(0)),
            (
                "frameList",
                PsbValue::List(vec![
                    frame(
                        0,
                        2,
                        Some(dict(vec![
                            ("src", text("src/content/art")),
                            ("coord", PsbValue::List(vec![num(100), num(100)])),
                            ("opa", num(255)),
                        ])),
                    ),
                    dict(vec![("time", num(10)), ("type", num(0))]),
                ]),
            ),
        ]);
        let group = dict(vec![
            ("label", text("group")),
            ("type", num(12)),
            ("stencilType", num(5)),
            (
                "stencilCompositeMaskLayerList",
                PsbValue::List(vec![text("mask_layer")]),
            ),
            (
                "frameList",
                PsbValue::List(vec![
                    frame(0, 2, Some(content(0, 0))),
                    dict(vec![("time", num(10)), ("type", num(0))]),
                ]),
            ),
            ("children", PsbValue::List(vec![content_layer])),
        ]);
        let mask = dict(vec![
            ("label", text("mask_layer")),
            ("type", num(0)),
            (
                "frameList",
                PsbValue::List(vec![
                    frame(
                        0,
                        2,
                        Some(dict(vec![
                            ("src", text("src/mask/art")),
                            ("coord", PsbValue::List(vec![num(100), num(100)])),
                            ("opa", num(0)),
                        ])),
                    ),
                    dict(vec![("time", num(10)), ("type", num(0))]),
                ]),
            ),
        ]);
        let clip = dict(vec![
            ("lastTime", num(10)),
            ("layer", PsbValue::List(vec![group, mask])),
        ]);
        let object = dict(vec![(
            "chara",
            dict(vec![("motion", dict(vec![("main", clip)]))]),
        )]);
        let snapshot = MotionSnapshot::from_psb(
            PsbDocument {
                root: dict(vec![("source", source), ("object", object)]),
                resources: Vec::new(),
            },
            "mask.psb",
        );
        let draw = |mask_mode: i32| -> u8 {
            let mut player = PlayerInst {
                snapshot: Some(std::sync::Arc::new(snapshot.clone())),
                motion_key: "main".to_string(),
                mask_mode,
                ..PlayerInst::default()
            };
            prime_timelines(&snapshot, &mut player.timelines);
            player.nodes = build_node_tree(&snapshot, snapshot.clip("main"));
            player.update_layers();
            let mut bitmap = BitmapState::new(800, 1080, vec![0u8; 800 * 1080 * 4]);
            let color = color_bytes(player.color.bytes);
            let missing = composite_pose(
                &mut bitmap,
                &player.nodes,
                player.snapshot.as_deref(),
                Some(&storage),
                DrawSettings {
                    color,
                    affine: player.affine,
                    preview: player.preview,
                    mask_mode,
                },
                &mut player.tinted,
            );
            assert!(missing.is_empty());
            alpha(&bitmap, 100, 100)
        };
        // The content alpha 128 blended through the mask alpha 128 is 191; a
        // threshold stencil forces the covered alpha to 255.
        assert_eq!(draw(1), 191, "the alpha mask blends the group alpha once");
        assert_eq!(draw(0), 255, "the threshold mask forces the covered alpha");
    }
}
