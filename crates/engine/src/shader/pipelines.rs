//! The effect pipelines the programs are drawn with.
//!
//! A pipeline is one program built for one blend mode; the shader library
//! creates it and owns it. This table is the scene's copy of the handles: the
//! projection binds a program's pipeline through it, and the transition tick
//! asks it whether the backend can draw the program at all.
//!
//! A program the table does not name has no pipeline in this build — the
//! backend cannot run effect pipelines — and the transition that needs it
//! blends on the CPU instead.

use std::collections::HashMap;

use render::EffectPipeline;

use crate::shader::Program;

/// The backend handles of the programs the scene can draw.
#[derive(Debug, Default)]
pub struct EffectPipelines {
    /// The pipeline of each program that has one.
    handles: HashMap<Program, EffectPipeline>,
}

impl EffectPipelines {
    /// An empty table: no program has a pipeline.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the backend handle of `program`'s pipeline.
    pub fn insert(&mut self, program: Program, handle: EffectPipeline) {
        self.handles.insert(program, handle);
    }

    /// The backend handle of `program`'s pipeline, or `None` when the program
    /// has none.
    pub fn handle(&self, program: Program) -> Option<&EffectPipeline> {
        self.handles.get(&program)
    }

    /// Whether `program` has a pipeline this build can draw.
    pub fn contains(&self, program: Program) -> bool {
        self.handles.contains_key(&program)
    }

    /// The number of programs with a pipeline.
    pub fn len(&self) -> usize {
        self.handles.len()
    }

    /// Whether no program has a pipeline.
    pub fn is_empty(&self) -> bool {
        self.handles.is_empty()
    }
}
