//! Motion data model: PSB schema mapping, node tree and timeline/variable
//! state.
//!
//! Behavioural reference: `reference/cpp/plugins/motionplayer/RuntimeSupport.*`,
//! `PlayerMotionLoad.cpp`, `PlayerTimeline.cpp`, `PlayerVariable.cpp`,
//! `NodeTree.cpp` and `MotionNode.h`. The reference parses the PSB tree into a
//! `MotionSnapshot` and then walks the active clip's `layer` array into a
//! persistent `MotionNode` deque; this module does the same over the compact
//! [`PsbValue`] tree produced by [`super::psb`].
//!
//! Everything here is plain data owned by a [`Player`](super::player::Player)
//! (or a [`ResourceManager`](super::resource::ResourceManager) cache entry);
//! there is no global registry.

use std::collections::{HashMap, HashSet};

use super::animator::Animator;
use super::psb::{Psb, PsbValue};

/// One variable keyframe (`VariableFrameInfo`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VariableFrame {
    /// The frame label.
    pub(crate) label: String,
    /// The value at the frame.
    pub(crate) value: f64,
}

/// A controller binding type (`VariableControllerBinding`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ControllerBinding {
    /// The controller type (`0..=8`).
    pub(crate) kind: i32,
    /// The entry index within its control list.
    pub(crate) index: i32,
    /// The source list name.
    pub(crate) source: String,
    /// The role name.
    pub(crate) role: String,
}

/// One option of a selector control.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SelectorOption {
    /// The option label.
    pub(crate) label: String,
    /// The off value.
    pub(crate) off: f64,
    /// The on value.
    pub(crate) on: f64,
}

/// A selector control binding.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SelectorBinding {
    /// The selector label.
    pub(crate) label: String,
    /// The options.
    pub(crate) options: Vec<SelectorOption>,
}

/// A timeline control keyframe.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TimelineFrame {
    /// The frame time.
    pub(crate) time: f64,
    /// Whether the frame is type zero (no value).
    pub(crate) is_type_zero: bool,
    /// The value.
    pub(crate) value: f32,
    /// The ease weight.
    pub(crate) easing_weight: f32,
}

/// A timeline control track.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TimelineTrack {
    /// The controlled variable label.
    pub(crate) label: String,
    /// Whether the label is instant.
    pub(crate) instant: bool,
    /// The keyframes.
    pub(crate) frames: Vec<TimelineFrame>,
}

/// A timeline control binding.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TimelineBinding {
    /// The timeline label.
    pub(crate) label: String,
    /// The loop begin time.
    pub(crate) loop_begin: f64,
    /// The loop end time.
    pub(crate) loop_end: f64,
    /// The last frame time.
    pub(crate) last_time: f64,
    /// The variable tracks.
    pub(crate) tracks: Vec<TimelineTrack>,
}

/// One frame of a layer's `frameList`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Frame {
    /// The frame time.
    pub(crate) time: f64,
    /// `0` invisible, `2` static, `3` interpolate.
    pub(crate) frame_type: i32,
    /// The icon source reference (`src/<chara>/<icon>`).
    pub(crate) src: String,
    /// The child motion clip label a `motion/<chara>/<clip>` source names.
    pub(crate) child_motion: Option<String>,
    /// The blend mode (`bm`).
    pub(crate) blend_mode: i32,
    /// Offset x (`ox`).
    pub(crate) ox: f64,
    /// Offset y (`oy`).
    pub(crate) oy: f64,
    /// Position x (`coord[0]`).
    pub(crate) x: f64,
    /// Position y (`coord[1]`).
    pub(crate) y: f64,
    /// Position z (`coord[2]`).
    pub(crate) z: f64,
    /// Rotation (`angle`).
    pub(crate) angle: f64,
    /// Slant x (`sx`).
    pub(crate) slant_x: f64,
    /// Slant y (`sy`).
    pub(crate) slant_y: f64,
    /// Scale x (`zx`).
    pub(crate) scale_x: f64,
    /// Scale y (`zy`).
    pub(crate) scale_y: f64,
    /// Opacity (`opa`), `0..=1`.
    pub(crate) opacity: f64,
    /// Mask flag.
    pub(crate) mask: bool,
    /// Action name.
    pub(crate) action: String,
    /// Whether the frame carries a sync marker.
    pub(crate) has_sync: bool,
}

impl Default for Frame {
    fn default() -> Self {
        Self {
            time: 0.0,
            frame_type: 0,
            src: String::new(),
            child_motion: None,
            blend_mode: 16,
            ox: 0.0,
            oy: 0.0,
            x: 0.0,
            y: 0.0,
            z: 0.0,
            angle: 0.0,
            slant_x: 0.0,
            slant_y: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            opacity: 1.0,
            mask: false,
            action: String::new(),
            has_sync: false,
        }
    }
}

/// A snapshot of the state a frame contributes (after interpolation).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FrameState {
    /// Whether the frame is visible.
    pub(crate) active: bool,
    /// Position.
    pub(crate) x: f64,
    /// Position.
    pub(crate) y: f64,
    /// Position.
    pub(crate) z: f64,
    /// Rotation.
    pub(crate) angle: f64,
    /// Slant.
    pub(crate) slant_x: f64,
    /// Slant.
    pub(crate) slant_y: f64,
    /// Scale.
    pub(crate) scale_x: f64,
    /// Scale.
    pub(crate) scale_y: f64,
    /// Opacity `0..=1`.
    pub(crate) opacity: f64,
    /// Blend mode.
    pub(crate) blend_mode: i32,
    /// Source reference.
    pub(crate) src: String,
    /// Action.
    pub(crate) action: String,
    /// Sync marker.
    pub(crate) has_sync: bool,
    /// Mask flag.
    pub(crate) mask: bool,
}

impl Default for FrameState {
    fn default() -> Self {
        Self {
            active: true,
            x: 0.0,
            y: 0.0,
            z: 0.0,
            angle: 0.0,
            slant_x: 0.0,
            slant_y: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            opacity: 1.0,
            blend_mode: 16,
            src: String::new(),
            action: String::new(),
            has_sync: false,
            mask: false,
        }
    }
}

/// A node in the persistent layer tree.
#[derive(Debug, Clone)]
pub(crate) struct MotionNode {
    /// Parent index (`0` is the root).
    pub(crate) parent: usize,
    /// Child indices.
    pub(crate) children: Vec<usize>,
    /// The layer label.
    pub(crate) label: String,
    /// Whether the node belongs to this player rather than an expanded child
    /// motion. Only own nodes answer the layer queries; the renderer draws
    /// both.
    pub(crate) own: bool,
    /// The child motion clip label the layer's frames reference.
    pub(crate) child_motion: Option<String>,
    /// The node type (`Motion::LayerType*`).
    pub(crate) node_type: i32,
    /// The coordinate mode.
    pub(crate) coordinate: i32,
    /// The shape type for shape nodes.
    pub(crate) shape: i32,
    /// The transform order.
    pub(crate) transform_order: [i32; 4],
    /// The `inheritMask`.
    pub(crate) inherit_mask: i32,
    /// The `parameterize` index.
    pub(crate) parameterize: i32,
    /// Mesh division level.
    pub(crate) mesh_division: i32,
    /// Mesh transform mode.
    pub(crate) mesh_type: i32,
    /// Particle trigger.
    pub(crate) particle_trigger: i32,
    /// Particle max count.
    pub(crate) particle_max: i32,
    /// Particle acceleration ratio / camera damping.
    pub(crate) particle_accel: f64,
    /// Particle fly direction.
    pub(crate) particle_fly: i32,
    /// Particle inherits angle.
    pub(crate) particle_inherit_angle: bool,
    /// Particle velocity inheritance.
    pub(crate) particle_inherit_velocity: i32,
    /// Particle delete-outside flag.
    pub(crate) particle_delete_outside: bool,
    /// The keyframes.
    pub(crate) frames: Vec<Frame>,
    /// The evaluated local state.
    pub(crate) local: FrameState,
    /// The accumulated world state (after inheritance).
    pub(crate) accumulated: FrameState,
    /// Whether an ancestor is visible.
    pub(crate) draw_flag: bool,
    /// The number of live particle children (particle emitters).
    pub(crate) particle_count: usize,
}

impl MotionNode {
    fn new(parent: usize, label: String) -> Self {
        Self {
            parent,
            children: Vec::new(),
            label,
            own: true,
            child_motion: None,
            node_type: 0,
            coordinate: 0,
            shape: 0,
            transform_order: [0, 1, 2, 3],
            inherit_mask: 0x1FC,
            parameterize: -1,
            mesh_division: 0,
            mesh_type: 0,
            particle_trigger: 0,
            particle_max: 0,
            particle_accel: 0.0,
            particle_fly: 0,
            particle_inherit_angle: false,
            particle_inherit_velocity: 0,
            particle_delete_outside: false,
            frames: Vec::new(),
            local: FrameState::default(),
            accumulated: FrameState::default(),
            draw_flag: true,
            particle_count: 0,
        }
    }
}

/// The canvas geometry of a `source.<chara>.icon.<name>` entry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct IconInfo {
    /// The icon width in pixels.
    pub(crate) width: f64,
    /// The icon height in pixels.
    pub(crate) height: f64,
    /// The horizontal anchor origin.
    pub(crate) origin_x: f64,
    /// The vertical anchor origin.
    pub(crate) origin_y: f64,
}

/// One motion clip (`MotionClip`).
#[derive(Debug, Clone)]
pub(crate) struct MotionClip {
    /// The clip label.
    pub(crate) label: String,
    /// The loop restart time (`>= 0` means loop).
    pub(crate) loop_time: f64,
    /// The total frames.
    pub(crate) total_frames: f64,
    /// The root layer dictionaries.
    pub(crate) layers: Vec<PsbValue>,
}

/// A parsed motion module (`MotionSnapshot`).
#[derive(Debug, Clone)]
pub(crate) struct MotionSnapshot {
    /// The natural width.
    pub(crate) width: f64,
    /// The natural height.
    pub(crate) height: f64,
    /// Main timeline labels.
    pub(crate) main_timelines: Vec<String>,
    /// Difference timeline labels.
    pub(crate) diff_timelines: Vec<String>,
    /// Variable labels in declaration order.
    pub(crate) variable_labels: Vec<String>,
    /// Whether each timeline loops.
    pub(crate) loop_timelines: HashMap<String, bool>,
    /// The loop restart time per timeline.
    pub(crate) timeline_loop_times: HashMap<String, f64>,
    /// The total frame count per timeline.
    pub(crate) timeline_total_frames: HashMap<String, f64>,
    /// The variable value range.
    pub(crate) variable_ranges: HashMap<String, (f64, f64)>,
    /// The variable keyframes.
    pub(crate) variable_frames: HashMap<String, Vec<VariableFrame>>,
    /// Controller bindings by label.
    pub(crate) controller_bindings: HashMap<String, ControllerBinding>,
    /// Selector controls by label.
    pub(crate) selector_controls: HashMap<String, SelectorBinding>,
    /// Instant variable labels.
    pub(crate) instant_variables: HashSet<String>,
    /// Mirror variable match list.
    pub(crate) mirror_match_list: Vec<String>,
    /// Timeline control bindings by label.
    pub(crate) timeline_controls: HashMap<String, TimelineBinding>,
    /// The motion clips.
    pub(crate) clips: Vec<MotionClip>,
    /// Label → clip index.
    pub(crate) clip_index: HashMap<String, usize>,
    /// `source.<chara>.icon.<name>` geometry keyed by `src/<chara>/<name>`.
    pub(crate) icons: HashMap<String, IconInfo>,
    /// The active root layer list.
    pub(crate) layers: Vec<PsbValue>,
    /// The raw metadata dictionary (for `Player::initPhysics` / `metadata`).
    pub(crate) metadata: PsbValue,
    /// The raw root dictionary.
    pub(crate) root: PsbValue,
}

impl MotionSnapshot {
    /// Parse a snapshot from a PSB document.
    pub(crate) fn from_psb(psb: Psb) -> Self {
        let Psb { root, .. } = psb;
        // `metadata.base` holds only the default character and action
        // (`chara`/`motion`); the variable, controller, selector and timeline
        // lists are direct children of `metadata`
        // (`reference/cpp/plugins/motionplayer/docs/MOTIONPLAYER_PSB_STRUCT.md`
        // §4.1; the e-mote3.0 fixture's `metadata.base` has exactly those two
        // keys).
        let base = navigate(&root, &["metadata", "base"])
            .cloned()
            .unwrap_or_else(|| root.clone());
        let metadata = root.get("metadata").cloned().unwrap_or(PsbValue::Null);
        // The logical canvas size lives in `screenSize`
        // (`reference/cpp/plugins/motionplayer/docs/MOTIONPLAYER_PSB_STRUCT.md`
        // §2); the e-mote3.0 fixture carries `screenSize.width/height` =
        // 800/1080. `metadata.base` is not a size source (it holds only
        // `chara`/`motion`), so `root.width/height` is kept as a last resort.
        let width = number_of(base.get("width"))
            .or_else(|| {
                number_of(
                    root.get("screenSize")
                        .and_then(|screen| screen.get("width")),
                )
            })
            .or_else(|| number_of(root.get("width")))
            .unwrap_or(0.0);
        let height = number_of(base.get("height"))
            .or_else(|| {
                number_of(
                    root.get("screenSize")
                        .and_then(|screen| screen.get("height")),
                )
            })
            .or_else(|| number_of(root.get("height")))
            .unwrap_or(0.0);
        let mut icons = HashMap::new();
        collect_icons(&root, &mut icons);

        let mut snapshot = Self {
            width,
            height,
            main_timelines: Vec::new(),
            diff_timelines: Vec::new(),
            variable_labels: Vec::new(),
            loop_timelines: HashMap::new(),
            timeline_loop_times: HashMap::new(),
            timeline_total_frames: HashMap::new(),
            variable_ranges: HashMap::new(),
            variable_frames: HashMap::new(),
            controller_bindings: HashMap::new(),
            selector_controls: HashMap::new(),
            instant_variables: HashSet::new(),
            mirror_match_list: Vec::new(),
            timeline_controls: HashMap::new(),
            clips: Vec::new(),
            clip_index: HashMap::new(),
            icons,
            layers: Vec::new(),
            metadata: PsbValue::Null,
            root,
        };

        // These lists live on the `metadata` dictionary itself, not on
        // `metadata.base`.
        snapshot.collect_variable_list(&metadata);
        snapshot.collect_instant_variables(&metadata);
        snapshot.collect_controls(&metadata);
        snapshot.collect_selectors(&metadata);
        snapshot.collect_mirror(&metadata);
        snapshot.collect_timeline_controls(&metadata);
        snapshot.collect_clips();
        snapshot.metadata = metadata;
        snapshot
    }

    /// The active clip for `label`, or the first clip when the label is empty.
    pub(crate) fn clip(&self, label: &str) -> Option<&MotionClip> {
        if label.is_empty() {
            return self.clips.first();
        }
        self.clip_index
            .get(label)
            .and_then(|index| self.clips.get(*index))
    }

    fn collect_variable_list(&mut self, metadata: &PsbValue) {
        let Some(list) = list_of(metadata.get("variableList")) else {
            return;
        };
        for item in list {
            let Some(label) = label_of(item) else {
                continue;
            };
            push_unique(&mut self.variable_labels, label.clone());
            let mut frames = Vec::new();
            let mut min = f64::INFINITY;
            let mut max = f64::NEG_INFINITY;
            if let Some(frame_list) = list_of(item.get("frameList")) {
                for (index, frame) in frame_list.iter().enumerate() {
                    let frame_label = label_of(frame).unwrap_or_else(|| index.to_string());
                    let value = number_of(frame.get("f"))
                        .or_else(|| number_of(frame.get("value")))
                        .or_else(|| frame.as_f64())
                        .unwrap_or(0.0);
                    frames.push(VariableFrame {
                        label: frame_label,
                        value,
                    });
                    min = min.min(value);
                    max = max.max(value);
                }
            }
            if !frames.is_empty() {
                self.variable_ranges.insert(label.clone(), (min, max));
                self.variable_frames.insert(label, frames);
            }
        }
    }

    fn collect_instant_variables(&mut self, metadata: &PsbValue) {
        let Some(list) = list_of(metadata.get("instantVariableList")) else {
            return;
        };
        for item in list {
            let label = label_of(item).or_else(|| string_of(Some(item)));
            if let Some(label) = label {
                push_unique(&mut self.variable_labels, label.clone());
                self.instant_variables.insert(label);
            }
        }
    }

    fn collect_controls(&mut self, metadata: &PsbValue) {
        const CONTROLS: &[(&str, i32, &[&str])] = &[
            ("bustControl", 0, &["var_lr", "var_ud"]),
            ("hairControl", 1, &["var_lr", "var_lrm", "var_ud"]),
            ("partsControl", 2, &["var_lr", "var_lrm", "var_ud"]),
            ("loopControl", 3, &["var_loop"]),
            ("eyeControl", 4, &["label"]),
            ("eyebrowControl", 5, &["label"]),
            ("mouthControl", 6, &["label", "talkLabel"]),
            ("transitionControl", 7, &["label"]),
        ];
        for (key, kind, roles) in CONTROLS {
            let Some(list) = list_of(metadata.get(key)) else {
                continue;
            };
            for (index, item) in list.iter().enumerate() {
                if !bool_of(item.get("enabled")).unwrap_or(false) {
                    continue;
                }
                for role in *roles {
                    if let Some(label) = string_of(item.get(role))
                        && !label.is_empty()
                    {
                        push_unique(&mut self.variable_labels, label.clone());
                        self.controller_bindings.insert(
                            label,
                            ControllerBinding {
                                kind: *kind,
                                index: index as i32,
                                source: (*key).to_string(),
                                role: (*role).to_string(),
                            },
                        );
                    }
                }
            }
        }
    }

    fn collect_selectors(&mut self, metadata: &PsbValue) {
        let Some(list) = list_of(metadata.get("selectorControl")) else {
            return;
        };
        for (index, item) in list.iter().enumerate() {
            let Some(label) = label_of(item) else {
                continue;
            };
            if !bool_of(item.get("enabled")).unwrap_or(false) {
                self.controller_bindings.remove(&label);
                continue;
            }
            let mut options = Vec::new();
            if let Some(option_list) = list_of(item.get("optionList")) {
                for option in option_list {
                    let Some(option_label) = label_of(option) else {
                        continue;
                    };
                    options.push(SelectorOption {
                        label: option_label,
                        off: number_of(option.get("offValue")).unwrap_or(0.0),
                        on: number_of(option.get("onValue")).unwrap_or(0.0),
                    });
                }
            }
            self.selector_controls.insert(
                label.clone(),
                SelectorBinding {
                    label: label.clone(),
                    options,
                },
            );
            self.record_binding(&label, 8, index as i32, "selectorControl", "label");
        }
    }

    fn collect_mirror(&mut self, metadata: &PsbValue) {
        let Some(mirror) = metadata.get("mirrorControl") else {
            return;
        };
        let Some(list) = list_of(mirror.get("variableMatchList")) else {
            return;
        };
        for item in list {
            if let Some(label) = string_of(Some(item)) {
                push_unique(&mut self.mirror_match_list, label);
            }
        }
    }

    fn collect_timeline_controls(&mut self, metadata: &PsbValue) {
        let Some(list) = list_of(metadata.get("timelineControl")) else {
            return;
        };
        for item in list {
            let Some(label) = label_of(item) else {
                continue;
            };
            let is_diff = bool_of(item.get("diff")).unwrap_or(false);
            if is_diff {
                push_unique(&mut self.diff_timelines, label.clone());
            } else {
                push_unique(&mut self.main_timelines, label.clone());
            }
            let mut binding = TimelineBinding {
                label: label.clone(),
                loop_begin: number_of(item.get("loopBegin")).unwrap_or(-1.0),
                loop_end: number_of(item.get("loopEnd")).unwrap_or(-1.0),
                last_time: number_of(item.get("lastTime")).unwrap_or(-1.0),
                tracks: Vec::new(),
            };
            if let Some(variable_list) = list_of(item.get("variableList")) {
                for variable in variable_list {
                    let Some(track_label) = label_of(variable) else {
                        continue;
                    };
                    let mut track = TimelineTrack {
                        label: track_label.clone(),
                        instant: self.instant_variables.contains(&track_label),
                        frames: Vec::new(),
                    };
                    if let Some(frame_list) = list_of(variable.get("frameList")) {
                        for frame in frame_list {
                            let time = number_of(frame.get("time")).unwrap_or(0.0);
                            let kind = number_of(frame.get("type")).unwrap_or(0.0) as i32;
                            let is_type_zero = kind == 0;
                            let value = if is_type_zero {
                                0.0
                            } else {
                                number_of(frame.get("content").and_then(|c| c.get("value")))
                                    .unwrap_or(0.0) as f32
                            };
                            let easing_weight = if is_type_zero {
                                1.0
                            } else {
                                timeline_ease_weight(
                                    number_of(frame.get("content").and_then(|c| c.get("easing")))
                                        .unwrap_or(0.0),
                                )
                            };
                            track.frames.push(TimelineFrame {
                                time,
                                is_type_zero,
                                value,
                                easing_weight,
                            });
                        }
                    }
                    if let Some(last) = track.frames.last() {
                        binding.last_time = binding.last_time.max(last.time);
                        binding.tracks.push(track);
                    }
                }
            }
            self.timeline_controls.insert(label, binding);
        }
    }

    fn collect_clips(&mut self) {
        // Clone the root slices so the immutable borrow of `self.root` does not
        // overlap the mutable borrow taken by `add_clip`.
        let objects = self
            .root
            .get("object")
            .and_then(PsbValue::as_dict)
            .map(<[(String, PsbValue)]>::to_vec);
        let mut found = false;
        if let Some(objects) = objects {
            for (owner, object) in &objects {
                if let Some(motions) = object.get("motion").and_then(PsbValue::as_dict) {
                    for (label, motion) in motions {
                        self.add_clip(label, motion);
                        found = true;
                    }
                }
                if let Some(motion) = object.get("motion")
                    && motion.as_dict().is_none()
                {
                    self.add_clip(owner, motion);
                    found = true;
                }
            }
        }
        // Fallback layout: root.motion.<label>.
        if !found {
            let motions = self
                .root
                .get("motion")
                .and_then(PsbValue::as_dict)
                .map(<[(String, PsbValue)]>::to_vec);
            if let Some(motions) = motions {
                for (label, motion) in &motions {
                    self.add_clip(label, motion);
                }
            }
        }
        // Fallback: a single unnamed clip built from the root layers.
        if self.clips.is_empty() {
            let layers = list_of(self.root.get("layer"))
                .map(<[PsbValue]>::to_vec)
                .unwrap_or_default();
            self.layers = layers.clone();
            self.add_clip(
                "default",
                &PsbValue::Dict(vec![("layer".into(), PsbValue::List(layers))]),
            );
        }
        // The first clip's layers are the default active set.
        if self.layers.is_empty()
            && let Some(layers) = self.clips.first().map(|clip| clip.layers.clone())
        {
            self.layers = layers;
        }
    }

    fn add_clip(&mut self, label: &str, motion: &PsbValue) {
        let total_frames = number_of(motion.get("lastTime"))
            .or_else(|| number_of(motion.get("frameCount")))
            .or_else(|| number_of(motion.get("frames")))
            .unwrap_or(0.0);
        let loop_time = number_of(motion.get("loopTime")).unwrap_or(-1.0);
        let looping = if motion.get("loopTime").is_some() {
            loop_time >= 0.0
        } else {
            bool_of(motion.get("loop")).unwrap_or(false)
        };
        let layers = list_of(motion.get("layer"))
            .map(<[PsbValue]>::to_vec)
            .unwrap_or_default();
        let index = self.clips.len();
        self.clips.push(MotionClip {
            label: label.to_string(),
            loop_time,
            total_frames,
            layers: layers.clone(),
        });
        self.clip_index.insert(label.to_string(), index);
        push_unique(&mut self.main_timelines, label.to_string());
        self.loop_timelines.insert(label.to_string(), looping);
        self.timeline_loop_times
            .insert(label.to_string(), loop_time);
        self.timeline_total_frames
            .insert(label.to_string(), total_frames);
        if self.layers.is_empty() && !layers.is_empty() {
            self.layers = layers;
        }
    }

    fn record_binding(&mut self, label: &str, kind: i32, index: i32, source: &str, role: &str) {
        if label.is_empty() {
            return;
        }
        push_unique(&mut self.variable_labels, label.to_string());
        self.controller_bindings.insert(
            label.to_string(),
            ControllerBinding {
                kind,
                index,
                source: source.to_string(),
                role: role.to_string(),
            },
        );
    }
}

/// A running timeline (`TimelineState`).
#[derive(Debug, Clone)]
pub(crate) struct TimelineState {
    /// The play flags.
    pub(crate) flags: i32,
    /// Whether the timeline is playing.
    pub(crate) playing: bool,
    /// Whether the timeline loops.
    pub(crate) looping: bool,
    /// The loop restart time.
    pub(crate) loop_time: f64,
    /// The total frames.
    pub(crate) total_frames: f64,
    /// The current time.
    pub(crate) current_time: f64,
    /// The difference blend ratio.
    pub(crate) blend_ratio: f64,
    /// The blend animator.
    pub(crate) blend: Animator,
    /// Whether the blend stops the timeline when it completes.
    pub(crate) blend_auto_stop: bool,
    /// Per-track animators for the control keyframes.
    pub(crate) track_animators: Vec<Animator>,
}

impl TimelineState {
    /// Create a timeline state (the state is keyed by label in the owning
    /// map, so it does not carry the label itself).
    pub(crate) fn new(total_frames: f64, looping: bool, loop_time: f64) -> Self {
        Self {
            flags: 0,
            playing: false,
            looping,
            loop_time,
            total_frames,
            current_time: 0.0,
            blend_ratio: 1.0,
            blend: Animator::at(1.0),
            blend_auto_stop: false,
            track_animators: Vec::new(),
        }
    }
}

/// Prime the timeline states for a snapshot.
pub(crate) fn prime_timelines(
    snapshot: &MotionSnapshot,
    states: &mut HashMap<String, TimelineState>,
) {
    states.clear();
    for label in snapshot
        .main_timelines
        .iter()
        .chain(snapshot.diff_timelines.iter())
    {
        let total = snapshot
            .timeline_total_frames
            .get(label)
            .copied()
            .unwrap_or(0.0);
        let looping = snapshot.loop_timelines.get(label).copied().unwrap_or(false);
        let loop_time = snapshot
            .timeline_loop_times
            .get(label)
            .copied()
            .unwrap_or(-1.0);
        states.insert(label.clone(), TimelineState::new(total, looping, loop_time));
    }
}

/// Advance a timeline's current time, wrapping on loop.
///
/// Returns `false` when the timeline should stop (`Player::wrapTimelineCurrentTime`).
pub(crate) fn wrap_timeline(current: f64, total: f64, loop_time: f64) -> (f64, bool) {
    if total <= 0.0 || current < total {
        return (current, true);
    }
    if loop_time < 0.0 || loop_time >= total {
        return (total, false);
    }
    let mut time = current;
    let mut guard = 0;
    while time >= total && guard < 1024 {
        time = time + loop_time - total;
        guard += 1;
    }
    if time >= total {
        (total, false)
    } else {
        (time, true)
    }
}

/// Step the active timelines by `dt` frames, returning the events generated.
pub(crate) fn step_timelines(
    snapshot: &MotionSnapshot,
    states: &mut HashMap<String, TimelineState>,
    dt: f64,
) -> Vec<MotionEvent> {
    let mut events = Vec::new();
    let labels: Vec<String> = states
        .iter()
        .filter(|(_, state)| state.playing)
        .map(|(label, _)| label.clone())
        .collect();
    for label in labels {
        let (loop_time, tracks) = {
            let Some(state) = states.get(&label) else {
                continue;
            };
            let tracks = snapshot
                .timeline_controls
                .get(&label)
                .map(|binding| binding.tracks.clone())
                .unwrap_or_default();
            (state.loop_time, tracks)
        };
        let Some(state) = states.get_mut(&label) else {
            continue;
        };
        state.current_time += dt;
        let (wrapped, keep) = wrap_timeline(state.current_time, state.total_frames, loop_time);
        state.current_time = wrapped;
        if !state.looping && !keep {
            state.playing = false;
        }
        step_timeline_controls(state, &tracks, dt, &mut events);
    }
    events
}

fn step_timeline_controls(
    state: &mut TimelineState,
    tracks: &[TimelineTrack],
    dt: f64,
    events: &mut Vec<MotionEvent>,
) {
    if state.track_animators.len() < tracks.len() {
        state
            .track_animators
            .resize_with(tracks.len(), || Animator::at(0.0));
    }
    for (index, track) in tracks.iter().enumerate() {
        if track.instant {
            if let Some(frame) = track
                .frames
                .iter()
                .rev()
                .find(|frame| frame.time <= state.current_time)
            {
                state.track_animators[index].set(frame.value);
            }
            continue;
        }
        let target = track
            .frames
            .iter()
            .rev()
            .find(|frame| frame.time <= state.current_time && !frame.is_type_zero);
        if let Some(frame) = target {
            let animator = &mut state.track_animators[index];
            if !animator.is_active() && (animator.value() - frame.value).abs() > f32::EPSILON {
                animator.transition_to(frame.value, 0.0, frame.easing_weight);
            } else {
                animator.set(frame.value);
            }
            animator.step(dt as f32);
            if let Some(action) = frame_action(track, frame) {
                events.push(MotionEvent {
                    kind: 0,
                    param1: action,
                    param2: track.label.clone(),
                });
            }
        }
    }
}

fn frame_action(_track: &TimelineTrack, _frame: &TimelineFrame) -> Option<String> {
    None
}

/// A pending action/sync event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MotionEvent {
    /// `0` is an action, `1` is a sync.
    pub(crate) kind: i32,
    /// The first parameter.
    pub(crate) param1: String,
    /// The second parameter.
    pub(crate) param2: String,
}

/// Build the node tree for `clip` (or the snapshot's active layers).
pub(crate) fn build_node_tree(
    snapshot: &MotionSnapshot,
    clip: Option<&MotionClip>,
) -> Vec<MotionNode> {
    let mut nodes = vec![MotionNode::new(0, String::new())];
    let layers = clip
        .map(|clip| clip.layers.as_slice())
        .filter(|layers| !layers.is_empty())
        .unwrap_or(snapshot.layers.as_slice());
    let mut stack: Vec<String> = Vec::new();
    if let Some(clip) = clip {
        stack.push(clip.label.clone());
    }
    for layer in layers {
        append_node(&mut nodes, 0, layer, snapshot, &mut stack, false);
    }
    nodes
}

fn append_node(
    nodes: &mut Vec<MotionNode>,
    parent: usize,
    layer: &PsbValue,
    snapshot: &MotionSnapshot,
    stack: &mut Vec<String>,
    child: bool,
) -> usize {
    let index = nodes.len();
    let label = label_of(layer).unwrap_or_default();
    let mut node = MotionNode::new(parent, label);
    node.own = !child;
    node.node_type = number_of(layer.get("type")).unwrap_or(0.0) as i32;
    node.coordinate = number_of(layer.get("coordinate")).unwrap_or(0.0) as i32;
    node.shape = number_of(layer.get("shape")).unwrap_or(0.0) as i32;
    node.inherit_mask = number_of(layer.get("inheritMask")).unwrap_or(508.0) as i32;
    node.parameterize = number_of(layer.get("parameterize")).unwrap_or(-1.0) as i32;
    node.mesh_division = number_of(layer.get("meshDivision")).unwrap_or(0.0) as i32;
    node.mesh_type = number_of(layer.get("meshTransform")).unwrap_or(0.0) as i32;
    node.particle_trigger = number_of(layer.get("particleTrigger")).unwrap_or(0.0) as i32;
    node.particle_max = number_of(layer.get("particleMaxNum")).unwrap_or(0.0) as i32;
    node.particle_accel = number_of(layer.get("particleAccelRatio")).unwrap_or(0.0);
    node.particle_fly = number_of(layer.get("particleFlyDirection")).unwrap_or(0.0) as i32;
    node.particle_inherit_angle = bool_of(layer.get("particleInheritAngle")).unwrap_or(false);
    node.particle_inherit_velocity =
        number_of(layer.get("particleInheritVelocity")).unwrap_or(0.0) as i32;
    node.particle_delete_outside = bool_of(layer.get("particleDeleteOutside")).unwrap_or(false);
    if let Some(order) = layer.get("transformOrder").and_then(PsbValue::as_array) {
        for (slot, value) in node.transform_order.iter_mut().zip(order.iter()) {
            *slot = *value as i32;
        }
    }
    node.frames = parse_frames(layer.get("frameList"));
    node.child_motion = node
        .frames
        .iter()
        .find_map(|frame| frame.child_motion.clone());
    nodes.push(node);
    nodes[parent].children.push(index);
    if let Some(label) = nodes[index].child_motion.clone()
        && !label.is_empty()
        && !stack.iter().any(|item| item == &label)
        && let Some(clip) = snapshot.clip(&label)
    {
        let layers = clip.layers.clone();
        stack.push(label);
        for layer in &layers {
            append_node(nodes, index, layer, snapshot, stack, true);
        }
        stack.pop();
    }
    if let Some(children) = list_of(layer.get("children")) {
        for child_layer in children {
            append_node(nodes, index, child_layer, snapshot, stack, child);
        }
    }
    index
}

fn parse_frames(value: Option<&PsbValue>) -> Vec<Frame> {
    let Some(list) = value.and_then(PsbValue::as_list) else {
        return Vec::new();
    };
    let mut frames = Vec::with_capacity(list.len());
    for item in list {
        let mut frame = Frame {
            time: number_of(item.get("time")).unwrap_or(0.0),
            frame_type: number_of(item.get("type")).unwrap_or(0.0) as i32,
            ..Frame::default()
        };
        if let Some(content) = item.get("content") {
            let raw_src = string_of(content.get("src"))
                .or_else(|| string_of(content.get("icon")))
                .unwrap_or_default();
            // A `motion/<chara>/<clip>` source names a child motion clip; the
            // remaining sources name an icon in `source.<chara>.icon.<name>`.
            if let Some(rest) = raw_src.strip_prefix("motion/") {
                frame.child_motion = rest
                    .rsplit('/')
                    .find(|segment| !segment.is_empty())
                    .map(str::to_string);
            } else {
                frame.src = raw_src;
            }
            frame.blend_mode = number_of(content.get("bm")).unwrap_or(16.0) as i32;
            frame.ox = number_of(content.get("ox")).unwrap_or(0.0);
            frame.oy = number_of(content.get("oy")).unwrap_or(0.0);
            if let Some(coord) = content.get("coord") {
                frame.x = coord_number(coord, 0);
                frame.y = coord_number(coord, 1);
                frame.z = coord_number(coord, 2);
            }
            frame.angle = number_of(content.get("angle")).unwrap_or(0.0);
            frame.slant_x = number_of(content.get("sx")).unwrap_or(0.0);
            frame.slant_y = number_of(content.get("sy")).unwrap_or(0.0);
            frame.scale_x = number_of(content.get("zx")).unwrap_or(1.0);
            frame.scale_y = number_of(content.get("zy")).unwrap_or(1.0);
            frame.opacity = number_of(content.get("opa")).unwrap_or(1.0);
            frame.mask = number_of(content.get("mask")).unwrap_or(0.0) != 0.0;
            frame.action = string_of(content.get("action"))
                .or_else(|| string_of(content.get("ac")))
                .unwrap_or_default();
            frame.has_sync = content.get("sync").map(PsbValue::as_bool).unwrap_or(false);
        }
        frames.push(frame);
    }
    frames
}

fn coord_number(coord: &PsbValue, index: usize) -> f64 {
    match coord {
        PsbValue::List(items) => items.get(index).and_then(PsbValue::as_f64).unwrap_or(0.0),
        PsbValue::Array(items) => items
            .get(index)
            .map(|value| f64::from(*value))
            .unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Evaluate a node's frame state at `time` frames.
pub(crate) fn evaluate_frames(frames: &[Frame], time: f64) -> FrameState {
    if frames.is_empty() {
        return FrameState::default();
    }
    let mut active_index = 0usize;
    for (index, frame) in frames.iter().enumerate() {
        if frame.time <= time {
            active_index = index;
        } else {
            break;
        }
    }
    let active = &frames[active_index];
    if active.frame_type == 3
        && let Some(next) = frames.iter().find(|frame| frame.time > time)
    {
        let start = active.time;
        let span = next.time - start;
        let t = if span > 0.0 {
            ((time - start) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        return interpolate(active, next, t);
    }
    if active.frame_type == 0 {
        return FrameState {
            active: false,
            src: active.src.clone(),
            ..FrameState::default()
        };
    }
    FrameState {
        active: true,
        x: active.x,
        y: active.y,
        z: active.z,
        angle: active.angle,
        slant_x: active.slant_x,
        slant_y: active.slant_y,
        scale_x: active.scale_x,
        scale_y: active.scale_y,
        opacity: active.opacity,
        blend_mode: active.blend_mode,
        src: active.src.clone(),
        action: active.action.clone(),
        has_sync: active.has_sync,
        mask: active.mask,
    }
}

fn interpolate(a: &Frame, b: &Frame, t: f64) -> FrameState {
    let lerp = |from: f64, to: f64| from + (to - from) * t;
    FrameState {
        active: true,
        x: lerp(a.x, b.x),
        y: lerp(a.y, b.y),
        z: lerp(a.z, b.z),
        angle: lerp(a.angle, b.angle),
        slant_x: lerp(a.slant_x, b.slant_x),
        slant_y: lerp(a.slant_y, b.slant_y),
        scale_x: lerp(a.scale_x, b.scale_x),
        scale_y: lerp(a.scale_y, b.scale_y),
        opacity: lerp(a.opacity, b.opacity),
        blend_mode: a.blend_mode,
        src: a.src.clone(),
        action: a.action.clone(),
        has_sync: a.has_sync,
        mask: a.mask,
    }
}

// ---------------------------------------------------------------------------
// PSB schema helpers
// ---------------------------------------------------------------------------

fn navigate<'a>(mut value: &'a PsbValue, path: &[&str]) -> Option<&'a PsbValue> {
    for key in path {
        value = value.get(key)?;
    }
    Some(value)
}

/// Collect the `source.<chara>.icon.<name>` canvas geometry keyed by the
/// `src/<chara>/<name>` path a frame's `content.src` carries. Reference
/// `collectRootResources` / `MotionSnapshot::iconList`.
fn collect_icons(root: &PsbValue, icons: &mut HashMap<String, IconInfo>) {
    let Some(source) = root.get("source").and_then(PsbValue::as_dict) else {
        return;
    };
    for (chara, node) in source {
        let Some(icon) = node.get("icon").and_then(PsbValue::as_dict) else {
            continue;
        };
        for (name, info) in icon {
            let width = number_of(info.get("width")).unwrap_or(0.0);
            let height = number_of(info.get("height")).unwrap_or(0.0);
            if width <= 0.0 || height <= 0.0 {
                continue;
            }
            icons.insert(
                format!("src/{chara}/{name}"),
                IconInfo {
                    width,
                    height,
                    origin_x: number_of(info.get("originX")).unwrap_or(0.0),
                    origin_y: number_of(info.get("originY")).unwrap_or(0.0),
                },
            );
        }
    }
}

fn list_of(value: Option<&PsbValue>) -> Option<&[PsbValue]> {
    value.and_then(PsbValue::as_list)
}

fn string_of(value: Option<&PsbValue>) -> Option<String> {
    match value? {
        PsbValue::String(text) => Some(text.clone()),
        _ => None,
    }
}

fn label_of(value: &PsbValue) -> Option<String> {
    string_of(value.get("label"))
        .or_else(|| string_of(value.get("name")))
        .or_else(|| string_of(value.get("id")))
}

fn number_of(value: Option<&PsbValue>) -> Option<f64> {
    value.and_then(PsbValue::as_f64)
}

fn bool_of(value: Option<&PsbValue>) -> Option<bool> {
    value.map(PsbValue::as_bool)
}

fn push_unique(list: &mut Vec<String>, value: String) {
    if !list.contains(&value) {
        list.push(value);
    }
}

fn timeline_ease_weight(easing: f64) -> f32 {
    if easing > 0.0 {
        (easing + 1.0) as f32
    } else if easing < 0.0 {
        (1.0 / (1.0 - easing)) as f32
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(entries: Vec<(&str, PsbValue)>) -> PsbValue {
        PsbValue::Dict(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    }

    fn text(value: &str) -> PsbValue {
        PsbValue::String(value.to_string())
    }

    fn num(value: i64) -> PsbValue {
        PsbValue::Int(value)
    }

    fn frame(time: i64, frame_type: i64, src: PsbValue) -> PsbValue {
        dict(vec![
            ("time", num(time)),
            ("type", num(frame_type)),
            ("content", dict(vec![("src", src)])),
        ])
    }

    /// A `title` clip whose `main` motion layer names the `char` child clip,
    /// and a `char` clip whose `logo` object layer names the `src/title/logo`
    /// icon. This mirrors the structure `title_bg.mtn` uses
    /// (`motion/title_bg/char_move` -> `char_move`; `src/title/logo`).
    fn snapshot() -> MotionSnapshot {
        let main = dict(vec![
            ("label", text("main")),
            ("type", num(3)),
            (
                "frameList",
                PsbValue::List(vec![frame(0, 3, text("motion/title_bg/char_move"))]),
            ),
        ]);
        let logo = dict(vec![
            ("label", text("logo")),
            ("type", num(0)),
            (
                "frameList",
                PsbValue::List(vec![frame(0, 2, text("src/title/logo"))]),
            ),
        ]);
        let title = dict(vec![
            ("lastTime", num(120)),
            ("layer", PsbValue::List(vec![main])),
        ]);
        let char_move = dict(vec![
            ("lastTime", num(120)),
            ("layer", PsbValue::List(vec![logo])),
        ]);
        let motions = dict(vec![("title", title), ("char_move", char_move)]);
        let owner = dict(vec![("motion", motions)]);
        let object = dict(vec![("title_bg", owner)]);
        let icon = dict(vec![(
            "logo",
            dict(vec![
                ("width", num(437)),
                ("height", num(184)),
                ("originX", num(219)),
                ("originY", num(92)),
            ]),
        )]);
        let source = dict(vec![("title", dict(vec![("icon", icon)]))]);
        let root = dict(vec![("object", object), ("source", source)]);
        MotionSnapshot::from_psb(Psb { root })
    }

    #[test]
    fn source_icons_carry_their_canvas_geometry() {
        let snapshot = snapshot();
        let icon = snapshot
            .icons
            .get("src/title/logo")
            .expect("the source icon is indexed by its frame path");
        // `source/<group>/icon/<name>` carries width/height/originX/originY
        // (`reference/cpp/plugins/motionplayer/RuntimeSupport.cpp:1514`,
        // `reference/cpp/plugins/motionplayer/PlayerInternal.h:1620`).
        assert_eq!(icon.width, 437.0);
        assert_eq!(icon.height, 184.0);
        assert_eq!(icon.origin_x, 219.0);
        assert_eq!(icon.origin_y, 92.0);
    }

    #[test]
    fn a_motion_layer_expands_the_clip_it_names() {
        let snapshot = snapshot();
        let clip = snapshot.clip("title").expect("the title clip parses");
        let nodes = build_node_tree(&snapshot, Some(clip));
        // The clip's layer references resolve to the layer dictionaries, and
        // a `motion/<chara>/<clip>` source plays the named child clip
        // (`reference/cpp/plugins/motionplayer/RuntimeSupport.cpp:1070`,
        // `reference/cpp/plugins/motionplayer/PlayerUpdateChildMotion.cpp:59`).
        // Root, the `main` motion layer, and the `char_move` `logo` object
        // layer it references. Before child-motion expansion the tree stopped
        // at `main`, so the title had no object layer to draw.
        assert_eq!(nodes.len(), 3, "labels: {:?}", node_labels(&nodes));
        assert_eq!(nodes[1].label, "main");
        assert_eq!(nodes[1].child_motion.as_deref(), Some("char_move"));
        assert!(nodes[1].own, "the title clip's own layer stays queryable");
        assert_eq!(nodes[2].label, "logo");
        assert_eq!(nodes[2].node_type, 0);
        assert!(
            !nodes[2].own,
            "the child clip's layers render but do not answer layer queries"
        );
    }

    /// A child reference that points back at an ancestor must not recurse
    /// forever; the clip label stack blocks the cycle like the reference's
    /// active-player set (`reference/cpp/plugins/motionplayer/PlayerRenderItems.cpp:15`).
    #[test]
    fn a_child_motion_cycle_is_bounded() {
        let main = dict(vec![
            ("label", text("main")),
            ("type", num(3)),
            (
                "frameList",
                PsbValue::List(vec![frame(0, 3, text("motion/self/self"))]),
            ),
        ]);
        let clip = dict(vec![
            ("lastTime", num(1)),
            ("layer", PsbValue::List(vec![main])),
        ]);
        let object = dict(vec![(
            "self",
            dict(vec![("motion", dict(vec![("self", clip)]))]),
        )]);
        let snapshot = MotionSnapshot::from_psb(Psb {
            root: dict(vec![("object", object)]),
        });
        let clip = snapshot.clip("self").expect("the self clip parses");
        let nodes = build_node_tree(&snapshot, Some(clip));
        assert_eq!(nodes.len(), 2);
    }

    #[test]
    fn animation_frames_evaluate_by_time() {
        // Static frames select by the last keyframe at or before the time
        // (`reference/cpp/plugins/motionplayer/PlayerUpdateLayerEval.cpp:1`).
        let frames = vec![
            Frame {
                time: 0.0,
                frame_type: 2,
                ..Frame::default()
            },
            Frame {
                time: 10.0,
                frame_type: 2,
                x: 5.0,
                ..Frame::default()
            },
        ];
        assert_eq!(evaluate_frames(&frames, 0.0).x, 0.0);
        assert_eq!(evaluate_frames(&frames, 10.0).x, 5.0);
        assert_eq!(evaluate_frames(&frames, 9.0).x, 0.0);
    }

    fn node_labels(nodes: &[MotionNode]) -> Vec<&str> {
        nodes.iter().map(|node| node.label.as_str()).collect()
    }
}
