//! Custom effect pipelines: stages, descriptor sets, uniform ring and draws.
//!
//! One [`EffectState`] belongs to a [`super::VulkanState`]. It owns every
//! pipeline created through `render::create_effect_pipeline`, the samplers and
//! descriptor sets those pipelines draw with, and one host-visible uniform ring
//! that carries a draw's parameters (one dynamic offset per draw).
//!
//! The ring has one slot per frame in flight plus one for the offscreen path,
//! so a frame never writes over the bytes a command buffer that is still in
//! flight reads. The descriptor sets a pipeline caches are bounded: the least
//! recently used set leaves the cache when it is full and is freed once no
//! command buffer that binds it is in flight.
//!
//! Layout contract, shared with the programs in `crates/engine/shaders`:
//!
//! | set 0 binding | holds |
//! |---|---|
//! | 0 | the program's uniform block, bound with a dynamic offset |
//! | 1..=4 | one combined image sampler per `BindingDesc`, at its declared index |
//!
//! Push constants carry the vertex transform only (`EFFECT_PUSH_CONSTANT_BYTES`,
//! 64 bytes), so no device limit beyond Vulkan's guaranteed minimum is needed.
//!
//! A graphics pipeline is built for one render pass, so a pipeline is built on
//! first use in each pass and cached by the render pass handle. Both passes this
//! backend records into are stable for the life of a state: the swapchain render
//! pass is reused by every replacement swapchain
//! (`swapchain::create` keeps `old.render_pass`), and an offscreen pass is
//! cached per target format.
#![allow(unsafe_code)]

use std::collections::HashMap;

use ash::vk;
use ash::vk::Handle;

use api::error::RenderError;
use api::shader::{
    BindingDesc, EFFECT_PUSH_CONSTANT_BYTES, EffectPipelineDesc, MAX_EFFECT_TEXTURES, SamplerKind,
    ShaderDataError, VertexLayout,
};

use super::pipelines;
use super::texture::{VulkanTexture, find_memory_type};
use crate::effect::{EffectPipeline, RenderEffect};
use crate::texture::Texture;

/// Bytes of one frame's uniform ring.
///
/// A draw consumes at most `MAX_EFFECT_UNIFORMS * 4` bytes rounded up to the
/// device's offset alignment, so the ring holds hundreds of effects per frame;
/// a frame that needs more is reported as [`RenderError::OutOfMemory`] naming
/// the requirement, never a wrapped write.
const UNIFORM_RING_BYTES: vk::DeviceSize = 64 * 1024;

/// Slots in the uniform ring.
///
/// One slot per frame in flight, so two frames never write over the same
/// bytes, plus one the offscreen path owns: an offscreen render records while
/// a live frame may still be in flight.
const RING_SLOTS: usize = super::MAX_FRAMES_IN_FLIGHT + 1;

/// The ring slot the offscreen path owns.
const OFFSCREEN_SLOT: usize = super::MAX_FRAMES_IN_FLIGHT;

/// Descriptor sets cached per pipeline, one per distinct texture tuple.
///
/// The cache is bounded so a program whose texture tuple changes does not
/// exhaust the pool: the least recently used set leaves the cache and is
/// freed once no command buffer that binds it is in flight.
const MAX_SETS_PER_PIPELINE: usize = 32;

/// Descriptor sets a pipeline may hold while they wait for a free.
///
/// A set waits for the frame that used it to complete, so the wait lasts at
/// most two frames.
const MAX_PENDING_SETS_PER_PIPELINE: usize = 32;

/// Descriptor sets a pipeline's pool holds.
///
/// The cached sets, the sets that wait for a free, and the one a draw
/// allocates while a set leaves the cache.
const POOL_SETS: u32 = (MAX_SETS_PER_PIPELINE + MAX_PENDING_SETS_PER_PIPELINE + 1) as u32;

/// Effect pipelines, their samplers and the frame's uniform ring.
pub(crate) struct EffectState {
    /// Pipelines keyed by [`EffectPipeline::key`].
    pipelines: HashMap<u64, EffectPipelineEntry>,
    /// Host-visible uniform storage, one slot per frame in flight.
    ring: UniformRing,
    /// One sampler per sampler kind this build serves, created once.
    ///
    /// A kind this build does not serve has no entry, and a draw that names
    /// one is refused with [`RenderError::Unsupported`] rather than sampled
    /// with another kind's sampler.
    samplers: HashMap<SamplerKind, vk::Sampler>,
    /// Ring slot the recording in flight writes its uniforms into.
    current_slot: usize,
    /// Frame slot of the live frame being recorded.
    ///
    /// `None` while an offscreen render records.
    current_frame_slot: Option<usize>,
}

/// One created effect pipeline.
struct EffectPipelineEntry {
    /// Vertex stage SPIR-V, kept to build the pipeline in a new render pass.
    vertex: Vec<u8>,
    /// Fragment stage SPIR-V, kept for the same reason.
    fragment: Vec<u8>,
    /// Texture bindings in descriptor order.
    bindings: Vec<BindingDesc>,
    /// Blend slot the pipeline is built with.
    blend_slot: usize,
    /// Uniform block size in bytes (`uniform_words * 4`).
    uniform_bytes: u32,
    /// Descriptor set layout of the program.
    set_layout: vk::DescriptorSetLayout,
    /// Pipeline layout (set 0 plus the 64-byte push-constant block).
    layout: vk::PipelineLayout,
    /// Pool backing this pipeline's cached descriptor sets.
    pool: vk::DescriptorPool,
    /// Graphics pipeline per render pass, built on first use in that pass.
    pipelines: HashMap<u64, vk::Pipeline>,
    /// Descriptor sets cached per texture tuple.
    sets: SetCache,
    /// Descriptor sets that left the cache and cannot be freed yet.
    retired: Vec<RetiredSet>,
}

/// One cached descriptor set.
#[derive(Clone, Copy)]
struct CachedSet {
    /// The set itself.
    set: vk::DescriptorSet,
    /// Use counter, which orders the entries by recency.
    used: u64,
    /// Frame slot of the live frame that last used the set.
    ///
    /// `None` when no live frame used it.
    last_live_slot: Option<usize>,
}

/// Bounded cache of descriptor sets, one per distinct texture tuple.
///
/// The cache holds at most [`MAX_SETS_PER_PIPELINE`] sets. When it is full the
/// least recently used set leaves it, so a program whose texture tuple keeps
/// changing keeps drawing instead of failing on an exhausted pool.
struct SetCache {
    /// Cached sets by texture tuple.
    entries: HashMap<[u64; MAX_EFFECT_TEXTURES], CachedSet>,
    /// Counter that orders the entries by recency.
    clock: u64,
    /// Number of sets the cache holds.
    capacity: usize,
}

impl SetCache {
    /// Creates a cache that holds `capacity` sets.
    fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            clock: 0,
            capacity,
        }
    }

    /// Returns the set cached for `key` and marks it as the most recently used.
    ///
    /// `live_slot` is the frame slot of the live frame that uses the set, or
    /// `None` for an offscreen render. A live use records the slot, so the set
    /// is freed only once that frame has completed.
    fn take(
        &mut self,
        key: &[u64; MAX_EFFECT_TEXTURES],
        live_slot: Option<usize>,
    ) -> Option<vk::DescriptorSet> {
        let used = self.next_clock();
        let entry = self.entries.get_mut(key)?;
        entry.used = used;
        if live_slot.is_some() {
            entry.last_live_slot = live_slot;
        }
        Some(entry.set)
    }

    /// Caches `set` for `key`.
    fn insert(
        &mut self,
        key: &[u64; MAX_EFFECT_TEXTURES],
        set: vk::DescriptorSet,
        live_slot: Option<usize>,
    ) {
        let used = self.next_clock();
        self.entries.insert(
            *key,
            CachedSet {
                set,
                used,
                last_live_slot: live_slot,
            },
        );
    }

    /// Whether the cache holds as many sets as it may.
    fn is_full(&self) -> bool {
        self.entries.len() >= self.capacity
    }

    /// Removes the least recently used set.
    fn evict_lru(&mut self) -> Option<CachedSet> {
        let key = self
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.used)
            .map(|(key, _)| *key)?;
        self.entries.remove(&key)
    }

    /// Advances the recency counter.
    fn next_clock(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }
}

/// A descriptor set that left the cache but cannot be freed yet.
///
/// A set may not be freed while a command buffer that binds it is in flight.
#[derive(Clone, Copy)]
struct RetiredSet {
    /// Pool the set was allocated from.
    pool: vk::DescriptorPool,
    /// The set itself.
    set: vk::DescriptorSet,
    /// Frame slot of the live frame that last used the set.
    ///
    /// `None` when no live frame used it.
    last_live_slot: Option<usize>,
}

impl RetiredSet {
    /// Whether the set may be freed when the live frame of `slot` starts.
    ///
    /// The frame that used the set has completed when the frame that reuses
    /// its slot starts, because the caller waits for that frame's fence
    /// first. A set no live frame used was bound by an offscreen render, which
    /// completed before the render returned.
    fn retirable(&self, slot: usize) -> bool {
        self.last_live_slot.is_none() || self.last_live_slot == Some(slot)
    }
}

/// Which retired descriptor sets a checkpoint may free.
#[derive(Clone, Copy)]
enum Retirement {
    /// The sets whose last live use was the frame of this slot, and the sets
    /// no live frame used.
    Slot(usize),
    /// Every retired set.
    All,
}

/// Frees one descriptor set back into its pool.
fn free_descriptor_set(device: &ash::Device, pool: vk::DescriptorPool, set: vk::DescriptorSet) {
    // SAFETY: the set was allocated from `pool`, and the caller frees it only
    // once no command buffer that binds it is in flight.
    unsafe {
        let _ = device.free_descriptor_sets(pool, &[set]);
    }
}

/// One effect draw's recording context.
pub(crate) struct EffectDrawContext<'a> {
    /// Logical device.
    pub(crate) device: &'a ash::Device,
    /// Command buffer being recorded.
    pub(crate) command: vk::CommandBuffer,
    /// Render pass the command buffer is inside.
    pub(crate) render_pass: vk::RenderPass,
    /// Uploaded textures keyed by texture identity.
    pub(crate) textures: &'a HashMap<u64, VulkanTexture>,
    /// Column-major matrix mapping the unit quad to normalised device space.
    pub(crate) mvp: [f32; 16],
    /// Composed draw opacity, written into the declared opacity word.
    pub(crate) opacity: f32,
    /// Scissor the draw is clipped to.
    pub(crate) scissor: vk::Rect2D,
}

impl EffectState {
    /// Creates the sampler set and the uniform ring.
    ///
    /// The sampler set covers every sampler kind this build serves, so a kind
    /// added to the vocabulary needs its entry in the list below as well.
    ///
    /// `offset_alignment` is the device's
    /// `min_uniform_buffer_offset_alignment`, the stride a dynamic offset must
    /// respect.
    pub(crate) fn create(
        device: &ash::Device,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
        offset_alignment: vk::DeviceSize,
    ) -> Result<Self, RenderError> {
        let ring = UniformRing::create(device, memory_properties, offset_alignment)?;
        let mut samplers: HashMap<SamplerKind, vk::Sampler> = HashMap::new();
        for kind in [
            SamplerKind::LinearClamp,
            SamplerKind::LinearRepeat,
            SamplerKind::NearestClamp,
        ] {
            match create_sampler(device, kind) {
                Ok(sampler) => {
                    samplers.insert(kind, sampler);
                }
                Err(error) => {
                    // SAFETY: only samplers created in this loop exist, and
                    // nothing has been submitted that could use them.
                    unsafe {
                        for sampler in samplers.values() {
                            device.destroy_sampler(*sampler, None);
                        }
                    }
                    ring.destroy(device);
                    return Err(error);
                }
            }
        }
        Ok(Self {
            pipelines: HashMap::new(),
            ring,
            samplers,
            current_slot: 0,
            current_frame_slot: None,
        })
    }

    /// Builds one effect pipeline from `desc`.
    pub(crate) fn create_pipeline(
        &mut self,
        device: &ash::Device,
        desc: &EffectPipelineDesc<'_>,
    ) -> Result<EffectPipeline, RenderError> {
        desc.validate().map_err(|error| match error {
            // A module the data layer rejects is a shader failure, not a bad
            // argument: the caller handed over stage code the backend cannot use.
            ShaderDataError::EmptyName
            | ShaderDataError::EmptyModule { .. }
            | ShaderDataError::TruncatedModule { .. }
            | ShaderDataError::MisalignedModule { .. }
            | ShaderDataError::BadSpirVMagic { .. }
            | ShaderDataError::UnsupportedSpirVVersion { .. }
            | ShaderDataError::WrongStage { .. } => RenderError::Shader {
                message: format!("effect pipeline descriptor is not usable: {error}"),
            },
            other => RenderError::InvalidArgument {
                message: format!("effect pipeline descriptor is not usable: {other}"),
            },
        })?;
        if desc.vertex_layout != VertexLayout::UnitQuad {
            return Err(RenderError::Unsupported {
                message: format!(
                    "effect pipeline {:?} requests a vertex layout this backend does not build",
                    desc.name
                ),
            });
        }
        let blend_slot =
            pipelines::blend_slot(desc.blend).ok_or_else(|| RenderError::Unsupported {
                message: format!(
                    "blend mode {:?} requires destination sampling, which the Vulkan backend \
                     does not implement",
                    desc.blend
                ),
            })?;

        // Create the modules once so a module the driver rejects fails here,
        // not on the first draw inside some render pass. The per-pass pipeline
        // is built later from the retained bytes.
        let vertex_module = pipelines::create_shader_module(device, desc.vertex.code.bytes())?;
        let fragment_module =
            match pipelines::create_shader_module(device, desc.fragment.code.bytes()) {
                Ok(module) => module,
                Err(error) => {
                    // SAFETY: the vertex module was created here and nothing uses it.
                    unsafe { device.destroy_shader_module(vertex_module, None) };
                    return Err(error);
                }
            };
        // SAFETY: neither module has been consumed by a pipeline; the pipeline
        // built per pass re-creates its own modules from the retained bytes.
        unsafe {
            device.destroy_shader_module(vertex_module, None);
            device.destroy_shader_module(fragment_module, None);
        }

        let set_layout = create_set_layout(device, desc.bindings)?;
        let layout = match create_pipeline_layout(device, set_layout) {
            Ok(layout) => layout,
            Err(error) => {
                // SAFETY: the set layout was created here and nothing uses it.
                unsafe { device.destroy_descriptor_set_layout(set_layout, None) };
                return Err(error);
            }
        };
        let pool = match create_descriptor_pool(device, desc.bindings.len()) {
            Ok(pool) => pool,
            Err(error) => {
                // SAFETY: both handles were created here and are unused.
                unsafe {
                    device.destroy_pipeline_layout(layout, None);
                    device.destroy_descriptor_set_layout(set_layout, None);
                }
                return Err(error);
            }
        };

        let handle = EffectPipeline::new(desc.name, desc.uniform_words, desc.opacity_uniform);
        self.pipelines.insert(
            handle.key(),
            EffectPipelineEntry {
                vertex: desc.vertex.code.bytes().to_vec(),
                fragment: desc.fragment.code.bytes().to_vec(),
                bindings: desc.bindings.to_vec(),
                blend_slot,
                uniform_bytes: desc.uniform_words.saturating_mul(4).max(4),
                set_layout,
                layout,
                pool,
                pipelines: HashMap::new(),
                sets: SetCache::new(MAX_SETS_PER_PIPELINE),
                retired: Vec::new(),
            },
        );
        Ok(handle)
    }

    /// Releases one pipeline and everything it owns.
    ///
    /// The caller waits for the device to go idle first, so nothing in flight
    /// still references these handles.
    pub(crate) fn destroy_pipeline(&mut self, device: &ash::Device, key: u64) {
        let Some(entry) = self.pipelines.remove(&key) else {
            return;
        };
        // SAFETY: the device is idle; every handle was created by `device` and
        // is owned by this entry.
        unsafe {
            for pipeline in entry.pipelines.values() {
                device.destroy_pipeline(*pipeline, None);
            }
            device.destroy_descriptor_pool(entry.pool, None);
            device.destroy_pipeline_layout(entry.layout, None);
            device.destroy_descriptor_set_layout(entry.set_layout, None);
        }
    }

    /// Starts a live frame.
    ///
    /// `slot` is the frame's slot: the frame that owned it has completed,
    /// because the caller waited for that frame's fence before recording. The
    /// frame may therefore write into the ring slot again, and the descriptor
    /// sets that frame bound may be freed.
    pub(crate) fn begin_live_frame(&mut self, device: &ash::Device, slot: usize) {
        self.free_retired(device, Retirement::Slot(slot));
        self.ring.reset(slot);
        self.current_slot = slot;
        self.current_frame_slot = Some(slot);
    }

    /// Starts an offscreen render.
    ///
    /// The offscreen path owns a ring slot of its own, because a live frame
    /// may still be in flight while the offscreen render records.
    pub(crate) fn begin_offscreen_frame(&mut self) {
        self.ring.reset(OFFSCREEN_SLOT);
        self.current_slot = OFFSCREEN_SLOT;
        self.current_frame_slot = None;
    }

    /// Ends an offscreen render.
    ///
    /// A queue runs its submissions in order, so the offscreen submit
    /// completes only after every command buffer submitted before it — the
    /// live frames' — has completed. No descriptor set is in use any more, so
    /// every retired set may be freed.
    pub(crate) fn end_offscreen_frame(&mut self, device: &ash::Device) {
        self.free_retired(device, Retirement::All);
    }

    /// Frees the retired descriptor sets the rule allows.
    fn free_retired(&mut self, device: &ash::Device, rule: Retirement) {
        for entry in self.pipelines.values_mut() {
            entry.retired.retain_mut(|retired| {
                let free = match rule {
                    Retirement::All => true,
                    Retirement::Slot(slot) => retired.retirable(slot),
                };
                if free {
                    free_descriptor_set(device, retired.pool, retired.set);
                    false
                } else {
                    true
                }
            });
        }
    }

    /// Records one effect draw.
    pub(crate) fn record(
        &mut self,
        ctx: &EffectDrawContext<'_>,
        effect: &RenderEffect,
    ) -> Result<(), RenderError> {
        let key = effect.pipeline.key();
        let Self {
            pipelines,
            ring,
            samplers,
            current_slot,
            current_frame_slot,
        } = self;
        let Some(entry) = pipelines.get_mut(&key) else {
            // A released pipeline can still be named by a projected scene, the
            // same way a released texture can. The draw is skipped and the
            // cause logged, never drawn with stale state.
            log::warn!("vulkan: effect pipeline {key} was released; skipping the draw");
            return Ok(());
        };
        if effect.textures.len() != entry.bindings.len() {
            return Err(RenderError::InvalidArgument {
                message: format!(
                    "effect pipeline {} binds {} textures, the draw supplies {}",
                    effect.pipeline.name(),
                    entry.bindings.len(),
                    effect.textures.len()
                ),
            });
        }
        let uniform_words = effect.pipeline.uniform_words();
        if effect.uniforms.len() != uniform_words {
            return Err(RenderError::InvalidArgument {
                message: format!(
                    "effect pipeline {} declares {uniform_words} uniform words, the draw \
                     supplies {}",
                    effect.pipeline.name(),
                    effect.uniforms.len()
                ),
            });
        }

        let pass = ctx.render_pass.as_raw();
        if !entry.pipelines.contains_key(&pass) {
            let built = build_pipeline_for_pass(entry, ctx.device, ctx.render_pass)?;
            entry.pipelines.insert(pass, built);
        }
        let Some(pipeline) = entry.pipelines.get(&pass).copied() else {
            return Err(RenderError::Framebuffer {
                message: format!(
                    "effect pipeline {} lost its pipeline for this render pass",
                    effect.pipeline.name()
                ),
            });
        };

        let live_slot = *current_frame_slot;
        let set_key = effect.texture_keys();
        let set = match entry.sets.take(&set_key, live_slot) {
            Some(set) => set,
            None => {
                // The cache is bounded: the least recently used set leaves it
                // and waits for a free, so a program whose texture tuple keeps
                // changing keeps drawing.
                if entry.sets.is_full()
                    && let Some(evicted) = entry.sets.evict_lru()
                {
                    if entry.retired.len() >= MAX_PENDING_SETS_PER_PIPELINE {
                        return Err(RenderError::OutOfMemory {
                            message: format!(
                                "effect pipeline {} holds {MAX_PENDING_SETS_PER_PIPELINE} \
                                 descriptor sets that are still in flight",
                                effect.pipeline.name()
                            ),
                        });
                    }
                    let pool = entry.pool;
                    entry.retired.push(RetiredSet {
                        pool,
                        set: evicted.set,
                        last_live_slot: evicted.last_live_slot,
                    });
                }
                let inputs = SetInputs {
                    device: ctx.device,
                    ring,
                    samplers,
                    textures: ctx.textures,
                };
                let set = allocate_set(&inputs, entry, &effect.textures)?;
                entry.sets.insert(&set_key, set, live_slot);
                set
            }
        };

        // The program's words, with the composed opacity in its declared slot.
        let mut words = [0.0f32; api::shader::MAX_EFFECT_UNIFORMS];
        let count = uniform_words as usize;
        words[..count].copy_from_slice(effect.uniforms.words());
        if let Some(index) = effect.pipeline.opacity_uniform() {
            words[index as usize] = ctx.opacity;
        }
        let uniform_bytes = {
            // SAFETY: the first `count * 4` bytes of `words` are initialised
            // `f32`s, and the slice does not outlive this call.
            unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), count * 4) }
        };
        let offset = ring.write(*current_slot, uniform_bytes)?;
        let push_constants = {
            // SAFETY: `mvp` is sixteen initialised `f32`s, exactly the
            // push-constant block's size.
            unsafe { std::slice::from_raw_parts(ctx.mvp.as_ptr().cast::<u8>(), 64) }
        };

        // SAFETY: the command buffer is recording, the pipeline and set are
        // live, and every handle was created by `ctx.device`.
        unsafe {
            ctx.device
                .cmd_bind_pipeline(ctx.command, vk::PipelineBindPoint::GRAPHICS, pipeline);
            ctx.device.cmd_bind_descriptor_sets(
                ctx.command,
                vk::PipelineBindPoint::GRAPHICS,
                entry.layout,
                0,
                &[set],
                &[offset],
            );
            ctx.device.cmd_push_constants(
                ctx.command,
                entry.layout,
                vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                0,
                push_constants,
            );
            ctx.device.cmd_set_scissor(ctx.command, 0, &[ctx.scissor]);
            ctx.device.cmd_draw(ctx.command, 4, 1, 0, 0);
        }
        Ok(())
    }

    /// Destroys every pipeline, sampler and the ring.
    pub(crate) fn destroy(&mut self, device: &ash::Device) {
        let keys: Vec<u64> = self.pipelines.keys().copied().collect();
        for key in keys {
            self.destroy_pipeline(device, key);
        }
        // SAFETY: the device is idle and every sampler was created here.
        unsafe {
            for sampler in self.samplers.values() {
                device.destroy_sampler(*sampler, None);
            }
        }
        self.samplers.clear();
        self.ring.destroy(device);
    }
}

/// Borrowed inputs a descriptor-set allocation needs.
struct SetInputs<'a> {
    /// Logical device.
    device: &'a ash::Device,
    /// Frame uniform ring the block binding points at.
    ring: &'a UniformRing,
    /// Samplers by kind.
    samplers: &'a HashMap<SamplerKind, vk::Sampler>,
    /// Uploaded textures.
    textures: &'a HashMap<u64, VulkanTexture>,
}

/// Allocates and writes one cached descriptor set for a texture tuple.
fn allocate_set(
    inputs: &SetInputs<'_>,
    entry: &EffectPipelineEntry,
    textures: &[Texture],
) -> Result<vk::DescriptorSet, RenderError> {
    let uniform = [vk::DescriptorBufferInfo::default()
        .buffer(inputs.ring.buffer)
        .offset(0)
        .range(vk::DeviceSize::from(entry.uniform_bytes))];
    let mut images = Vec::with_capacity(entry.bindings.len());
    for (binding, texture) in entry.bindings.iter().zip(textures) {
        let texture_key = texture.key();
        let Some(gpu) = inputs.textures.get(&texture_key) else {
            return Err(RenderError::InvalidArgument {
                message: format!(
                    "an effect draw samples texture {texture_key}, which was not uploaded"
                ),
            });
        };
        let Some(sampler) = inputs.samplers.get(&binding.sampler).copied() else {
            return Err(RenderError::Unsupported {
                message: format!("this build has no sampler for {:?}", binding.sampler),
            });
        };
        images.push(
            vk::DescriptorImageInfo::default()
                .sampler(sampler)
                .image_view(gpu.view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL),
        );
    }

    let set_layouts = [entry.set_layout];
    let allocate = vk::DescriptorSetAllocateInfo::default()
        .descriptor_pool(entry.pool)
        .set_layouts(&set_layouts);
    let allocated = {
        // SAFETY: the pool and layout belong to `entry`; when this tuple has no
        // cached set, the pool has a free set for it.
        unsafe { inputs.device.allocate_descriptor_sets(&allocate) }
    }
    .map_err(|error| RenderError::OutOfMemory {
        message: format!(
            "failed to allocate an effect descriptor set ({error}); a pipeline holds at most \
             {MAX_SETS_PER_PIPELINE} cached sets and {MAX_PENDING_SETS_PER_PIPELINE} sets that \
             wait for a free"
        ),
    })?;
    let Some(set) = allocated.first().copied() else {
        return Err(RenderError::Framebuffer {
            message: "the driver allocated no effect descriptor set".to_owned(),
        });
    };

    let mut writes = Vec::with_capacity(1 + images.len());
    writes.push(
        vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC)
            .descriptor_count(1)
            .buffer_info(&uniform),
    );
    for (index, binding) in entry.bindings.iter().enumerate() {
        writes.push(
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(binding.binding)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .image_info(std::slice::from_ref(&images[index])),
        );
    }
    // SAFETY: every write names a live set of `entry.pool` and borrows locals
    // that outlive the call.
    unsafe {
        inputs.device.update_descriptor_sets(&writes, &[]);
    }
    Ok(set)
}

/// Builds the graphics pipeline for `render_pass` from the entry's retained
/// stage bytes.
fn build_pipeline_for_pass(
    entry: &EffectPipelineEntry,
    device: &ash::Device,
    render_pass: vk::RenderPass,
) -> Result<vk::Pipeline, RenderError> {
    let vertex = pipelines::create_shader_module(device, &entry.vertex)?;
    let fragment = match pipelines::create_shader_module(device, &entry.fragment) {
        Ok(module) => module,
        Err(error) => {
            // SAFETY: the vertex module was created here and nothing uses it.
            unsafe { device.destroy_shader_module(vertex, None) };
            return Err(error);
        }
    };
    let built = pipelines::build_pipeline(
        device,
        entry.layout,
        render_pass,
        vertex,
        fragment,
        entry.blend_slot,
    );
    // SAFETY: a created pipeline captured the modules' code, and the modules
    // are not needed afterwards.
    unsafe {
        device.destroy_shader_module(vertex, None);
        device.destroy_shader_module(fragment, None);
    }
    built
}

/// Creates the descriptor set layout: the uniform block at 0, then one combined
/// image sampler per declared binding.
fn create_set_layout(
    device: &ash::Device,
    bindings: &[BindingDesc],
) -> Result<vk::DescriptorSetLayout, RenderError> {
    let mut layout_bindings = Vec::with_capacity(1 + bindings.len());
    layout_bindings.push(
        vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT),
    );
    for binding in bindings {
        layout_bindings.push(
            vk::DescriptorSetLayoutBinding::default()
                .binding(binding.binding)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT),
        );
    }
    let create_info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&layout_bindings);
    // SAFETY: `create_info` borrows locals valid for the call.
    unsafe { device.create_descriptor_set_layout(&create_info, None) }.map_err(|error| {
        RenderError::Shader {
            message: format!("failed to create the effect descriptor set layout: {error}"),
        }
    })
}

/// Creates the pipeline layout: one descriptor set plus the 64-byte transform
/// block.
fn create_pipeline_layout(
    device: &ash::Device,
    set_layout: vk::DescriptorSetLayout,
) -> Result<vk::PipelineLayout, RenderError> {
    let set_layouts = [set_layout];
    let push_constant = vk::PushConstantRange::default()
        .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
        .offset(0)
        .size(EFFECT_PUSH_CONSTANT_BYTES);
    let push_constants = [push_constant];
    let create_info = vk::PipelineLayoutCreateInfo::default()
        .set_layouts(&set_layouts)
        .push_constant_ranges(&push_constants);
    // SAFETY: `create_info` borrows locals valid for the call.
    unsafe { device.create_pipeline_layout(&create_info, None) }.map_err(|error| {
        RenderError::Shader {
            message: format!("failed to create the effect pipeline layout: {error}"),
        }
    })
}

/// Creates the descriptor pool backing one pipeline's cached sets.
///
/// The pool holds the cached sets, the sets that wait for a free and the one
/// a draw allocates while a set leaves the cache.
fn create_descriptor_pool(
    device: &ash::Device,
    texture_bindings: usize,
) -> Result<vk::DescriptorPool, RenderError> {
    let uniform = vk::DescriptorPoolSize::default()
        .ty(vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC)
        .descriptor_count(POOL_SETS);
    // The API budget caps a program's texture bindings at `MAX_EFFECT_TEXTURES`
    // (4), so one set's sampler descriptors fit a `u32` by construction.
    let per_set = texture_bindings.max(1) as u32;
    let samplers = vk::DescriptorPoolSize::default()
        .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .descriptor_count(POOL_SETS * per_set);
    let sizes = [uniform, samplers];
    let create_info = vk::DescriptorPoolCreateInfo::default()
        .pool_sizes(&sizes)
        .max_sets(POOL_SETS)
        .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET);
    // SAFETY: `create_info` borrows locals valid for the call.
    unsafe { device.create_descriptor_pool(&create_info, None) }.map_err(|error| {
        RenderError::OutOfMemory {
            message: format!("failed to create an effect descriptor pool: {error}"),
        }
    })
}

/// Creates one sampler for `kind`.
///
/// Every kind maps to a distinct filter and address mode, so a kind this
/// build does not know has no sampler to create: the draw is refused with
/// [`RenderError::Unsupported`] instead of being sampled with a substitute.
fn create_sampler(device: &ash::Device, kind: SamplerKind) -> Result<vk::Sampler, RenderError> {
    let (filter, address) = match kind {
        SamplerKind::LinearClamp => (vk::Filter::LINEAR, vk::SamplerAddressMode::CLAMP_TO_EDGE),
        SamplerKind::LinearRepeat => (vk::Filter::LINEAR, vk::SamplerAddressMode::REPEAT),
        SamplerKind::NearestClamp => (vk::Filter::NEAREST, vk::SamplerAddressMode::CLAMP_TO_EDGE),
        _ => {
            return Err(RenderError::Unsupported {
                message: format!("this build has no sampler for {kind:?}"),
            });
        }
    };
    let create_info = vk::SamplerCreateInfo::default()
        .mag_filter(filter)
        .min_filter(filter)
        .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
        .address_mode_u(address)
        .address_mode_v(address)
        .address_mode_w(address)
        .anisotropy_enable(false)
        .unnormalized_coordinates(false)
        .min_lod(0.0)
        .max_lod(0.0);
    // SAFETY: `create_info` borrows a local valid for the call.
    unsafe { device.create_sampler(&create_info, None) }.map_err(|error| {
        RenderError::DeviceUnavailable {
            message: format!("failed to create the effect sampler for {kind:?}: {error}"),
        }
    })
}

/// The byte layout of the uniform ring.
///
/// The ring holds one slot per frame in flight plus one for the offscreen
/// path. A slot is emptied only when the frame that owns it may write into it
/// again, so a frame never overwrites the bytes a command buffer that is
/// still in flight reads.
#[derive(Clone, Copy, Debug)]
struct RingLayout {
    /// Base offset of every slot inside the buffer.
    base: [vk::DeviceSize; RING_SLOTS],
    /// Bytes a slot may hold.
    capacity: vk::DeviceSize,
    /// Bytes used in every slot.
    used: [vk::DeviceSize; RING_SLOTS],
    /// Offset alignment the device requires.
    alignment: vk::DeviceSize,
    /// Distance between two slots.
    stride: vk::DeviceSize,
}

impl RingLayout {
    /// Lays out `capacity` bytes per slot with the device's alignment.
    fn new(capacity: vk::DeviceSize, alignment: vk::DeviceSize) -> Self {
        let stride = capacity.next_multiple_of(alignment);
        let mut base = [0; RING_SLOTS];
        for (index, slot_base) in base.iter_mut().enumerate() {
            *slot_base = stride * index as vk::DeviceSize;
        }
        Self {
            base,
            capacity,
            used: [0; RING_SLOTS],
            alignment,
            stride,
        }
    }

    /// Bytes the buffer behind this layout needs.
    fn total_bytes(&self) -> vk::DeviceSize {
        self.stride * RING_SLOTS as vk::DeviceSize
    }

    /// Empties one slot.
    fn reset(&mut self, slot: usize) {
        self.used[slot] = 0;
    }

    /// Reserves `bytes` in `slot` and returns the buffer offset to bind.
    fn allocate(&mut self, slot: usize, bytes: usize) -> Result<u32, RenderError> {
        let offset = self.base[slot] + self.used[slot].next_multiple_of(self.alignment);
        let end = offset.checked_add(bytes as vk::DeviceSize).ok_or_else(|| {
            RenderError::OutOfMemory {
                message: "the effect uniform size overflows the ring".to_owned(),
            }
        })?;
        if end > self.base[slot] + self.capacity {
            return Err(RenderError::OutOfMemory {
                message: format!(
                    "this frame's effect uniforms need {bytes} bytes, the ring slot holds {}",
                    self.capacity
                ),
            });
        }
        self.used[slot] = end - self.base[slot];
        u32::try_from(offset).map_err(|_| RenderError::OutOfMemory {
            message: "the effect uniform offset exceeds the 32-bit dynamic-offset range".to_owned(),
        })
    }
}

/// Host-visible uniform storage, one slot per frame in flight, consumed by
/// bump allocation.
struct UniformRing {
    /// Host-visible buffer.
    buffer: vk::Buffer,
    /// Memory backing [`Self::buffer`].
    memory: vk::DeviceMemory,
    /// Mapped address of [`Self::memory`], stored as an address so the state
    /// stays free of raw pointers.
    mapped: usize,
    /// The slots' byte layout.
    layout: RingLayout,
}

impl UniformRing {
    /// Creates the ring.
    fn create(
        device: &ash::Device,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
        offset_alignment: vk::DeviceSize,
    ) -> Result<Self, RenderError> {
        let layout = RingLayout::new(UNIFORM_RING_BYTES, offset_alignment.max(4));
        let size = layout.total_bytes();
        let buffer_info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(vk::BufferUsageFlags::UNIFORM_BUFFER)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        // SAFETY: `buffer_info` borrows a local valid for the call.
        let buffer = unsafe { device.create_buffer(&buffer_info, None) }.map_err(|error| {
            RenderError::OutOfMemory {
                message: format!("failed to create the effect uniform ring: {error}"),
            }
        })?;

        // SAFETY: `buffer` was just created by `device`.
        let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };
        let memory_type = match find_memory_type(
            memory_properties,
            requirements.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        ) {
            Ok(memory_type) => memory_type,
            Err(error) => {
                // SAFETY: `buffer` is owned here and unused elsewhere.
                unsafe { device.destroy_buffer(buffer, None) };
                return Err(error);
            }
        };
        let allocate_info = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size)
            .memory_type_index(memory_type);
        // SAFETY: `allocate_info` borrows a local valid for the call.
        let memory = unsafe { device.allocate_memory(&allocate_info, None) }.map_err(|error| {
            // SAFETY: `buffer` is owned here and unused elsewhere.
            unsafe { device.destroy_buffer(buffer, None) };
            RenderError::OutOfMemory {
                message: format!("failed to allocate effect uniform memory: {error}"),
            }
        })?;
        // SAFETY: `memory` was allocated for `buffer`.
        if let Err(error) = unsafe { device.bind_buffer_memory(buffer, memory, 0) } {
            // SAFETY: both handles are owned here and unused elsewhere.
            unsafe {
                device.destroy_buffer(buffer, None);
                device.free_memory(memory, None);
            }
            return Err(RenderError::OutOfMemory {
                message: format!("failed to bind effect uniform memory: {error}"),
            });
        }
        // SAFETY: the memory is host-visible and not referenced by any GPU work
        // yet; the mapping stays valid until `destroy`.
        let mapping = unsafe { device.map_memory(memory, 0, size, vk::MemoryMapFlags::empty()) };
        let mapped = match mapping {
            Ok(mapped) => mapped,
            Err(error) => {
                // SAFETY: both handles are owned here and unused elsewhere.
                unsafe {
                    device.destroy_buffer(buffer, None);
                    device.free_memory(memory, None);
                }
                return Err(RenderError::OutOfMemory {
                    message: format!("failed to map effect uniform memory: {error}"),
                });
            }
        };
        Ok(Self {
            buffer,
            memory,
            mapped: mapped as usize,
            layout,
        })
    }

    /// Empties the slot a frame owns.
    fn reset(&mut self, slot: usize) {
        self.layout.reset(slot);
    }

    /// Copies `bytes` into the ring's `slot` and returns the dynamic offset to
    /// bind.
    fn write(&mut self, slot: usize, bytes: &[u8]) -> Result<u32, RenderError> {
        let offset = self.layout.allocate(slot, bytes.len())?;
        // SAFETY: the offset lies inside the mapped capacity, and the frame
        // that last wrote this slot has been waited on before the slot was
        // reset.
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                (self.mapped as *mut u8).add(offset as usize),
                bytes.len(),
            );
        }
        Ok(offset)
    }

    /// Unmaps and frees the ring.
    fn destroy(&self, device: &ash::Device) {
        // SAFETY: the device is idle; the mapping and both handles were created
        // here and are not used afterwards.
        unsafe {
            device.unmap_memory(self.memory);
            device.destroy_buffer(self.buffer, None);
            device.free_memory(self.memory, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every frame in flight owns a ring slot of its own.
    #[test]
    fn the_ring_gives_every_frame_in_flight_its_own_slot() {
        let mut layout = RingLayout::new(UNIFORM_RING_BYTES, 256);
        let first = layout.allocate(0, 64).expect("the first slot");
        let second = layout.allocate(1, 64).expect("the second slot");
        let offscreen = layout
            .allocate(OFFSCREEN_SLOT, 64)
            .expect("the offscreen slot");
        assert_ne!(first, second);
        assert_ne!(second, offscreen);
        assert_ne!(first, offscreen);
        for offset in [first, second, offscreen] {
            assert_eq!(offset % 256, 0, "the offset must suit the device alignment");
        }
    }

    /// The slot a frame reuses starts empty, and the other slots keep their
    /// bytes.
    #[test]
    fn a_reset_slot_starts_empty_and_leaves_the_other_slots_alone() {
        let mut layout = RingLayout::new(UNIFORM_RING_BYTES, 256);
        let first = layout.allocate(0, 64).expect("the first slot");
        let second = layout.allocate(1, 64).expect("the second slot");
        layout.reset(0);
        assert_eq!(layout.allocate(0, 64).expect("the reset slot"), first);
        assert_ne!(layout.allocate(1, 64).expect("the second slot"), second);
    }

    /// A slot that does not hold the request is refused.
    #[test]
    fn a_slot_that_does_not_fit_is_refused() {
        let mut layout = RingLayout::new(1024, 256);
        let error = layout.allocate(0, 2048).expect_err("the slot is too small");
        assert!(matches!(error, RenderError::OutOfMemory { .. }));
    }

    /// The ring holds a slot per frame in flight and one for the offscreen path.
    #[test]
    fn the_ring_holds_a_slot_per_frame_in_flight_and_one_for_the_offscreen_path() {
        let mut layout = RingLayout::new(UNIFORM_RING_BYTES, 256);
        let mut offsets = Vec::new();
        for slot in 0..RING_SLOTS {
            offsets.push(layout.allocate(slot, 64).expect("a slot"));
        }
        assert_eq!(offsets.len(), super::super::MAX_FRAMES_IN_FLIGHT + 1);
    }

    /// A cached set is reused, and the least recently used set leaves the
    /// cache when it is full.
    #[test]
    fn the_cache_reuses_a_set_and_evicts_the_least_recently_used_one() {
        let mut cache = SetCache::new(2);
        let first = vk::DescriptorSet::from_raw(1);
        let second = vk::DescriptorSet::from_raw(2);
        let third = vk::DescriptorSet::from_raw(3);
        cache.insert(&[1, 0, 0, 0], first, Some(0));
        cache.insert(&[2, 0, 0, 0], second, Some(0));
        assert!(cache.is_full());

        // The first set is used again, so the second one is the least
        // recently used one.
        assert_eq!(cache.take(&[1, 0, 0, 0], Some(0)), Some(first));
        assert!(cache.is_full());
        let evicted = cache.evict_lru().expect("a set leaves the cache");
        assert_eq!(evicted.set, second);

        // The cache takes the new set and hands it out again.
        cache.insert(&[3, 0, 0, 0], third, Some(0));
        assert_eq!(cache.take(&[3, 0, 0, 0], Some(0)), Some(third));
    }

    /// A set a live frame used waits for that frame's slot before it is
    /// freed.
    #[test]
    fn a_retired_set_waits_for_the_frame_that_used_it() {
        let live = RetiredSet {
            pool: vk::DescriptorPool::null(),
            set: vk::DescriptorSet::from_raw(1),
            last_live_slot: Some(0),
        };
        assert!(live.retirable(0), "the frame of slot 0 has completed");
        assert!(!live.retirable(1), "the frame of slot 1 may be in flight");

        // A set no live frame used was bound by an offscreen render, which
        // completed before the render returned.
        let offscreen = RetiredSet {
            pool: vk::DescriptorPool::null(),
            set: vk::DescriptorSet::from_raw(2),
            last_live_slot: None,
        };
        assert!(offscreen.retirable(0));
        assert!(offscreen.retirable(1));
    }
}
