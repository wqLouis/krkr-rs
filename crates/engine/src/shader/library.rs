//! The registry of registered shader programs and their cached pipelines.

use api::blend::BlendMode;
use render::{
    EffectPipeline, RenderPipeline, RenderState, create_effect_pipeline, destroy_effect_pipeline,
};

use crate::shader::{LoadedProgram, PipelineId, ProgramDesc, ShaderError, ShaderId};

/// One pipeline the library created, with the pair it was created for.
#[derive(Debug)]
struct LoadedPipeline {
    /// Library identity of the pipeline.
    id: PipelineId,
    /// Program the pipeline was built from.
    shader: ShaderId,
    /// Blend the pipeline was built with.
    blend: BlendMode,
    /// The backend's handle.
    handle: EffectPipeline,
}

/// The shader programs a host has registered, and the pipelines built from them.
///
/// The library owns the programs (it allocates their ids, keeps an owned copy of
/// their stage code and answers lookups by id) and the pipelines it created from
/// them (one per `(program, blend)` pair, cached so a repeating draw never
/// rebuilds one). It holds no other backend object: every call names the
/// [`RenderPipeline`] and [`RenderState`] explicitly, like the rest of the
/// middle layer, so the library is plain state and works on a headless host.
///
/// Registration is idempotent: loading a descriptor identical to a registered
/// one returns the id already issued, so a startup path that runs twice does not
/// grow the table. A sequence of different programs grows it in load order.
#[derive(Debug, Default)]
pub struct ShaderLibrary {
    /// The id the next registration receives. Monotonic, never reused.
    next_shader: u64,
    /// The id the next pipeline receives. Monotonic, never reused.
    next_pipeline: u64,
    /// Registered programs in load order.
    programs: Vec<LoadedProgram>,
    /// Created pipelines in creation order.
    pipelines: Vec<LoadedPipeline>,
}

impl ShaderLibrary {
    /// An empty library.
    pub fn new() -> Self {
        Self::default()
    }

    /// Validates `desc` and registers it, returning its id.
    ///
    /// An identical registration returns the id already issued for it.
    ///
    /// # Errors
    ///
    /// Returns [`ShaderError::Data`] with the data-level refusal when the
    /// descriptor's stages, bindings or uniform budgets are not usable.
    pub fn load(&mut self, desc: ProgramDesc<'_>) -> Result<ShaderId, ShaderError> {
        desc.validate()?;
        if let Some(program) = self.programs.iter().find(|program| program.matches(&desc)) {
            return Ok(program.id());
        }
        let id = ShaderId::from_raw(self.next_shader);
        self.next_shader = self.next_shader.saturating_add(1);
        self.programs.push(LoadedProgram::register(id, &desc));
        Ok(id)
    }

    /// The registered program `id` names, or `None` when the library never
    /// issued it (or it has been unloaded).
    pub fn program(&self, id: ShaderId) -> Option<&LoadedProgram> {
        self.programs.iter().find(|program| program.id() == id)
    }

    /// Every registered program, in load order.
    pub fn programs(&self) -> impl Iterator<Item = &LoadedProgram> {
        self.programs.iter()
    }

    /// Whether `id` names a registered program.
    pub fn contains(&self, id: ShaderId) -> bool {
        self.program(id).is_some()
    }

    /// The pipeline for `shader` built with `blend`, created on first use.
    ///
    /// A pipeline is identified by its `(program, blend)` pair: the blend is
    /// part of the backend's pipeline object, so the same program drawn with
    /// two blends needs two pipelines and returns two ids.
    ///
    /// # Errors
    ///
    /// Returns [`ShaderError::UnknownShader`] for an id this library never
    /// issued, [`ShaderError::Unsupported`] when the backend cannot build
    /// effect pipelines at all (the headless backend), and
    /// [`ShaderError::Render`] when the descriptor or the module is rejected by
    /// the backend.
    pub fn pipeline(
        &mut self,
        render: &RenderPipeline,
        state: &mut RenderState,
        shader: ShaderId,
        blend: BlendMode,
    ) -> Result<PipelineId, ShaderError> {
        if let Some(existing) = self
            .pipelines
            .iter()
            .find(|pipeline| pipeline.shader == shader && pipeline.blend == blend)
        {
            return Ok(existing.id);
        }
        let index = self
            .programs
            .iter()
            .position(|program| program.id() == shader)
            .ok_or(ShaderError::UnknownShader(shader))?;
        let handle = {
            let desc = self.programs[index].pipeline_desc(blend);
            create_effect_pipeline(render, state, &desc).map_err(ShaderError::from_render)?
        };
        let id = PipelineId::from_raw(self.next_pipeline);
        self.next_pipeline = self.next_pipeline.saturating_add(1);
        self.pipelines.push(LoadedPipeline {
            id,
            shader,
            blend,
            handle,
        });
        Ok(id)
    }

    /// Every created pipeline, as `(id, program, blend)`, in creation order.
    pub fn pipelines(&self) -> impl Iterator<Item = (PipelineId, ShaderId, BlendMode)> {
        self.pipelines
            .iter()
            .map(|pipeline| (pipeline.id, pipeline.shader, pipeline.blend))
    }

    /// The backend handle of `pipeline`, or `None` when the library never
    /// issued that id (or has released it).
    ///
    /// The handle is the object a draw binds; the library keeps it so a
    /// caller does not have to ask the backend twice.
    pub fn pipeline_handle(&self, pipeline: PipelineId) -> Option<&EffectPipeline> {
        self.pipelines
            .iter()
            .find(|entry| entry.id == pipeline)
            .map(|entry| &entry.handle)
    }

    /// The number of created pipelines.
    pub fn pipeline_count(&self) -> usize {
        self.pipelines.len()
    }

    /// Releases one cached pipeline.
    ///
    /// A projected scene may still name the handle for the frame in flight; the
    /// backend skips such a draw and logs it, exactly as it does for a released
    /// texture.
    ///
    /// # Errors
    ///
    /// Returns [`ShaderError::UnknownPipeline`] when the library never issued
    /// `pipeline`.
    pub fn release_pipeline(
        &mut self,
        render: &RenderPipeline,
        state: &mut RenderState,
        pipeline: PipelineId,
    ) -> Result<(), ShaderError> {
        let index = self
            .pipelines
            .iter()
            .position(|entry| entry.id == pipeline)
            .ok_or(ShaderError::UnknownPipeline(pipeline))?;
        let removed = self.pipelines.remove(index);
        destroy_effect_pipeline(render, state, removed.handle);
        Ok(())
    }

    /// Unregisters `id`, releasing the pipelines built from it first.
    ///
    /// The id is not reused: a later registration of the same descriptor
    /// receives a fresh one.
    ///
    /// # Errors
    ///
    /// Returns [`ShaderError::UnknownShader`] when the library never issued
    /// `id`.
    pub fn unload(
        &mut self,
        render: &RenderPipeline,
        state: &mut RenderState,
        id: ShaderId,
    ) -> Result<LoadedProgram, ShaderError> {
        let index = self
            .programs
            .iter()
            .position(|program| program.id() == id)
            .ok_or(ShaderError::UnknownShader(id))?;
        // The pipelines must go before the stage code they were built from.
        let mut kept = Vec::with_capacity(self.pipelines.len());
        for pipeline in std::mem::take(&mut self.pipelines) {
            if pipeline.shader == id {
                destroy_effect_pipeline(render, state, pipeline.handle);
            } else {
                kept.push(pipeline);
            }
        }
        self.pipelines = kept;
        Ok(self.programs.remove(index))
    }

    /// Unregisters every program, releasing every pipeline first. Ids issued
    /// before the call stay unused.
    pub fn clear(&mut self, render: &RenderPipeline, state: &mut RenderState) {
        for pipeline in std::mem::take(&mut self.pipelines) {
            destroy_effect_pipeline(render, state, pipeline.handle);
        }
        self.programs.clear();
    }

    /// The number of registered programs.
    pub fn len(&self) -> usize {
        self.programs.len()
    }

    /// Whether no program is registered.
    pub fn is_empty(&self) -> bool {
        self.programs.is_empty()
    }
}
