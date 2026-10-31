//! Motion data model: PSB schema mapping, node tree and timeline/variable
//! state.
//!
//! Behavioural reference: `reference/cpp/plugins/motionplayer/RuntimeSupport.*`,
//! `PlayerMotionLoad.cpp`, `PlayerTimeline.cpp`, `PlayerVariable.cpp`,
//! `NodeTree.cpp` and `MotionNode.h`. The reference parses the PSB tree into a
//! `MotionSnapshot` and then walks the active clip's `layer` array into a
//! persistent `MotionNode` deque; this module does the same over the compact
//! [`PsbValue`] tree produced by the shared PSB container parser
//! ([`psb_file::container`](crate::natives::plugins::psb_file::container)).
//!
//! Everything here is plain data owned by a [`Player`](super::player::Player)
//! (or a [`ResourceManager`](super::resource::ResourceManager) cache entry);
//! there is no global registry.

use super::animator::Animator;
use super::texture::PixelCache;
use crate::collections::{Map, Set};
use crate::natives::plugins::psb_file::container::{PsbDocument, PsbValue};

/// One variable keyframe (`VariableFrameInfo`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VariableFrame {
    /// The frame label.
    pub(crate) label: String,
    /// The value at the frame.
    pub(crate) value: f64,
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
    /// Whether the label is instant.
    pub(crate) instant: bool,
    /// The keyframes.
    pub(crate) frames: Vec<TimelineFrame>,
}

/// A cubic bezier easing curve (`BezierCurve`): `x` holds the time control
/// points and `y` the eased value control points, `3N+1` entries per segment.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct BezierCurve {
    /// The time control points.
    pub(crate) x: Vec<f64>,
    /// The value control points.
    pub(crate) y: Vec<f64>,
}

impl BezierCurve {
    /// Whether the frame carries no curve at all, which makes the easing the
    /// identity.
    pub(crate) fn is_empty(&self) -> bool {
        self.x.is_empty()
    }
}

/// One spline segment of a rotation control-point curve.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SplineSegment {
    /// The segment's parameter knots.
    pub(crate) x: Vec<f64>,
    /// The segment's values.
    pub(crate) y: Vec<f64>,
    /// The segment's spline slopes.
    pub(crate) p: Vec<f64>,
}

/// A rotation control-point curve: the main bezier control points, the time
/// knots and the per-segment spline data.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ControlPointCurve {
    /// The main bezier X control points.
    pub(crate) x: Vec<f64>,
    /// The main bezier Y control points.
    pub(crate) y: Vec<f64>,
    /// The time knot points.
    pub(crate) t: Vec<f64>,
    /// The per-segment spline data.
    pub(crate) s: Vec<SplineSegment>,
}

impl ControlPointCurve {
    /// Whether the curve carries no knots, which leaves the rotation alone.
    pub(crate) fn is_empty(&self) -> bool {
        self.t.is_empty()
    }
}

/// The four vertex colours a frame carries when the content names none: a
/// half gray with a full alpha on every corner.
pub(crate) const DEFAULT_NODE_COLORS: [[u8; 4]; 4] = [[0x80, 0x80, 0x80, 0xFF]; 4];

/// One frame of a layer's `frameList`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Frame {
    /// The frame time.
    pub(crate) time: f64,
    /// `0` invisible, `2` static, `3` interpolate.
    pub(crate) frame_type: i32,
    /// Whether the frame carries a `content` dictionary. A frame without
    /// content is invisible; the trailing one is only a terminator.
    pub(crate) has_content: bool,
    /// The icon source reference (`src/<chara>/<icon>`).
    pub(crate) src: String,
    /// The child motion clip label a `motion/<chara>/<clip>` source names.
    pub(crate) child_motion: Option<String>,
    /// The blend mode (`bm`).
    pub(crate) blend_mode: i32,
    /// The four packed vertex colours, each as red, green, blue and alpha
    /// bytes.
    pub(crate) colors: [[u8; 4]; 4],
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
    /// Whether the node is mirrored along x (`fx`).
    pub(crate) flip_x: bool,
    /// Whether the node is mirrored along y (`fy`).
    pub(crate) flip_y: bool,
    /// Scale x (`zx`).
    pub(crate) scale_x: f64,
    /// Scale y (`zy`).
    pub(crate) scale_y: f64,
    /// Opacity (`opa`), `0..=1`.
    pub(crate) opacity: f64,
    /// The content's property mask: the bit field that says which of the
    /// content's members carry a value.
    pub(crate) content_mask: i64,
    /// Action name.
    pub(crate) action: String,
    /// Whether the frame carries a sync marker.
    pub(crate) has_sync: bool,
    /// Position easing curve (`ccc`).
    pub(crate) ccc: BezierCurve,
    /// Angle easing curve (`acc`).
    pub(crate) acc: BezierCurve,
    /// Zoom easing curve (`zcc`).
    pub(crate) zcc: BezierCurve,
    /// Slant easing curve (`scc`).
    pub(crate) scc: BezierCurve,
    /// Rotation control-point curve (`cp`): the path the position follows
    /// between two keyframes.
    pub(crate) cp: ControlPointCurve,
    /// The child motion's clock offset (`motion.timeOffset`): the child clip
    /// runs that much ahead of the parent clock.
    pub(crate) motion_time_offset: f64,
}

impl Default for Frame {
    fn default() -> Self {
        Self {
            time: 0.0,
            frame_type: 0,
            has_content: false,
            src: String::new(),
            child_motion: None,
            blend_mode: 16,
            colors: DEFAULT_NODE_COLORS,
            ox: 0.0,
            oy: 0.0,
            x: 0.0,
            y: 0.0,
            z: 0.0,
            angle: 0.0,
            slant_x: 0.0,
            slant_y: 0.0,
            flip_x: false,
            flip_y: false,
            scale_x: 1.0,
            scale_y: 1.0,
            opacity: 1.0,
            content_mask: 0,
            action: String::new(),
            has_sync: false,
            ccc: BezierCurve::default(),
            acc: BezierCurve::default(),
            zcc: BezierCurve::default(),
            scc: BezierCurve::default(),
            cp: ControlPointCurve::default(),
            motion_time_offset: 0.0,
        }
    }
}

/// The child clip a type-3 node's active keyframe selects.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChildSlot {
    /// The child motion clip label.
    pub(crate) clip: String,
    /// The keyframe time the child clip starts at (`slot.clipStartTime`).
    pub(crate) start: f64,
    /// The child motion's clock offset (`slot.motionTimeOffset`).
    pub(crate) offset: f64,
}

/// A snapshot of the state a frame contributes (after interpolation).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FrameState {
    /// Whether the frame is visible.
    pub(crate) active: bool,
    /// Clip origin x (`ox`).
    pub(crate) ox: f64,
    /// Clip origin y (`oy`).
    pub(crate) oy: f64,
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
    /// Whether the node is mirrored along x.
    pub(crate) flip_x: bool,
    /// Whether the node is mirrored along y.
    pub(crate) flip_y: bool,
    /// Scale.
    pub(crate) scale_x: f64,
    /// Scale.
    pub(crate) scale_y: f64,
    /// Opacity `0..=1`.
    pub(crate) opacity: f64,
    /// Blend mode.
    pub(crate) blend_mode: i32,
    /// The four packed vertex colours, each as red, green, blue and alpha
    /// bytes.
    pub(crate) colors: [[u8; 4]; 4],
    /// Source reference.
    pub(crate) src: String,
    /// Action.
    pub(crate) action: String,
    /// Sync marker.
    pub(crate) has_sync: bool,
}

impl Default for FrameState {
    fn default() -> Self {
        Self {
            active: true,
            ox: 0.0,
            oy: 0.0,
            x: 0.0,
            y: 0.0,
            z: 0.0,
            angle: 0.0,
            slant_x: 0.0,
            slant_y: 0.0,
            flip_x: false,
            flip_y: false,
            scale_x: 1.0,
            scale_y: 1.0,
            opacity: 1.0,
            blend_mode: 16,
            colors: DEFAULT_NODE_COLORS,
            src: String::new(),
            action: String::new(),
            has_sync: false,
        }
    }
}

/// A node in the persistent layer tree.
#[derive(Debug, Clone)]
pub(crate) struct MotionNode {
    /// Parent index (`0` is the root).
    pub(crate) parent: usize,
    /// The layer label.
    pub(crate) label: String,
    /// Whether the node belongs to this player rather than an expanded child
    /// motion. Only own nodes answer the layer queries; the renderer draws
    /// both.
    pub(crate) own: bool,
    /// The node type (`Motion::LayerType*`).
    pub(crate) node_type: i32,
    /// The shape type for shape nodes.
    pub(crate) shape: i32,
    /// The coordinate plane the position is interpolated in: `0` is the
    /// screen plane, `1` the plane that carries the depth.
    pub(crate) coordinate_mode: i32,
    /// The transform order.
    pub(crate) transform_order: [i32; 4],
    /// The inheritance mask (`inheritMask`).
    pub(crate) inherit_flags: i32,
    /// The raw `stencilType`; bit 2 marks a stencil-composite group.
    pub(crate) stencil_type: i32,
    /// The layer labels whose images stencil-mask a composite group
    /// (`stencilCompositeMaskLayerList`).
    pub(crate) mask_layers: Vec<String>,
    /// The parent-clock time from which the node draws. A layer expanded from
    /// a child motion starts at the parent keyframe that named the clip.
    pub(crate) visible_from: f64,
    /// The keyframes.
    pub(crate) frames: Vec<Frame>,
    /// The evaluated local state.
    pub(crate) local: FrameState,
    /// The accumulated world state (after inheritance).
    pub(crate) accumulated: FrameState,
    /// The accumulated 2x2 transform `[m11, m12, m21, m22]`, composed from the
    /// parent's matrix and this node's local transform.
    pub(crate) matrix: [f64; 4],
    /// The parent-clock offset this node's frames run on. A layer expanded from
    /// a child motion starts where the parent frame named that clip, so it
    /// evaluates at `parent time - time_offset`.
    pub(crate) time_offset: f64,
    /// Whether an ancestor is visible.
    pub(crate) draw_flag: bool,
    /// Whether any frame names a source.
    pub(crate) has_source: bool,
    /// Mesh transform mode (`meshTransform`): `0` none, `1` patch, `2` grid.
    pub(crate) mesh_type: i32,
    /// Mesh child-sync flags (`meshSyncChildMask`).
    pub(crate) mesh_flags: i32,
    /// Mesh subdivision level (`meshDivision`).
    pub(crate) mesh_division: i32,
    /// The computed grid dimensions `[div_x, div_y]`.
    pub(crate) mesh_div: [i32; 2],
    /// The deformed grid points of this frame, as `(x, y)` pairs.
    pub(crate) mesh_points: Vec<f32>,
    /// The inverse of the node's world size matrix, for child deformation.
    pub(crate) mesh_inv: [f64; 4],
    /// The negated world origin the inverse matrix is applied to.
    pub(crate) mesh_inv_offset: [f32; 2],
    /// The node's four corner vertices in player space.
    pub(crate) vertices: [f64; 8],
    /// The type-7 shape's axis-aligned bounds, when the node is a shape.
    pub(crate) shape_aabb: [f64; 4],
    /// The nearest ancestor type-7 shape that clips this node.
    pub(crate) parent_clip: Option<usize>,
    /// The type-3 node this node's child clip instance is expanded from.
    pub(crate) child_owner: Option<usize>,
    /// The child clip label this expanded node belongs to.
    pub(crate) child_clip: Option<String>,
    /// The child clip a type-3 node's active keyframe selects.
    pub(crate) child_slot: Option<ChildSlot>,
}

impl MotionNode {
    /// A node attached to `parent` with default layer state.
    pub(crate) fn new(parent: usize, label: String) -> Self {
        Self {
            parent,
            label,
            own: true,
            node_type: 0,
            shape: 0,
            coordinate_mode: 0,
            transform_order: [0, 1, 2, 3],
            inherit_flags: 0x1FC,
            stencil_type: 0,
            mask_layers: Vec::new(),
            visible_from: 0.0,
            frames: Vec::new(),
            local: FrameState::default(),
            accumulated: FrameState::default(),
            matrix: [1.0, 0.0, 0.0, 1.0],
            time_offset: 0.0,
            draw_flag: true,
            has_source: false,
            mesh_type: 0,
            mesh_flags: 0,
            mesh_division: 0,
            mesh_div: [0, 0],
            mesh_points: Vec::new(),
            mesh_inv: [0.0, 0.0, 0.0, 0.0],
            mesh_inv_offset: [0.0, 0.0],
            vertices: [0.0; 8],
            shape_aabb: [0.0; 4],
            parent_clip: None,
            child_owner: None,
            child_clip: None,
            child_slot: None,
        }
    }
}

/// The canvas geometry of a `source.<chara>.icon.<name>` entry.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct IconInfo {
    /// The `source.<group>` the icon belongs to.
    pub(crate) group: String,
    /// The icon's name inside its group.
    pub(crate) name: String,
    /// The icon width in pixels.
    pub(crate) width: f64,
    /// The icon height in pixels.
    pub(crate) height: f64,
    /// The icon's horizontal offset inside its group texture atlas.
    pub(crate) left: f64,
    /// The icon's vertical offset inside its group texture atlas.
    pub(crate) top: f64,
    /// The horizontal anchor origin.
    pub(crate) origin_x: f64,
    /// The vertical anchor origin.
    pub(crate) origin_y: f64,
    /// Whether the icon's own pixel chunk is `RL` run-length compressed.
    pub(crate) compressed: bool,
}

/// The pixel atlas a group's icons are cropped from
/// (`source.<group>.texture`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TextureInfo {
    /// The atlas width in pixels.
    pub(crate) width: f64,
    /// The atlas height in pixels.
    pub(crate) height: f64,
    /// Whether the atlas pixel chunk is `RL` run-length compressed.
    pub(crate) compressed: bool,
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
    /// The normalized `psb://` container name the module was loaded from.
    pub(crate) container: String,
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
    pub(crate) loop_timelines: Map<String, bool>,
    /// The loop restart time per timeline.
    pub(crate) timeline_loop_times: Map<String, f64>,
    /// The total frame count per timeline.
    pub(crate) timeline_total_frames: Map<String, f64>,
    /// The variable value range.
    pub(crate) variable_ranges: Map<String, (f64, f64)>,
    /// The variable keyframes.
    pub(crate) variable_frames: Map<String, Vec<VariableFrame>>,
    /// Instant variable labels.
    pub(crate) instant_variables: Set<String>,
    /// The control tracks of each timeline, by label.
    pub(crate) timeline_controls: Map<String, Vec<TimelineTrack>>,
    /// The motion clips.
    pub(crate) clips: Vec<MotionClip>,
    /// Label → clip index.
    pub(crate) clip_index: Map<String, usize>,
    /// The action/sync keyframes of every clip layer. Every clip is scanned,
    /// not only the ones the active node tree contains.
    pub(crate) event_tracks: Vec<EventTrack>,
    /// `source.<chara>.icon.<name>` geometry keyed by `src/<chara>/<name>`.
    pub(crate) icons: Map<String, IconInfo>,
    /// `source.<chara>.texture` atlases keyed by `<chara>`.
    pub(crate) textures: Map<String, TextureInfo>,
    /// The decoded pixel chunks this module's icons draw from, keyed by their
    /// `psb://` storage name and shared by every player that loads the
    /// snapshot.
    pub(crate) pixels: PixelCache,
    /// The active root layer list.
    pub(crate) layers: Vec<PsbValue>,
    /// The raw metadata dictionary (for `Player::initPhysics` / `metadata`).
    pub(crate) metadata: PsbValue,
    /// The raw root dictionary.
    pub(crate) root: PsbValue,
}

impl MotionSnapshot {
    /// Parse a snapshot from a parsed PSB container loaded from `container`.
    ///
    /// The snapshot keeps its own copy of the container's root value; the
    /// `psb://` media serves the container's resources from the parsed
    /// document itself.
    pub(crate) fn from_psb(document: &PsbDocument, container: &str) -> Self {
        let root = document.root.clone();
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
        let mut icons = Map::default();
        let mut textures = Map::default();
        collect_sources(&root, &mut icons, &mut textures);

        let mut snapshot = Self {
            container: container.to_string(),
            width,
            height,
            main_timelines: Vec::new(),
            diff_timelines: Vec::new(),
            variable_labels: Vec::new(),
            loop_timelines: Map::default(),
            timeline_loop_times: Map::default(),
            timeline_total_frames: Map::default(),
            variable_ranges: Map::default(),
            variable_frames: Map::default(),
            instant_variables: Set::default(),
            timeline_controls: Map::default(),
            clips: Vec::new(),
            clip_index: Map::default(),
            event_tracks: Vec::new(),
            icons,
            textures,
            pixels: PixelCache::default(),
            layers: Vec::new(),
            metadata: PsbValue::Null,
            root,
        };

        // These lists live on the `metadata` dictionary itself, not on
        // `metadata.base`.
        snapshot.collect_variable_list(&metadata);
        snapshot.collect_instant_variables(&metadata);
        snapshot.collect_control_labels(&metadata);
        snapshot.collect_selector_labels(&metadata);
        snapshot.collect_timeline_controls(&metadata);
        snapshot.collect_clips();
        let event_tracks = snapshot.collect_event_tracks();
        snapshot.event_tracks = event_tracks;
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

    /// Collect the variable labels the enabled controller lists name.
    fn collect_control_labels(&mut self, metadata: &PsbValue) {
        const CONTROLS: &[(&str, &[&str])] = &[
            ("bustControl", &["var_lr", "var_ud"]),
            ("hairControl", &["var_lr", "var_lrm", "var_ud"]),
            ("partsControl", &["var_lr", "var_lrm", "var_ud"]),
            ("loopControl", &["var_loop"]),
            ("eyeControl", &["label"]),
            ("eyebrowControl", &["label"]),
            ("mouthControl", &["label", "talkLabel"]),
            ("transitionControl", &["label"]),
        ];
        for (key, roles) in CONTROLS {
            let Some(list) = list_of(metadata.get(key)) else {
                continue;
            };
            for item in list {
                if !bool_of(item.get("enabled")).unwrap_or(false) {
                    continue;
                }
                for role in *roles {
                    if let Some(label) = string_of(item.get(role))
                        && !label.is_empty()
                    {
                        push_unique(&mut self.variable_labels, label);
                    }
                }
            }
        }
    }

    /// Collect the variable labels the enabled selector controls name.
    fn collect_selector_labels(&mut self, metadata: &PsbValue) {
        let Some(list) = list_of(metadata.get("selectorControl")) else {
            return;
        };
        for item in list {
            let Some(label) = label_of(item) else {
                continue;
            };
            if !bool_of(item.get("enabled")).unwrap_or(false) || label.is_empty() {
                continue;
            }
            push_unique(&mut self.variable_labels, label);
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
            let mut tracks = Vec::new();
            if let Some(variable_list) = list_of(item.get("variableList")) {
                for variable in variable_list {
                    let Some(track_label) = label_of(variable) else {
                        continue;
                    };
                    let mut track = TimelineTrack {
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
                    if !track.frames.is_empty() {
                        tracks.push(track);
                    }
                }
            }
            self.timeline_controls.insert(label, tracks);
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
                &PsbValue::Dictionary(vec![("layer".into(), PsbValue::List(layers))]),
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
        // The clip's loop is read in two steps: a numeric `loopTime` is the
        // loop restart time, and the clip loops when it is not negative;
        // otherwise the boolean `loop`/`repeat`/`is_loop` decides, and a
        // looping clip restarts at time 0 - so its timeline wraps instead of
        // stopping at the clip's last frame.
        let (loop_time, looping) = match number_of(motion.get("loopTime")) {
            Some(value) => (value, value >= 0.0),
            None => {
                let looping = ["loop", "repeat", "is_loop"]
                    .iter()
                    .find_map(|key| bool_value(motion.get(key)))
                    .unwrap_or(false);
                (if looping { 0.0 } else { -1.0 }, looping)
            }
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

    /// Collect the action/sync keyframes of the container's layer
    /// dictionaries and of every clip's layer array.
    ///
    /// The container scan records every layer dictionary it finds into one
    /// flat list, and the event scan walks that list before the clips' own
    /// layer arrays. A clip's top layer therefore raises its keyframes twice
    /// and a nested layer raises them once.
    fn collect_event_tracks(&self) -> Vec<EventTrack> {
        let mut tracks = Vec::new();
        collect_container_layer_events(&self.root, &mut Vec::new(), &mut tracks);
        for clip in &self.clips {
            collect_layer_events(&clip.layers, &mut tracks);
        }
        tracks
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
pub(crate) fn prime_timelines(snapshot: &MotionSnapshot, states: &mut Map<String, TimelineState>) {
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

/// The clock of a child-motion instance.
///
/// The child clip's own length bounds it: a clip that does not loop stops at
/// its last frame, and a clip that loops restarts at its loop time.
pub(crate) fn child_clip_time(clip: &MotionClip, local_time: f64) -> f64 {
    wrap_timeline(local_time, clip.total_frames, clip.loop_time).0
}

/// Step the active timelines by `dt` frames, returning the events generated.
pub(crate) fn step_timelines(
    snapshot: &MotionSnapshot,
    states: &mut Map<String, TimelineState>,
    dt: f64,
) -> Vec<MotionEvent> {
    let mut events = Vec::new();
    let labels: Vec<String> = states
        .iter()
        .filter(|(_, state)| state.playing)
        .map(|(label, _)| label.clone())
        .collect();
    for label in labels {
        let Some(state) = states.get_mut(&label) else {
            continue;
        };
        let tracks = snapshot
            .timeline_controls
            .get(&label)
            .map(Vec::as_slice)
            .unwrap_or_default();
        state.current_time += dt;
        let (wrapped, keep) =
            wrap_timeline(state.current_time, state.total_frames, state.loop_time);
        state.current_time = wrapped;
        // A timeline stops when its time ran past the length and cannot wrap:
        // the wrap fails, and a failed wrap ends the timeline. A timeline that
        // can wrap keeps playing, so a clip that loops plays for ever.
        if !keep {
            state.playing = false;
            events.push(MotionEvent {
                kind: 1,
                param1: label,
                param2: String::new(),
            });
        }
        step_timeline_controls(state, tracks, dt);
    }
    events
}

fn step_timeline_controls(state: &mut TimelineState, tracks: &[TimelineTrack], dt: f64) {
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
        }
    }
}

/// One action/sync keyframe of a clip layer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EventFrame {
    /// The keyframe time.
    pub(crate) time: f64,
    /// The action name (`content.action`), empty when the frame only syncs.
    pub(crate) action: String,
    /// Whether the frame carries a nonzero `content.sync` marker.
    pub(crate) has_sync: bool,
}

/// The action/sync keyframes of one clip layer, keyed by its label.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EventTrack {
    /// The layer label the events name.
    pub(crate) label: String,
    /// The keyframes that carry an action or a sync marker.
    pub(crate) frames: Vec<EventFrame>,
}

/// Collect the action/sync keyframes of a clip's layer array.
///
/// The clip scan reads the clip's own layer dictionaries and does not descend
/// into their children.
fn collect_layer_events(layers: &[PsbValue], tracks: &mut Vec<EventTrack>) {
    for layer in layers {
        collect_layer_event_frames(layer, tracks);
    }
}

/// Collect the action/sync keyframes of every layer dictionary in the
/// container, in the container's order.
///
/// The container scan walks the whole tree and records every layer-like
/// dictionary into one flat list; the event scan walks that list.
fn collect_container_layer_events(
    value: &PsbValue,
    path: &mut Vec<String>,
    tracks: &mut Vec<EventTrack>,
) {
    match value {
        PsbValue::Dictionary(entries) => {
            if is_layer_dictionary(value, path.as_slice()) {
                collect_layer_event_frames(value, tracks);
            }
            for (key, child) in entries {
                path.push(key.clone());
                collect_container_layer_events(child, path, tracks);
                path.pop();
            }
        }
        PsbValue::List(items) => {
            for (index, item) in items.iter().enumerate() {
                path.push(index.to_string());
                collect_container_layer_events(item, path, tracks);
                path.pop();
            }
        }
        _ => {}
    }
}

/// Whether a dictionary is one of the container's layer dictionaries.
///
/// A dictionary is a layer when it carries a non-empty label and either sits
/// under a `layer` path segment or looks like a layer by its keys.
fn is_layer_dictionary(value: &PsbValue, path: &[String]) -> bool {
    let has_label = ["label", "name", "id"]
        .iter()
        .copied()
        .any(|key| matches!(value.get(key), Some(PsbValue::String(text)) if !text.is_empty()));
    if !has_label {
        return false;
    }
    path.iter()
        .any(|part| part.to_ascii_lowercase().contains("layer"))
        || value.get("layer_id").is_some()
        || value.get("layer_type").is_some()
        || (value.get("label").is_some() && value.get("frameList").is_some())
        || (value.get("width").is_some()
            && value.get("height").is_some()
            && (value.get("left").is_some() || value.get("top").is_some()))
}

/// Collect the action/sync keyframes of one layer dictionary.
fn collect_layer_event_frames(layer: &PsbValue, tracks: &mut Vec<EventTrack>) {
    let mut frames = Vec::new();
    if let Some(frame_list) = list_of(layer.get("frameList")) {
        for frame in frame_list {
            let Some(content) = frame.get("content") else {
                continue;
            };
            let action = string_of(content.get("action")).unwrap_or_default();
            let has_sync = sync_marker(content.get("sync"));
            if action.is_empty() && !has_sync {
                continue;
            }
            frames.push(EventFrame {
                time: number_of(frame.get("time")).unwrap_or(0.0),
                action,
                has_sync,
            });
        }
    }
    if frames.is_empty() {
        return;
    }
    tracks.push(EventTrack {
        label: label_of(layer).unwrap_or_default(),
        frames,
    });
}

/// Whether a frame's `sync` marker is set.
///
/// The marker is read as a number, and only the float and the integer forms
/// count; a double marker is ignored.
fn sync_marker(value: Option<&PsbValue>) -> bool {
    match value {
        Some(PsbValue::Int(number)) => *number != 0,
        Some(PsbValue::Float(number)) => *number != 0.0,
        _ => false,
    }
}

/// Collect the action and sync events whose keyframe time is crossed when the
/// evaluation clock advances from `previous` to `current`.
pub(crate) fn scan_frame_events(
    tracks: &[EventTrack],
    previous: f64,
    current: f64,
) -> Vec<MotionEvent> {
    let mut events = Vec::new();
    if current <= previous {
        return events;
    }
    for track in tracks {
        for frame in &track.frames {
            if frame.time <= previous || frame.time > current {
                continue;
            }
            if !frame.action.is_empty() {
                events.push(MotionEvent {
                    kind: 0,
                    param1: frame.action.clone(),
                    param2: track.label.clone(),
                });
            }
            if frame.has_sync {
                events.push(MotionEvent {
                    kind: 1,
                    param1: track.label.clone(),
                    param2: String::new(),
                });
            }
        }
    }
    events
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
    build_node_tree_at(snapshot, clip, 0.0)
}

/// Build the node tree for `clip` at `time`.
///
/// A type-3 node's child clip instance is expanded for the keyframe active at
/// that time, so a clip whose keyframes name different child clips builds the
/// one that is playing.
pub(crate) fn build_node_tree_at(
    snapshot: &MotionSnapshot,
    clip: Option<&MotionClip>,
    time: f64,
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
    let placement = NodePlacement {
        child: false,
        time_offset: 0.0,
        start_time: 0.0,
    };
    for layer in layers {
        append_node(&mut nodes, 0, layer, snapshot, &mut stack, placement, time);
    }
    nodes
}

/// Where a node sits relative to the parent motion's clock.
#[derive(Clone, Copy)]
struct NodePlacement {
    child: bool,
    time_offset: f64,
    start_time: f64,
}

fn append_node(
    nodes: &mut Vec<MotionNode>,
    parent: usize,
    layer: &PsbValue,
    snapshot: &MotionSnapshot,
    stack: &mut Vec<String>,
    placement: NodePlacement,
    time: f64,
) {
    let index = nodes.len();
    let label = label_of(layer).unwrap_or_default();
    let mut node = MotionNode::new(parent, label);
    node.own = !placement.child;
    node.time_offset = placement.time_offset;
    node.visible_from = placement.start_time;
    node.node_type = number_of(layer.get("type")).unwrap_or(0.0) as i32;
    node.shape = number_of(layer.get("shape")).unwrap_or(0.0) as i32;
    node.coordinate_mode = number_of(layer.get("coordinate")).unwrap_or(0.0) as i32;
    node.inherit_flags = number_of(layer.get("inheritMask")).unwrap_or(f64::from(0x1FCu32)) as i32;
    node.stencil_type = number_of(layer.get("stencilType")).unwrap_or(0.0) as i32;
    node.mask_layers = list_of(layer.get("stencilCompositeMaskLayerList"))
        .map(|labels| {
            labels
                .iter()
                .filter_map(|label| string_of(Some(label)))
                .collect()
        })
        .unwrap_or_default();
    node.mesh_type = number_of(layer.get("meshTransform")).unwrap_or(0.0) as i32;
    node.mesh_flags = number_of(layer.get("meshSyncChildMask")).unwrap_or(0.0) as i32;
    node.mesh_division = number_of(layer.get("meshDivision")).unwrap_or(0.0) as i32;
    if let Some(order) = layer.get("transformOrder").and_then(PsbValue::as_array) {
        for (slot, value) in node.transform_order.iter_mut().zip(order.iter()) {
            *slot = *value as i32;
        }
    }
    node.frames = parse_frames(layer.get("frameList"));
    node.has_source = node
        .frames
        .iter()
        .any(|frame| !frame.src.is_empty() || frame.child_motion.is_some());
    node.child_slot = child_slot(&node.frames, (time - placement.time_offset).max(0.0))
        .or_else(|| first_child_slot(&node.frames));
    let child_clip = node.child_slot.as_ref().map(|slot| slot.clip.clone());
    nodes.push(node);
    if let Some(slot) = nodes[index].child_slot.clone()
        && !slot.clip.is_empty()
        && !stack.iter().any(|item| item == &slot.clip)
        && let Some(clip) = snapshot.clip(&slot.clip)
    {
        // The child clip starts at the parent keyframe that names it, so every
        // layer of that clip runs that much behind the parent clock.
        let child_offset = placement.time_offset + slot.start - slot.offset;
        // The clip a type-3 node names first exists from the player's start:
        // its clock is zero before the keyframe that named it, so it draws
        // its first pose right away. A clip that replaces it later starts at
        // the keyframe that named it instead.
        let child_start_time = if names_first_child(&nodes[index].frames, &slot) {
            placement.start_time
        } else {
            placement.time_offset + slot.start
        };
        let layers = clip.layers.clone();
        stack.push(slot.clip.clone());
        for layer in &layers {
            let child_index = nodes.len();
            append_node(
                nodes,
                index,
                layer,
                snapshot,
                stack,
                NodePlacement {
                    child: true,
                    time_offset: child_offset,
                    start_time: child_start_time,
                },
                time,
            );
            // Every layer the child clip expanded belongs to this instance,
            // except the layers a nested child clip instance owns.
            for child in &mut nodes[child_index..] {
                if child.child_owner.is_none() {
                    child.child_owner = Some(index);
                    child.child_clip = child_clip.clone();
                }
            }
        }
        stack.pop();
    }
    if let Some(children) = list_of(layer.get("children")) {
        for child_layer in children {
            append_node(nodes, index, child_layer, snapshot, stack, placement, time);
        }
    }
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
            has_content: item
                .get("content")
                .is_some_and(|content| !matches!(content, PsbValue::Null)),
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
            frame.content_mask = number_of(content.get("mask")).unwrap_or(0.0) as i64;
            // The blend mode and the colour are read when the content's mask
            // carries them.
            if frame.content_mask & 0x20000 != 0 {
                frame.blend_mode = number_of(content.get("bm")).unwrap_or(16.0) as i32;
            }
            if frame.content_mask & 0x200 != 0 {
                frame.colors = parse_node_colors(content);
            }
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
            frame.opacity =
                (number_of(content.get("opa")).unwrap_or(255.0) / 255.0).clamp(0.0, 1.0);
            // The flip flags are read when the content's mask carries them.
            if frame.content_mask & 0x4 != 0 {
                frame.flip_x = number_of(content.get("fx")).unwrap_or(0.0) != 0.0;
            }
            if frame.content_mask & 0x8 != 0 {
                frame.flip_y = number_of(content.get("fy")).unwrap_or(0.0) != 0.0;
            }
            frame.action = string_of(content.get("action"))
                .or_else(|| string_of(content.get("ac")))
                .unwrap_or_default();
            frame.has_sync = content.get("sync").map(PsbValue::as_bool).unwrap_or(false);
            frame.ccc = parse_bezier_curve(content.get("ccc"));
            frame.acc = parse_bezier_curve(content.get("acc"));
            frame.zcc = parse_bezier_curve(content.get("zcc"));
            frame.scc = parse_bezier_curve(content.get("scc"));
            frame.cp = parse_control_point_curve(content.get("cp"));
            frame.motion_time_offset = motion_time_offset(content, frame.content_mask);
        }
        frames.push(frame);
    }
    frames
}

/// The keyframe active at `time`.
///
/// The trailing entry of a multi-frame list is only a terminator, so it is
/// never the active keyframe.
pub(crate) fn active_frame(frames: &[Frame], time: f64) -> Option<&Frame> {
    if frames.is_empty() {
        return None;
    }
    let mut index = 0usize;
    for (position, frame) in frames.iter().enumerate() {
        if frame.time <= time {
            index = position;
        } else {
            break;
        }
    }
    if frames.len() > 1 {
        index = index.min(frames.len() - 2);
    }
    let frame = &frames[index];
    frame.has_content.then_some(frame)
}

/// The child clip the keyframe active at `time` selects, when it names one.
pub(crate) fn child_slot(frames: &[Frame], time: f64) -> Option<ChildSlot> {
    let frame = active_frame(frames, time)?;
    Some(ChildSlot {
        clip: frame.child_motion.clone()?,
        start: frame.time,
        offset: frame.motion_time_offset,
    })
}

/// The first keyframe that names a child clip.
pub(crate) fn first_child_slot(frames: &[Frame]) -> Option<ChildSlot> {
    frames
        .iter()
        .find(|frame| frame.child_motion.is_some() && frame.has_content)
        .map(|frame| ChildSlot {
            clip: frame.child_motion.clone().unwrap_or_default(),
            start: frame.time,
            offset: frame.motion_time_offset,
        })
}

/// Whether `slot` names the child clip a layer names first.
///
/// The clip a type-3 node names first is the one its player starts with; a
/// later keyframe can replace it with another clip.
pub(crate) fn names_first_child(frames: &[Frame], slot: &ChildSlot) -> bool {
    first_child_slot(frames).is_some_and(|first| first.clip == slot.clip)
}

/// The node's four vertex colours, from the content's `color` member.
///
/// The member is either a dictionary of four packed colours — one per vertex,
/// keyed `"0"` through `"3"` — or a single packed colour broadcast to all of
/// them. The packed value carries the red channel in its low byte, the green
/// channel in the second, the blue channel in the third and the alpha in the
/// high byte.
fn parse_node_colors(content: &PsbValue) -> [[u8; 4]; 4] {
    let Some(color) = content.get("color") else {
        return DEFAULT_NODE_COLORS;
    };
    if let Some(packed) = color.as_f64() {
        return [unpack_color(packed); 4];
    }
    let mut colors = DEFAULT_NODE_COLORS;
    for (index, slot) in colors.iter_mut().enumerate() {
        if let Some(packed) = color.get(&index.to_string()).and_then(PsbValue::as_f64) {
            *slot = unpack_color(packed);
        }
    }
    colors
}

/// Unpack a `0xAABBGGRR` colour into red, green, blue and alpha bytes.
fn unpack_color(packed: f64) -> [u8; 4] {
    let packed = packed as i64 as u32;
    [
        (packed & 0xFF) as u8,
        ((packed >> 8) & 0xFF) as u8,
        ((packed >> 16) & 0xFF) as u8,
        ((packed >> 24) & 0xFF) as u8,
    ]
}

/// Parse a `ccc`/`acc`/`zcc`/`scc` curve dictionary. Both `x` and `y` must be
/// present for the curve to be used.
fn parse_bezier_curve(value: Option<&PsbValue>) -> BezierCurve {
    let Some(dict) = value else {
        return BezierCurve::default();
    };
    let (Some(x_list), Some(y_list)) = (list_of(dict.get("x")), list_of(dict.get("y"))) else {
        return BezierCurve::default();
    };
    let numbers =
        |list: &[PsbValue]| -> Vec<f64> { list.iter().filter_map(PsbValue::as_f64).collect() };
    BezierCurve {
        x: numbers(x_list),
        y: numbers(y_list),
    }
}

/// Parse a `cp` rotation control-point curve.
///
/// The curve needs its `x`, `y` and `t` lists; the per-segment spline data
/// (`s`) is optional.
fn parse_control_point_curve(value: Option<&PsbValue>) -> ControlPointCurve {
    let Some(dict) = value else {
        return ControlPointCurve::default();
    };
    let (Some(x_list), Some(y_list), Some(t_list)) = (
        list_of(dict.get("x")),
        list_of(dict.get("y")),
        list_of(dict.get("t")),
    ) else {
        return ControlPointCurve::default();
    };
    let numbers =
        |list: &[PsbValue]| -> Vec<f64> { list.iter().filter_map(PsbValue::as_f64).collect() };
    let mut curve = ControlPointCurve {
        x: numbers(x_list),
        y: numbers(y_list),
        t: numbers(t_list),
        s: Vec::new(),
    };
    if let Some(segments) = list_of(dict.get("s")) {
        for segment in segments {
            let mut spline = SplineSegment::default();
            if let Some(values) = list_of(segment.get("x")) {
                spline.x = numbers(values);
            }
            if let Some(values) = list_of(segment.get("y")) {
                spline.y = numbers(values);
            }
            if let Some(values) = list_of(segment.get("p")) {
                spline.p = numbers(values);
            }
            curve.s.push(spline);
        }
    }
    curve
}

/// The child motion's clock offset, from the content's `motion` sub-object.
///
/// The sub-object is read when the content's mask carries the motion bit; its
/// `timeOffset` shifts the child motion's own clock.
fn motion_time_offset(content: &PsbValue, content_mask: i64) -> f64 {
    if content_mask & 0x80000 == 0 {
        return 0.0;
    }
    let Some(motion) = content.get("motion") else {
        return 0.0;
    };
    number_of(motion.get("timeOffset")).unwrap_or(0.0)
}

/// Evaluate a curve at `t`, or return `t` unchanged when the frame carries no
/// curve.
fn ease(curve: &BezierCurve, t: f64) -> f64 {
    if curve.is_empty() {
        t
    } else {
        evaluate_bezier_curve(curve, t)
    }
}

/// Evaluate a `3N+1` cubic bezier curve at `t`.
///
/// The control points are read as shared endpoints, so each segment is
/// `y[i-3..=i]`; `t` is the curve parameter itself, not a per-segment ratio.
fn evaluate_bezier_curve(curve: &BezierCurve, t: f64) -> f64 {
    let n = curve.x.len();
    if n < 2 || curve.y.len() != n {
        return t;
    }
    if curve.x[0] >= t {
        return curve.y[0];
    }
    if curve.x[n - 1] <= t {
        return curve.y[n - 1];
    }
    let mut i = 0;
    while i < n && curve.x[i] < t {
        i += 3;
    }
    if i < 3 || i >= n {
        return t;
    }
    let (p0, p1, p2, p3) = (curve.y[i - 3], curve.y[i - 2], curve.y[i - 1], curve.y[i]);
    let u = 1.0 - t;
    u * u * u * p0 + 3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t * p3
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

/// Evaluate a rotation control-point curve at `input_t`.
///
/// The curve is sampled per segment: the segment's spline maps the local time
/// to the curve parameter, and the main bezier maps that parameter to the
/// rotation. A curve the segment list cannot answer leaves the rotation
/// alone.
fn evaluate_control_point_curve(curve: &ControlPointCurve, input_t: f64) -> (f64, f64) {
    let identity = (1.0, 0.0);
    if curve.t.len() < 2 || curve.x.len() < 4 || curve.y.len() < 4 {
        return identity;
    }
    let mut segment_index = 0usize;
    let mut main_index = 0usize;
    for index in 1..curve.t.len() {
        main_index += 3;
        if curve.t[index] >= input_t {
            segment_index = index - 1;
            break;
        }
        segment_index = index - 1;
    }
    let Some(segment) = curve.s.get(segment_index) else {
        return identity;
    };
    let start = curve.t[segment_index];
    let end = curve.t.get(segment_index + 1).copied().unwrap_or(start);
    let local_t = if end != start {
        (input_t - start) / (end - start)
    } else {
        0.0
    };
    let mut parameter = local_t;
    if !segment.x.is_empty() && segment.x.len() == segment.y.len() {
        if segment.x[0] >= local_t {
            parameter = segment.y[0];
        } else if *segment.x.last().unwrap_or(&0.0) <= local_t {
            parameter = *segment.y.last().unwrap_or(&0.0);
        } else if let Some(sub_index) =
            (0..segment.x.len() - 1).find(|index| segment.x[index + 1] >= local_t)
        {
            let x0 = segment.x[sub_index];
            let x1 = segment.x[sub_index + 1];
            let y0 = segment.y[sub_index];
            let y1 = segment.y[sub_index + 1];
            let span = x1 - x0;
            if span != 0.0 {
                let u = (local_t - x0) / span;
                let p0 = segment.p.get(sub_index).copied().unwrap_or(0.0);
                let p1 = segment.p.get(sub_index + 1).copied().unwrap_or(0.0);
                parameter =
                    span * span * ((u * u * u - u) * p1 + ((1.0 - u).powi(3) - (1.0 - u)) * p0)
                        / 6.0
                        + u * y1
                        + (1.0 - u) * y0;
            }
        }
    }
    if main_index < 3 || main_index >= curve.x.len() || main_index >= curve.y.len() {
        return identity;
    }
    let px = [
        curve.x[main_index - 3],
        curve.x[main_index - 2],
        curve.x[main_index - 1],
        curve.x[main_index],
    ];
    let py = [
        curve.y[main_index - 3],
        curve.y[main_index - 2],
        curve.y[main_index - 1],
        curve.y[main_index],
    ];
    let u = 1.0 - parameter;
    let cos = u * u * u * px[0]
        + 3.0 * u * u * parameter * px[1]
        + 3.0 * u * parameter * parameter * px[2]
        + parameter * parameter * parameter * px[3];
    let sin = u * u * u * py[0]
        + 3.0 * u * u * parameter * py[1]
        + 3.0 * u * parameter * parameter * py[2]
        + parameter * parameter * parameter * py[3];
    (cos, sin)
}

/// The position between two keyframes.
///
/// The `ccc` curve eases the interpolation ratio. A frame that carries a `cp`
/// curve does not move along a straight line: the curve rotates the path the
/// position follows.
fn interpolate_position(
    easing: &BezierCurve,
    rotation: &ControlPointCurve,
    src: (f64, f64, f64),
    dst: (f64, f64, f64),
    coordinate_mode: i32,
    ratio: f64,
) -> (f64, f64, f64) {
    let lerp = |from: f64, to: f64, r: f64| from + (to - from) * r;
    if src == dst {
        return src;
    }
    let eased = if easing.is_empty() {
        ratio
    } else {
        evaluate_bezier_curve(easing, ratio)
    };
    if rotation.is_empty() {
        return (
            lerp(src.0, dst.0, eased),
            lerp(src.1, dst.1, eased),
            lerp(src.2, dst.2, eased),
        );
    }
    let (cos, sin) = evaluate_control_point_curve(rotation, eased);
    match coordinate_mode {
        0 => {
            let dx = dst.0 - src.0;
            let dy = dst.1 - src.1;
            (
                src.0 + dx * cos - dy * sin,
                src.1 + dx * sin + dy * cos,
                lerp(src.2, dst.2, eased),
            )
        }
        1 => {
            let dx = dst.0 - src.0;
            let dz = dst.2 - src.2;
            (
                src.0 + dx * cos - dz * sin,
                lerp(src.1, dst.1, eased),
                src.2 + dz * cos + dx * sin,
            )
        }
        _ => src,
    }
}

/// Evaluate a node's frame state at `time` frames.
///
/// The last entry of a multi-frame list is a terminator: it is never selected
/// as the active frame, so a trailing contentless frame leaves the last
/// content frame held. Interpolation only runs toward a frame that carries
/// content.
pub(crate) fn evaluate_frames(frames: &[Frame], time: f64, coordinate_mode: i32) -> FrameState {
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
    // The last entry of a multi-frame list is never the active keyframe, so a
    // trailing contentless frame holds the frame before it.
    if frames.len() > 1 {
        active_index = active_index.min(frames.len() - 2);
    }
    let active = &frames[active_index];
    if !active.has_content {
        return FrameState {
            active: false,
            src: active.src.clone(),
            ..FrameState::default()
        };
    }
    if active.frame_type == 3
        && let Some(next) = frames.get(active_index + 1)
        && next.has_content
    {
        let start = active.time;
        let span = next.time - start;
        let t = if span > 0.0 {
            ((time - start) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        return interpolate(active, next, t, coordinate_mode);
    }
    FrameState {
        active: true,
        ox: active.ox,
        oy: active.oy,
        x: active.x,
        y: active.y,
        z: active.z,
        angle: active.angle,
        slant_x: active.slant_x,
        slant_y: active.slant_y,
        flip_x: active.flip_x,
        flip_y: active.flip_y,
        colors: active.colors,
        scale_x: active.scale_x,
        scale_y: active.scale_y,
        opacity: active.opacity,
        blend_mode: active.blend_mode,
        src: active.src.clone(),
        action: active.action.clone(),
        has_sync: active.has_sync,
    }
}

fn interpolate(a: &Frame, b: &Frame, t: f64, coordinate_mode: i32) -> FrameState {
    // Position, angle, zoom and slant each carry their own easing curve; an
    // absent curve leaves the ratio untouched.
    let (x, y, z) = interpolate_position(
        &a.ccc,
        &a.cp,
        (a.x, a.y, a.z),
        (b.x, b.y, b.z),
        coordinate_mode,
        t,
    );
    let t_angle = ease(&a.acc, t);
    let t_scale = ease(&a.zcc, t);
    let t_slant = ease(&a.scc, t);
    let lerp = |from: f64, to: f64, ratio: f64| from + (to - from) * ratio;
    // Opacity is interpolated in the 0..255 integer domain and rounded half
    // away from zero, then converted back.
    let opacity = if a.opacity != b.opacity {
        let interp = lerp(a.opacity * 255.0, b.opacity * 255.0, t);
        let rounded = if interp < 0.0 {
            (interp - 0.5).ceil()
        } else {
            (interp + 0.5).floor()
        };
        (rounded / 255.0).clamp(0.0, 1.0)
    } else {
        a.opacity
    };
    // Rotation takes the shorter of the two directions across the 0/360 seam.
    let angle = if a.angle != b.angle {
        let mut next = b.angle;
        if a.angle >= next {
            if a.angle - next > 180.0 {
                next += 360.0;
            }
        } else if next - a.angle > 180.0 {
            next -= 360.0;
        }
        let mut interp = lerp(a.angle, next, t_angle);
        if interp < 0.0 {
            interp += 360.0;
        } else if interp >= 360.0 {
            interp -= 360.0;
        }
        interp
    } else {
        a.angle
    };
    FrameState {
        active: true,
        ox: lerp(a.ox, b.ox, t),
        oy: lerp(a.oy, b.oy, t),
        x,
        y,
        z,
        angle,
        slant_x: lerp(a.slant_x, b.slant_x, t_slant),
        slant_y: lerp(a.slant_y, b.slant_y, t_slant),
        scale_x: lerp(a.scale_x, b.scale_x, t_scale),
        scale_y: lerp(a.scale_y, b.scale_y, t_scale),
        opacity,
        blend_mode: a.blend_mode,
        colors: a.colors,
        src: if a.src.is_empty() {
            b.src.clone()
        } else {
            a.src.clone()
        },
        action: a.action.clone(),
        has_sync: a.has_sync,
        flip_x: a.flip_x,
        flip_y: a.flip_y,
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

/// Collect the `source` geometry: the `source.<chara>.icon.<name>` canvas
/// geometry keyed by the `src/<chara>/<name>` path a frame's `content.src`
/// carries, and the `source.<chara>.texture` atlas sizes keyed by `<chara>`.
///
/// `width`/`height` fall back to the node's `truncated_width`/
/// `truncated_height`, and a node with no usable size is skipped.
fn collect_sources(
    root: &PsbValue,
    icons: &mut Map<String, IconInfo>,
    textures: &mut Map<String, TextureInfo>,
) {
    let Some(source) = root.get("source").and_then(PsbValue::as_dict) else {
        return;
    };
    for (group, node) in source {
        if let Some(icon) = node.get("icon").and_then(PsbValue::as_dict) {
            for (name, info) in icon {
                let Some((width, height)) = source_size(info) else {
                    continue;
                };
                if width <= 0.0 || height <= 0.0 {
                    continue;
                }
                icons.insert(
                    format!("src/{group}/{name}"),
                    IconInfo {
                        group: group.clone(),
                        name: name.clone(),
                        width,
                        height,
                        left: number_of(info.get("left")).unwrap_or(0.0),
                        top: number_of(info.get("top")).unwrap_or(0.0),
                        origin_x: number_of(info.get("originX")).unwrap_or(0.0),
                        origin_y: number_of(info.get("originY")).unwrap_or(0.0),
                        compressed: is_rl_compressed(info),
                    },
                );
            }
        }
        if let Some(texture) = node.get("texture")
            && let Some((width, height)) = source_size(texture)
            && width > 0.0
            && height > 0.0
        {
            textures.insert(
                group.clone(),
                TextureInfo {
                    width,
                    height,
                    compressed: is_rl_compressed(texture),
                },
            );
        }
    }
}

/// The `width`/`height` of a source node, falling back to its truncated size.
fn source_size(value: &PsbValue) -> Option<(f64, f64)> {
    let width =
        number_of(value.get("width")).or_else(|| number_of(value.get("truncated_width")))?;
    let height =
        number_of(value.get("height")).or_else(|| number_of(value.get("truncated_height")))?;
    Some((width, height))
}

/// Whether a node's `compress` member names the `RL` pixel compression.
fn is_rl_compressed(value: &PsbValue) -> bool {
    matches!(value.get("compress"), Some(PsbValue::String(name)) if name == "RL")
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

/// Whether a PSB value reads as a boolean: a boolean, or a number that is
/// not zero. Any other value is not a boolean, so a reader moves on to the
/// next key.
fn bool_value(value: Option<&PsbValue>) -> Option<bool> {
    match value? {
        PsbValue::Bool(value) => Some(*value),
        PsbValue::Int(value) => Some(*value != 0),
        PsbValue::Float(value) => Some(*value != 0.0),
        PsbValue::Double(value) => Some(*value != 0.0),
        _ => None,
    }
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
        PsbValue::Dictionary(
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
                ("left", num(12)),
                ("top", num(7)),
            ]),
        )]);
        let texture = dict(vec![("width", num(800)), ("height", num(1080))]);
        let source = dict(vec![(
            "title",
            dict(vec![("icon", icon), ("texture", texture)]),
        )]);
        let root = dict(vec![("object", object), ("source", source)]);
        MotionSnapshot::from_psb(
            &PsbDocument {
                root,
                resources: Vec::new(),
            },
            "title.psb",
        )
    }

    /// A clip that loops by flag restarts at time 0, and that restart time is
    /// what the timeline wraps on
    /// (`reference/cpp/plugins/motionplayer/RuntimeSupport.cpp:1010-1017`).
    /// A clip without a loop keeps the reference's `-1`, which stops the
    /// timeline at its last frame.
    #[test]
    fn a_clip_that_loops_by_flag_restarts_at_time_zero() {
        let snapshot = clip_with(vec![("loop", PsbValue::Bool(true))]);
        assert_eq!(snapshot.loop_timelines.get("title"), Some(&true));
        assert_eq!(snapshot.timeline_loop_times.get("title"), Some(&0.0));

        let snapshot = clip_with(vec![("repeat", PsbValue::Bool(true))]);
        assert_eq!(snapshot.loop_timelines.get("title"), Some(&true));
        assert_eq!(snapshot.timeline_loop_times.get("title"), Some(&0.0));

        let snapshot = clip_with(vec![("is_loop", PsbValue::Bool(true))]);
        assert_eq!(snapshot.loop_timelines.get("title"), Some(&true));
        assert_eq!(snapshot.timeline_loop_times.get("title"), Some(&0.0));

        // a numeric loopTime is the restart time itself
        let snapshot = clip_with(vec![("loopTime", num(30))]);
        assert_eq!(snapshot.loop_timelines.get("title"), Some(&true));
        assert_eq!(snapshot.timeline_loop_times.get("title"), Some(&30.0));

        // and a clip that does not loop keeps the reference's -1
        let snapshot = clip_with(vec![]);
        assert_eq!(snapshot.loop_timelines.get("title"), Some(&false));
        assert_eq!(snapshot.timeline_loop_times.get("title"), Some(&-1.0));
    }

    /// The snapshot of `snapshot()`, with the `title` clip carrying the given
    /// extra members.
    fn clip_with(extra: Vec<(&str, PsbValue)>) -> MotionSnapshot {
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
        let mut title = vec![
            ("lastTime", num(120)),
            ("layer", PsbValue::List(vec![main])),
        ];
        title.extend(extra);
        let char_move = dict(vec![
            ("lastTime", num(120)),
            ("layer", PsbValue::List(vec![logo])),
        ]);
        let motions = dict(vec![("title", dict(title)), ("char_move", char_move)]);
        let owner = dict(vec![("motion", motions)]);
        let object = dict(vec![("title_bg", owner)]);
        let icon = dict(vec![(
            "logo",
            dict(vec![
                ("width", num(437)),
                ("height", num(184)),
                ("originX", num(219)),
                ("originY", num(92)),
                ("left", num(12)),
                ("top", num(7)),
            ]),
        )]);
        let texture = dict(vec![("width", num(800)), ("height", num(1080))]);
        let source = dict(vec![(
            "title",
            dict(vec![("icon", icon), ("texture", texture)]),
        )]);
        let root = dict(vec![("object", object), ("source", source)]);
        MotionSnapshot::from_psb(
            &PsbDocument {
                root,
                resources: Vec::new(),
            },
            "title.psb",
        )
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
        // `reference/cpp/plugins/motionplayer/PlayerInternal.h:1620`), plus
        // the atlas crop offsets the texture path uses
        // (`reference/cpp/plugins/motionplayer/PlayerInternal.h:1693-1700`).
        assert_eq!(icon.width, 437.0);
        assert_eq!(icon.height, 184.0);
        assert_eq!(icon.origin_x, 219.0);
        assert_eq!(icon.origin_y, 92.0);
        assert_eq!(icon.left, 12.0);
        assert_eq!(icon.top, 7.0);
        assert_eq!(icon.group, "title");
        assert_eq!(icon.name, "logo");
        let texture = snapshot
            .textures
            .get("title")
            .expect("the group texture is indexed by its group");
        // The atlas size lives on the sibling `texture` node
        // (`reference/cpp/plugins/motionplayer/PlayerInternal.h:1683-1700`).
        assert_eq!(texture.width, 800.0);
        assert_eq!(texture.height, 1080.0);
        assert!(!texture.compressed);
        assert_eq!(snapshot.container, "title.psb");
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
        assert_eq!(
            nodes[1].child_slot.as_ref().map(|slot| slot.clip.as_str()),
            Some("char_move")
        );
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
        let snapshot = MotionSnapshot::from_psb(
            &PsbDocument {
                root: dict(vec![("object", object)]),
                resources: Vec::new(),
            },
            "self.psb",
        );
        let clip = snapshot.clip("self").expect("the self clip parses");
        let nodes = build_node_tree(&snapshot, Some(clip));
        assert_eq!(nodes.len(), 2);
    }

    /// The child clip's own length bounds the child's clock: a clip that does
    /// not loop stops at its last frame, and a clip that loops restarts at its
    /// loop time (`reference/cpp/plugins/motionplayer/RuntimeSupport.h:445-458`).
    #[test]
    fn a_child_clip_bounds_its_own_clock() {
        let clip = MotionClip {
            label: "child".to_string(),
            loop_time: -1.0,
            total_frames: 10.0,
            layers: Vec::new(),
        };
        assert_eq!(child_clip_time(&clip, 4.0), 4.0, "inside the clip");
        assert_eq!(child_clip_time(&clip, 10.0), 10.0, "at the clip's end");
        assert_eq!(
            child_clip_time(&clip, 30.0),
            10.0,
            "a clip that does not loop stops at its last frame"
        );
        let looping = MotionClip {
            label: "looping".to_string(),
            loop_time: 4.0,
            total_frames: 10.0,
            layers: Vec::new(),
        };
        assert_eq!(
            child_clip_time(&looping, 14.0),
            8.0,
            "a clip that loops restarts at its loop time"
        );
        assert_eq!(
            child_clip_time(&looping, 30.0),
            6.0,
            "a clip that loops wraps into its own length"
        );
    }

    #[test]
    fn animation_frames_evaluate_by_time() {
        // Static frames select by the last keyframe at or before the time
        // (`reference/cpp/plugins/motionplayer/PlayerUpdateLayerEval.cpp:1`).
        let frames = vec![
            Frame {
                time: 0.0,
                frame_type: 2,
                has_content: true,
                ..Frame::default()
            },
            Frame {
                time: 10.0,
                frame_type: 2,
                has_content: true,
                x: 5.0,
                ..Frame::default()
            },
            Frame {
                time: 20.0,
                frame_type: 0,
                ..Frame::default()
            },
        ];
        assert_eq!(evaluate_frames(&frames, 0.0, 0).x, 0.0);
        assert_eq!(evaluate_frames(&frames, 9.0, 0).x, 0.0);
        // The last entry is a terminator, never an active keyframe
        // (`reference/cpp/plugins/motionplayer/PlayerUpdateLayerEval.cpp:282-311`).
        assert_eq!(evaluate_frames(&frames, 10.0, 0).x, 5.0);
        assert_eq!(evaluate_frames(&frames, 1_000_000.0, 0).x, 5.0);
    }

    #[test]
    fn a_trailing_contentless_logo_frame_holds_the_last_content_pose() {
        // `title_bg` `char_move`/`logo`: t=40 fades a 1.1x logo to opacity 0,
        // t=75 restores it, and the trailing t=120 frame has no content. The
        // reference holds the t=75 pose to the end of the clip
        // (`reference/cpp/plugins/motionplayer/PlayerUpdateLayerEval.cpp:282-311`,
        // `PlayerUpdateLayerEval.cpp:447-449`).
        let frames = vec![
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
        ];
        assert!(
            !evaluate_frames(&frames, 0.0, 0).active,
            "the contentless first frame is invisible"
        );
        for time in [97.0, 119.9, 120.0, 1_000_000.0] {
            let state = evaluate_frames(&frames, time, 0);
            assert!(state.active, "t={time} must hold a content frame");
            assert_eq!(state.x, -705.0, "t={time}");
            assert_eq!(state.y, -388.0, "t={time}");
            assert_eq!(state.scale_x, 1.0, "t={time}");
            assert_eq!(state.scale_y, 1.0, "t={time}");
            assert_eq!(state.opacity, 1.0, "t={time}");
            assert_eq!(state.src, "src/title/logo", "t={time}");
        }
    }

    #[test]
    fn content_presence_decides_visibility_for_a_one_frame_clip() {
        // A frame with content is visible even when its type is 0; a frame
        // without content is the terminator/invisible frame
        // (`reference/cpp/plugins/motionplayer/PlayerInternal.h:1278`,
        // `PlayerUpdateLayerEval.cpp:447-449`).
        let visible = vec![Frame {
            time: 0.0,
            frame_type: 0,
            has_content: true,
            x: -705.0,
            y: -388.0,
            ..Frame::default()
        }];
        for time in [0.0, 100.0] {
            let state = evaluate_frames(&visible, time, 0);
            assert!(state.active);
            assert_eq!(state.x, -705.0);
        }
        let hidden = vec![Frame {
            time: 0.0,
            frame_type: 0,
            has_content: false,
            ..Frame::default()
        }];
        assert!(!evaluate_frames(&hidden, 0.0, 0).active);
    }

    #[test]
    fn the_clip_origin_interpolates_between_content_frames() {
        // `ox`/`oy` are interpolated with the other slot properties
        // (`reference/cpp/plugins/motionplayer/PlayerInternal.h:1331-1332`).
        let frames = vec![
            Frame {
                time: 0.0,
                frame_type: 3,
                has_content: true,
                ox: 10.0,
                oy: -4.0,
                ..Frame::default()
            },
            Frame {
                time: 10.0,
                frame_type: 3,
                has_content: true,
                ox: 30.0,
                oy: 4.0,
                ..Frame::default()
            },
            Frame {
                time: 20.0,
                frame_type: 0,
                ..Frame::default()
            },
        ];
        let midway = evaluate_frames(&frames, 5.0, 0);
        assert_eq!(midway.ox, 20.0);
        assert_eq!(midway.oy, 0.0);
        let held = evaluate_frames(&frames, 20.0, 0);
        assert_eq!(held.ox, 30.0);
        assert_eq!(held.oy, 4.0);
    }

    #[test]
    fn keyframe_actions_and_syncs_fire_when_their_time_is_crossed() {
        // A frame's `content.action` emits an action event carrying the layer
        // label, and a nonzero `content.sync` emits a sync event
        // (`reference/cpp/plugins/motionplayer/RuntimeSupport.cpp:1814-1920`,
        // `PlayerFrameProgress.cpp:872-885`).
        let layer = dict(vec![
            ("label", text("layer")),
            ("type", num(0)),
            (
                "frameList",
                PsbValue::List(vec![
                    frame(0, 2, text("src/a/b")),
                    dict(vec![
                        ("time", num(10)),
                        ("type", num(2)),
                        (
                            "content",
                            dict(vec![
                                ("src", text("src/a/b")),
                                ("action", text("wave")),
                                ("sync", num(1)),
                            ]),
                        ),
                    ]),
                    dict(vec![("time", num(20)), ("type", num(0))]),
                ]),
            ),
        ]);
        let clip = dict(vec![
            ("lastTime", num(20)),
            ("layer", PsbValue::List(vec![layer])),
        ]);
        let root = dict(vec![(
            "object",
            dict(vec![(
                "test",
                dict(vec![("motion", dict(vec![("clip", clip)]))]),
            )]),
        )]);
        let snapshot = MotionSnapshot::from_psb(
            &PsbDocument {
                root,
                resources: Vec::new(),
            },
            "events.psb",
        );
        let tracks = &snapshot.event_tracks;
        // The clip's layer is scanned twice: once through the container's own
        // layer list and once through the clip's layer array
        // (reference/cpp/plugins/motionplayer/RuntimeSupport.cpp:1907-1919).
        assert_eq!(
            scan_frame_events(tracks, 0.0, 10.0),
            vec![
                MotionEvent {
                    kind: 0,
                    param1: "wave".to_string(),
                    param2: "layer".to_string(),
                },
                MotionEvent {
                    kind: 1,
                    param1: "layer".to_string(),
                    param2: String::new(),
                },
                MotionEvent {
                    kind: 0,
                    param1: "wave".to_string(),
                    param2: "layer".to_string(),
                },
                MotionEvent {
                    kind: 1,
                    param1: "layer".to_string(),
                    param2: String::new(),
                },
            ]
        );
        assert!(scan_frame_events(tracks, 10.0, 20.0).is_empty());
        // A loop restart moves the clock backwards; no crossing is reported.
        assert!(scan_frame_events(tracks, 20.0, 5.0).is_empty());
    }

    /// An inactive clip's layers still raise their action/sync keyframes: the
    /// reference scans the whole snapshot, not the active node tree
    /// (`reference/cpp/plugins/motionplayer/RuntimeSupport.cpp:1907-1917`).
    ///
    /// Every layer is scanned twice, because the container's own layer list
    /// holds the clip's layers as well
    /// (`reference/cpp/plugins/motionplayer/RuntimeSupport.cpp:869-893`,
    /// `:1907-1919`).
    #[test]
    fn every_clip_layer_contributes_events() {
        let eventful = |label: &str, time: i64, action: &str| {
            dict(vec![
                ("label", text(label)),
                ("type", num(0)),
                (
                    "frameList",
                    PsbValue::List(vec![
                        frame(0, 2, text("src/a/b")),
                        dict(vec![
                            ("time", num(time)),
                            ("type", num(2)),
                            (
                                "content",
                                dict(vec![
                                    ("src", text("src/a/b")),
                                    ("action", text(action)),
                                    ("sync", num(1)),
                                ]),
                            ),
                        ]),
                    ]),
                ),
            ])
        };
        let active = dict(vec![
            ("lastTime", num(20)),
            (
                "layer",
                PsbValue::List(vec![eventful("active", 10, "wave")]),
            ),
        ]);
        let inactive = dict(vec![
            ("lastTime", num(20)),
            (
                "layer",
                PsbValue::List(vec![eventful("inactive", 12, "blink")]),
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
        let snapshot = MotionSnapshot::from_psb(
            &PsbDocument {
                root,
                resources: Vec::new(),
            },
            "events.psb",
        );
        let events = scan_frame_events(&snapshot.event_tracks, 0.0, 15.0);
        // Every layer is scanned twice, because the container's own layer list
        // holds the clip's layers as well
        // (reference/cpp/plugins/motionplayer/RuntimeSupport.cpp:869-893,
        // :1907-1919).
        assert_eq!(
            events
                .iter()
                .map(|event| (event.kind, event.param1.as_str(), event.param2.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (0, "wave", "active"),
                (1, "active", ""),
                (0, "blink", "inactive"),
                (1, "inactive", ""),
                (0, "wave", "active"),
                (1, "active", ""),
                (0, "blink", "inactive"),
                (1, "inactive", "")
            ]
        );
    }

    /// The container's own layer list is scanned before the clips' layer
    /// arrays, and a nested layer belongs to the container's list only.
    /// (`reference/cpp/plugins/motionplayer/RuntimeSupport.cpp:869-893`,
    /// `:1907-1919`).
    #[test]
    fn a_clip_layer_raises_its_keyframes_twice_and_a_nested_layer_once() {
        let eventful = |label: &str, time: i64, action: &str| {
            dict(vec![
                ("label", text(label)),
                ("type", num(0)),
                (
                    "frameList",
                    PsbValue::List(vec![
                        frame(0, 2, text("src/a/b")),
                        dict(vec![
                            ("time", num(time)),
                            ("type", num(2)),
                            (
                                "content",
                                dict(vec![
                                    ("src", text("src/a/b")),
                                    ("action", text(action)),
                                    ("sync", num(1)),
                                ]),
                            ),
                        ]),
                    ]),
                ),
            ])
        };
        let child = eventful("child", 12, "blink");
        let parent = dict(vec![
            ("label", text("parent")),
            ("type", num(0)),
            ("children", PsbValue::List(vec![child])),
            (
                "frameList",
                PsbValue::List(vec![
                    frame(0, 2, text("src/a/b")),
                    dict(vec![
                        ("time", num(10)),
                        ("type", num(2)),
                        (
                            "content",
                            dict(vec![
                                ("src", text("src/a/b")),
                                ("action", text("wave")),
                                ("sync", num(1)),
                            ]),
                        ),
                    ]),
                ]),
            ),
        ]);
        let clip = dict(vec![
            ("lastTime", num(20)),
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
            &PsbDocument {
                root,
                resources: Vec::new(),
            },
            "events.psb",
        );
        let events = scan_frame_events(&snapshot.event_tracks, 0.0, 15.0);
        assert_eq!(
            events
                .iter()
                .map(|event| (event.kind, event.param1.as_str(), event.param2.as_str()))
                .collect::<Vec<_>>(),
            vec![
                // The container's layer list holds the parent and its child.
                (0, "wave", "parent"),
                (1, "parent", ""),
                (0, "blink", "child"),
                (1, "child", ""),
                // The clip's layer array holds the parent alone.
                (0, "wave", "parent"),
                (1, "parent", ""),
            ]
        );
    }

    /// A frame's `sync` marker is read as a number, and only the float and
    /// the integer forms count; a double marker is ignored
    /// (`reference/cpp/plugins/motionplayer/RuntimeSupport.cpp:1892-1904`).
    #[test]
    fn only_a_float_or_an_integer_sync_marker_counts() {
        let layer_with = |sync: PsbValue| {
            dict(vec![
                ("label", text("layer")),
                ("type", num(0)),
                (
                    "frameList",
                    PsbValue::List(vec![dict(vec![
                        ("time", num(10)),
                        ("type", num(2)),
                        ("content", dict(vec![("sync", sync)])),
                    ])]),
                ),
            ])
        };
        let clip = dict(vec![
            ("lastTime", num(20)),
            (
                "layer",
                PsbValue::List(vec![layer_with(PsbValue::Float(1.0))]),
            ),
        ]);
        let root = dict(vec![(
            "object",
            dict(vec![(
                "test",
                dict(vec![("motion", dict(vec![("clip", clip)]))]),
            )]),
        )]);
        let snapshot = MotionSnapshot::from_psb(
            &PsbDocument {
                root,
                resources: Vec::new(),
            },
            "events.psb",
        );
        let events = scan_frame_events(&snapshot.event_tracks, 0.0, 20.0);
        assert_eq!(
            events
                .iter()
                .map(|event| (event.kind, event.param1.as_str(), event.param2.as_str()))
                .collect::<Vec<_>>(),
            vec![(1, "layer", ""), (1, "layer", "")],
            "a float marker raises the sync event"
        );

        let clip = dict(vec![
            ("lastTime", num(20)),
            (
                "layer",
                PsbValue::List(vec![layer_with(PsbValue::Double(1.0))]),
            ),
        ]);
        let root = dict(vec![(
            "object",
            dict(vec![(
                "test",
                dict(vec![("motion", dict(vec![("clip", clip)]))]),
            )]),
        )]);
        let snapshot = MotionSnapshot::from_psb(
            &PsbDocument {
                root,
                resources: Vec::new(),
            },
            "events.psb",
        );
        assert!(
            scan_frame_events(&snapshot.event_tracks, 0.0, 20.0).is_empty(),
            "a double marker raises nothing"
        );
    }

    /// A child motion layer runs on its own clock: it starts at the parent
    /// keyframe that names the clip, not at the parent's zero
    /// (`reference/cpp/plugins/motionplayer/PlayerUpdateChildMotion.cpp:172-181`,
    /// `PlayerUpdateLayerEval.cpp:253`).
    #[test]
    fn a_child_motion_layer_carries_the_parent_start_offset() {
        let main = dict(vec![
            ("label", text("main")),
            ("type", num(3)),
            (
                "frameList",
                PsbValue::List(vec![
                    dict(vec![("time", num(0)), ("type", num(0))]),
                    frame(40, 3, text("motion/title_bg/char_move")),
                    dict(vec![("time", num(120)), ("type", num(0))]),
                ]),
            ),
        ]);
        let logo = dict(vec![
            ("label", text("logo")),
            ("type", num(0)),
            (
                "frameList",
                PsbValue::List(vec![
                    dict(vec![
                        ("time", num(0)),
                        ("type", num(3)),
                        (
                            "content",
                            dict(vec![
                                ("src", text("src/title/logo")),
                                ("coord", PsbValue::List(vec![num(0), num(0)])),
                            ]),
                        ),
                    ]),
                    dict(vec![
                        ("time", num(30)),
                        ("type", num(3)),
                        (
                            "content",
                            dict(vec![
                                ("src", text("src/title/logo")),
                                ("coord", PsbValue::List(vec![num(300), num(0)])),
                            ]),
                        ),
                    ]),
                    dict(vec![("time", num(60)), ("type", num(0))]),
                ]),
            ),
        ]);
        let title = dict(vec![
            ("lastTime", num(120)),
            ("layer", PsbValue::List(vec![main])),
        ]);
        let char_move = dict(vec![
            ("lastTime", num(60)),
            ("layer", PsbValue::List(vec![logo])),
        ]);
        let object = dict(vec![(
            "title_bg",
            dict(vec![(
                "motion",
                dict(vec![("title", title), ("char_move", char_move)]),
            )]),
        )]);
        let snapshot = MotionSnapshot::from_psb(
            &PsbDocument {
                root: dict(vec![("object", object)]),
                resources: Vec::new(),
            },
            "title.psb",
        );
        let nodes = build_node_tree(&snapshot, snapshot.clip("title"));
        let child = nodes
            .iter()
            .find(|node| node.label == "logo")
            .expect("the child clip layer is expanded");
        assert_eq!(child.time_offset, 40.0);
        // The child pose at its own t=0 differs from its pose at t=30, so a
        // parent clock of 40 must evaluate the former.
        let frames = &child.frames;
        assert_eq!(evaluate_frames(frames, 0.0, 0).x, 0.0);
        assert_eq!(evaluate_frames(frames, 30.0, 0).x, 300.0);
        assert_eq!(
            evaluate_frames(frames, (40.0f64 - child.time_offset).max(0.0), 0).x,
            0.0
        );
    }

    /// A child motion's own `timeOffset` shifts the child clip's clock: the
    /// child runs at `parent time - start + timeOffset`
    /// (`reference/cpp/plugins/motionplayer/PlayerUpdateChildMotion.cpp:172-181`,
    /// `PlayerInternal.h:1123-1159`).
    #[test]
    fn a_child_motion_time_offset_shifts_its_clock() {
        // The parent names the child at t=40 and the child's own time offset
        // moves its clock ten frames ahead.
        let main = dict(vec![
            ("label", text("main")),
            ("type", num(3)),
            (
                "frameList",
                PsbValue::List(vec![dict(vec![
                    ("time", num(40)),
                    ("type", num(2)),
                    (
                        "content",
                        dict(vec![
                            // The motion bit tells the reader that the frame
                            // carries a motion sub-object.
                            ("mask", num(0x80001)),
                            ("src", text("motion/title_bg/char_move")),
                            (
                                "motion",
                                dict(vec![("mask", num(0)), ("timeOffset", num(10))]),
                            ),
                        ]),
                    ),
                ])]),
            ),
        ]);
        let logo = dict(vec![
            ("label", text("logo")),
            ("type", num(0)),
            (
                "frameList",
                PsbValue::List(vec![
                    dict(vec![
                        ("time", num(0)),
                        ("type", num(3)),
                        (
                            "content",
                            dict(vec![
                                ("src", text("src/title/logo")),
                                ("coord", PsbValue::List(vec![num(0), num(0)])),
                            ]),
                        ),
                    ]),
                    dict(vec![
                        ("time", num(30)),
                        ("type", num(3)),
                        (
                            "content",
                            dict(vec![
                                ("src", text("src/title/logo")),
                                ("coord", PsbValue::List(vec![num(300), num(0)])),
                            ]),
                        ),
                    ]),
                    dict(vec![("time", num(60)), ("type", num(0))]),
                ]),
            ),
        ]);
        let title = dict(vec![
            ("lastTime", num(120)),
            ("layer", PsbValue::List(vec![main])),
        ]);
        let char_move = dict(vec![
            ("lastTime", num(60)),
            ("layer", PsbValue::List(vec![logo])),
        ]);
        let object = dict(vec![(
            "title_bg",
            dict(vec![(
                "motion",
                dict(vec![("title", title), ("char_move", char_move)]),
            )]),
        )]);
        let snapshot = MotionSnapshot::from_psb(
            &PsbDocument {
                root: dict(vec![("object", object)]),
                resources: Vec::new(),
            },
            "title.psb",
        );
        let nodes = build_node_tree(&snapshot, snapshot.clip("title"));
        let child = nodes
            .iter()
            .find(|node| node.label == "logo")
            .expect("the child clip layer is expanded");
        // The child exists from the player's start, but its clock starts at the
        // parent keyframe and its own offset shifts the clock the other way.
        assert_eq!(child.time_offset, 30.0);
        assert_eq!(child.visible_from, 0.0);
        // A parent clock of 40 puts the child at its own t=10, which is a third
        // of the way to the second keyframe.
        let state = evaluate_frames(&child.frames, 40.0 - child.time_offset, 0);
        assert!(
            (state.x - 100.0).abs() < 1e-9,
            "the offset must move the child pose: {}",
            state.x
        );
    }

    /// The `ccc`/`zcc` curves on a frame ease position and zoom instead of the
    /// linear ratio, and opacity is quantized to 1/255
    /// (`reference/cpp/plugins/motionplayer/PlayerInternal.h:1306-1404`).
    #[test]
    fn frame_curves_ease_the_animated_fields() {
        let curve = |x: [f64; 4], y: [f64; 4]| BezierCurve {
            x: x.to_vec(),
            y: y.to_vec(),
        };
        let cubic = |y: [f64; 4], t: f64| {
            let u = 1.0 - t;
            u * u * u * y[0] + 3.0 * u * u * t * y[1] + 3.0 * u * t * t * y[2] + t * t * t * y[3]
        };
        let position = [0.0, 0.05, 0.6, 1.0];
        let zoom = [0.0, 0.2, 0.4, 1.0];
        let frames = vec![
            Frame {
                time: 40.0,
                frame_type: 3,
                has_content: true,
                src: "src/title/logo".to_string(),
                x: 0.0,
                scale_x: 1.0,
                opacity: 1.0,
                ccc: curve([0.0, 0.3, 0.7, 1.0], position),
                zcc: curve([0.0, 0.3, 0.7, 1.0], zoom),
                ..Frame::default()
            },
            Frame {
                time: 75.0,
                frame_type: 3,
                has_content: true,
                src: "src/title/logo".to_string(),
                x: 100.0,
                scale_x: 2.0,
                opacity: 0.0,
                ..Frame::default()
            },
            Frame {
                time: 120.0,
                frame_type: 0,
                ..Frame::default()
            },
        ];
        let state = evaluate_frames(&frames, 57.5, 0);
        let expected_x = 100.0 * cubic(position, 0.5);
        let expected_scale = 1.0 + cubic(zoom, 0.5);
        assert!(
            (state.x - expected_x).abs() < 1e-9,
            "x {} != {expected_x}",
            state.x
        );
        assert!((state.x - 50.0).abs() > 5.0, "the curve is not linear");
        assert!(
            (state.scale_x - expected_scale).abs() < 1e-9,
            "zoom {} != {expected_scale}",
            state.scale_x
        );
        // 1.0 -> 0.0 in the 0..255 domain at t=0.5 is 127.5, rounded away from
        // zero to 128, then converted back.
        let expected_opacity = 128.0 / 255.0;
        assert!(
            (state.opacity - expected_opacity).abs() < 1e-12,
            "opacity {} != {expected_opacity}",
            state.opacity
        );
    }

    #[test]
    fn an_absent_curve_is_the_linear_ratio() {
        let frames = vec![
            Frame {
                time: 0.0,
                frame_type: 3,
                has_content: true,
                x: 0.0,
                scale_x: 1.0,
                ..Frame::default()
            },
            Frame {
                time: 10.0,
                frame_type: 3,
                has_content: true,
                x: 100.0,
                scale_x: 2.0,
                ..Frame::default()
            },
        ];
        let state = evaluate_frames(&frames, 5.0, 0);
        assert_eq!(state.x, 50.0);
        assert_eq!(state.scale_x, 1.5);
    }

    fn node_labels(nodes: &[MotionNode]) -> Vec<&str> {
        nodes.iter().map(|node| node.label.as_str()).collect()
    }
}
