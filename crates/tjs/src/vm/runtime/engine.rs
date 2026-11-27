//! Running one compiled script: the text in, the top-level call out.
//!
//! The entries below are the ones a host calls instead of the engine's own
//! execute and evaluate entries: each compiles the text with the front end,
//! builds the block the image describes, and runs the block's top-level
//! context against the engine's global object. The object that context
//! reaches is a [`CHandle`], so a script that creates an engine object, reads
//! its members and calls it is running in the Rust executor while the values
//! are still the engine's.
//!
//! # The receiver and the global
//!
//! The top-level call's receiver is the engine's global object itself, which is
//! also the machine's global. A top-level script runs in the global scope, so a
//! `var` the script declares and a `function` it declares are both properties
//! of that one object and outlive the call. The block's top-level context is
//! the code's own structure; it does not receive the script's members.
//!
//! # Why the global handle carries a store
//!
//! The engine's global can hold a scalar, an octet and an engine object, but it
//! cannot hold a context object of the Rust object model. A declared function is
//! exactly such an object. The global handle therefore carries a
//! [`GlobalMembers`] store: a write of a Rust-owned object stays there, while
//! every value the engine can hold is written to the engine's global. A read
//! checks the store first and then the engine, so the script sees one global
//! object in both cases. The store is shared across calls, which is what keeps
//! a function declared by one script callable by the next.

use std::rc::Rc;

use crate::Tjs2Engine;
use crate::vm::runtime::bridge::{CHandle, SharedGlobals, global_members};
use crate::vm::runtime::exec::build::{Block, BuildError};
use crate::vm::runtime::exec::throw::FrameTrace;
use crate::vm::runtime::exec::{Fault, Machine};
use crate::vm::runtime::object::{Dispatch, ObjectError};
use crate::vm::runtime::variant::Variant;
use crate::vm::{ParseError, compile, compile_expression};

/// The reason a script did not run.
///
/// Every branch carries the typed failure its own part returned: the front
/// end's compile failure, the builder's, a code fault, or an object the
/// executor reached refusing an entry.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EngineError {
    /// The script text does not compile.
    #[error(transparent)]
    Compile(#[from] ParseError),
    /// The compiled image does not describe a block.
    #[error(transparent)]
    Build(#[from] BuildError),
    /// The block's top-level code raised or reached an instruction this
    /// interpreter does not run.
    #[error(transparent)]
    Run(#[from] Fault),
    /// An object the code reached refused the entry.
    #[error(transparent)]
    Object(#[from] ObjectError),
    /// The engine did not hand out its global object.
    #[error("the engine has no global object: {0}")]
    Global(String),
}

/// A script failure and the frames the code passed before it surfaced.
///
/// The trace is innermost first. A failure raised before any code ran — a
/// compile, a block build, or the global object's own refusal — has an empty
/// trace.
#[derive(Debug)]
pub struct ScriptFailure {
    /// The executor failure.
    pub error: EngineError,
    /// The frames the code passed, innermost first.
    pub trace: Vec<FrameTrace>,
}

impl ScriptFailure {
    /// A failure that has not passed a frame.
    fn before_run(error: EngineError) -> Self {
        Self {
            error,
            trace: Vec::new(),
        }
    }
}

/// Compiles `script` and runs its top-level context.
///
/// `name` labels the block, as it does in [`compile`]. The answer is what the
/// top-level code stored as its result: the empty variant for a script whose
/// top code falls off its end, and the value of a top-level `return`.
///
/// # Errors
///
/// The compile's failure, the builder's, the code's, or an object entry's; no
/// step substitutes a value for a failure.
pub fn run(engine: Rc<Tjs2Engine>, script: &str, name: &str) -> Result<Variant, EngineError> {
    run_with_members(engine, script, name, global_members()).map_err(|failure| failure.error)
}

/// Compiles `script` and runs its top-level context with a shared global.
///
/// The global is the handle the machine uses and the receiver a top-level
/// context runs with. The members the engine cannot hold are kept in `globals`,
/// so a top-level function outlives the call that declared it.
///
/// # Errors
///
/// The compile's failure, the builder's, the code's, or an object entry's; a
/// code failure carries the frames it passed.
pub fn run_with_members(
    engine: Rc<Tjs2Engine>,
    script: &str,
    name: &str,
    globals: SharedGlobals,
) -> Result<Variant, ScriptFailure> {
    let image = compile(script, name).map_err(|error| ScriptFailure::before_run(error.into()))?;
    let block = Block::build(&image).map_err(|error| ScriptFailure::before_run(error.into()))?;
    run_block(engine, block, globals)
}

/// Compiles `expression` and runs its top-level context.
///
/// `name` labels the block, as it does in [`compile`]. The answer is the value
/// the expression produced.
///
/// # Errors
///
/// The compile's failure, the builder's, the code's, or an object entry's; no
/// step substitutes a value for a failure.
pub fn eval(engine: Rc<Tjs2Engine>, expression: &str, name: &str) -> Result<Variant, EngineError> {
    eval_with_members(engine, expression, name, global_members()).map_err(|failure| failure.error)
}

/// Compiles `expression` and runs it with a shared global.
///
/// # Errors
///
/// The compile's failure, the builder's, the code's, or an object entry's; a
/// code failure carries the frames it passed.
pub fn eval_with_members(
    engine: Rc<Tjs2Engine>,
    expression: &str,
    name: &str,
    globals: SharedGlobals,
) -> Result<Variant, ScriptFailure> {
    let image = compile_expression(expression, name)
        .map_err(|error| ScriptFailure::before_run(error.into()))?;
    let block = Block::build(&image).map_err(|error| ScriptFailure::before_run(error.into()))?;
    run_block(engine, block, globals)
}

/// Runs a built block's top-level context against the engine's global object.
fn run_block(
    engine: Rc<Tjs2Engine>,
    block: Block,
    globals: SharedGlobals,
) -> Result<Variant, ScriptFailure> {
    let global_id = engine
        .global_object()
        .map_err(|error| ScriptFailure::before_run(EngineError::Global(error)))?;
    let global: Rc<dyn Dispatch> = Rc::new(
        CHandle::with_members(engine, global_id, globals)
            .map_err(|error| ScriptFailure::before_run(EngineError::Object(error)))?,
    );
    let top_level = block
        .top_level()
        .ok_or(BuildError::TopLevel { index: -1 })
        .map_err(|error| ScriptFailure::before_run(EngineError::Build(error)))?;
    let context = top_level.object().clone();
    let mut machine = Machine::with_global(global);
    // A top-level context handed no receiver runs with the machine's global
    // object, which is the scope a script's own body runs in.
    match machine.run_context_traced(&context, None, &[]) {
        Ok(answer) => Ok(answer),
        Err(unwind) => {
            let trace = unwind.trace().to_vec();
            Err(ScriptFailure {
                error: EngineError::Run(unwind.fault()),
                trace,
            })
        }
    }
}
