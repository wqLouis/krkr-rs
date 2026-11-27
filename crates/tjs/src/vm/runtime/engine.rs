//! Running one compiled script: the text in, the top-level call out.
//!
//! The entry below is the one a host calls instead of the engine's own
//! execute entry: it compiles the text with the front end, builds the block
//! the image describes, and runs the block's top-level context against the
//! engine's global object. The object that context reaches is a
//! [`CHandle`], so a script that creates an engine object, reads its members
//! and calls it is running in the Rust executor while the values are still
//! the engine's.
//!
//! # The receiver and the global
//!
//! The top-level call's receiver is the block's own top-level context object,
//! with the engine's global object behind it. The context is where a declared
//! function is registered, so the member registration that publishes the
//! block's functions has somewhere to write: an engine object, which is what
//! the global is, cannot hold a function value of the Rust executor.
//!
//! The engine's global object stays the machine's global. It is what the
//! `global` keyword names, so a member a script writes through that keyword
//! lives where the engine's global lives and outlives the call.
//!
//! # What the entry does not finish
//!
//! A block's declared function is a context object, and invoking one is the
//! interpreter's frame entry, which the object surface does not serve; a call
//! that names one raises `NotCallable`. The entry runs a script whose
//! top-level code, member accesses, operators and engine calls are live; a
//! function the script declares and then calls is the one shape it cannot
//! finish.

use std::rc::Rc;

use crate::Tjs2Engine;
use crate::vm::runtime::bridge::CHandle;
use crate::vm::runtime::exec::build::{Block, BuildError};
use crate::vm::runtime::exec::{Fault, Machine, ObjThisContext};
use crate::vm::runtime::object::{Dispatch, ObjectError};
use crate::vm::runtime::variant::Variant;
use crate::vm::{ParseError, compile};

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
    let image = compile(script, name)?;
    let block = Block::build(&image)?;
    let global_id = engine.global_object().map_err(EngineError::Global)?;
    let global: Rc<dyn Dispatch> = Rc::new(CHandle::new(engine, global_id)?);
    let top_level = block
        .top_level()
        .ok_or(BuildError::TopLevel { index: -1 })?;
    let context = top_level.object().clone();
    let receiver: Rc<dyn Dispatch> =
        Rc::new(ObjThisContext::new(context.clone(), Some(global.clone())));
    let mut machine = Machine::with_global(global);
    Ok(machine.run_context(&context, Some(receiver), &[])?)
}
