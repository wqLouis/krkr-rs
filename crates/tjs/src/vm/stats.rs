//! The front end's own analysis of the compiles it has done.
//!
//! The three columns interleave: the lexer produces the tokens, the parser
//! reads them and drives the generator, and the generator writes the code.
//! The phases cannot be timed from the outside, so each column times its own
//! work where the work happens, and the totals are kept here.
//!
//! The measurement is off by default. A corpus is hundreds of thousands of
//! tokens, and a clock read per token costs tens of milliseconds, so a run
//! that does not ask for the analysis pays one relaxed atomic load per token
//! and nothing else.
//!
//! The counters are process-global: the VM is single-threaded and keeps its
//! state in globals.

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::vm::codegen::CodegenError;
use crate::vm::codegen::context::ContextType;
use crate::vm::codegen::node::Node;
use crate::vm::parser::Commands;

/// Whether the front end measures itself.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// The number of compiles.
static COMPILES: AtomicU64 = AtomicU64::new(0);

/// The number of script code units the compiles read.
static CODE_UNITS: AtomicU64 = AtomicU64::new(0);

/// The number of script bytes the compiles read.
static BYTES: AtomicU64 = AtomicU64::new(0);

/// The time the whole compiles took, in nanoseconds.
static TOTAL_NS: AtomicU64 = AtomicU64::new(0);

/// The time the lexer spent on its tokens, in nanoseconds.
static LEXER_NS: AtomicU64 = AtomicU64::new(0);

/// The time the parser spent on its own work, in nanoseconds.
static PARSER_NS: AtomicU64 = AtomicU64::new(0);

/// The time the generator spent on its own work, in nanoseconds.
static GENERATOR_NS: AtomicU64 = AtomicU64::new(0);

/// The time the image builds took, in nanoseconds.
static IMAGE_NS: AtomicU64 = AtomicU64::new(0);

/// The front end's analysis of the compiles it has done.
///
/// The times are the measured wall-clock costs of the whole run: the compile
/// as a whole, and the three columns and the image build inside it. The
/// columns are measured where their work happens, and the parser's own time
/// is what the parse window holds besides the two timed columns.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CompileStats {
    /// The number of compiles.
    pub compiles: u64,
    /// The number of script code units the compiles read.
    pub code_units: u64,
    /// The number of script bytes the compiles read.
    pub bytes: u64,
    /// The time the whole compiles took, in milliseconds.
    pub total_ms: f64,
    /// The time the lexer spent on its tokens, in milliseconds.
    pub lexer_ms: f64,
    /// The time the parser spent on its own work, in milliseconds.
    pub parser_ms: f64,
    /// The time the generator spent on its own work, in milliseconds.
    pub generator_ms: f64,
    /// The time the image builds took, in milliseconds.
    pub image_ms: f64,
}

/// Switches the front end's measurement on or off.
///
/// The measurement is off by default, and a run that leaves it off pays one
/// relaxed atomic load per token and nothing else. Switching it on starts no
/// new count: the totals are the compiles the process has done, whether they
/// were measured or not.
pub fn set_compile_stats_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// Whether the front end measures itself.
pub fn compile_stats_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// The front end's analysis of the compiles it has done.
///
/// The snapshot is the process's totals: the number of compiles, the size of
/// the scripts they read, and the time the compiles and their phases took.
/// Every number is measured; nothing here is estimated.
pub fn compile_stats() -> CompileStats {
    CompileStats {
        compiles: COMPILES.load(Ordering::Relaxed),
        code_units: CODE_UNITS.load(Ordering::Relaxed),
        bytes: BYTES.load(Ordering::Relaxed),
        total_ms: millis(TOTAL_NS.load(Ordering::Relaxed)),
        lexer_ms: millis(LEXER_NS.load(Ordering::Relaxed)),
        parser_ms: millis(PARSER_NS.load(Ordering::Relaxed)),
        generator_ms: millis(GENERATOR_NS.load(Ordering::Relaxed)),
        image_ms: millis(IMAGE_NS.load(Ordering::Relaxed)),
    }
}

/// The milliseconds of a nanosecond count.
fn millis(nanos: u64) -> f64 {
    nanos as f64 / 1_000_000.0
}

/// The nanoseconds of a duration, saturating at the largest count.
fn nanos(duration: Duration) -> u64 {
    duration
        .as_secs()
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::from(duration.subsec_nanos()))
}

/// Times the work and adds its time to the lexer's total.
///
/// The measurement is off by default, and the check is one relaxed atomic
/// load: a token that is not measured costs that load and nothing else.
pub(crate) fn timed_lexer<R>(work: impl FnOnce() -> R) -> R {
    if !compile_stats_enabled() {
        return work();
    }
    let started = Instant::now();
    let result = work();
    LEXER_NS.fetch_add(nanos(started.elapsed()), Ordering::Relaxed);
    result
}

/// Times the work and adds its time to the generator's total.
///
/// The measurement is off by default, and the check is one relaxed atomic
/// load: a command that is not measured costs that load and nothing else.
pub(crate) fn timed_generator<R>(work: impl FnOnce() -> R) -> R {
    if !compile_stats_enabled() {
        return work();
    }
    let started = Instant::now();
    let result = work();
    GENERATOR_NS.fetch_add(nanos(started.elapsed()), Ordering::Relaxed);
    result
}

/// The commands the parser drives, with the time every call costs.
///
/// The generator's work is what the calls into it cost, and the parser
/// drives the generator through the [`Commands`] trait, so the trait is
/// where the generator's time is measured. The wrapper forwards every call
/// and adds the time the call took to the generator's total.
pub(crate) struct Timed<C> {
    /// The commands the parser drives.
    commands: C,
}

impl<C> Timed<C> {
    /// Wraps the commands the parser drives.
    pub(crate) fn new(commands: C) -> Self {
        Self { commands }
    }
}

/// Implements the trait for the wrapper: every call is timed, and the
/// generator's total takes the time the call cost.
///
/// The methods are the trait's, one by one. The compiler checks the list, so
/// a method the trait gains and the wrapper does not is a build error.
macro_rules! timed_commands {
    ($($name:ident ( $($arg:ident : $arg_ty:ty),* ) $(-> $ret:ty)?;)*) => {
        impl<C: Commands> Commands for Timed<C> {
            $(
                fn $name(&mut self, $($arg: $arg_ty),*) $(-> $ret)? {
                    crate::vm::stats::timed_generator(|| self.commands.$name($($arg),*))
                }
            )*
            /// The parser tells the generator where the analyser is. The
            /// telling is the parser's own work, and it happens once per
            /// token, so it is not measured.
            fn at_lex_position(&mut self, position: i32) {
                self.commands.at_lex_position(position);
            }
        }
    };
}

timed_commands! {
    create_expr_code(node: &Rc<Node>) -> Result<(), CodegenError>;
    add_local_variable(name: &str, init: i32);
    init_local_variable(name: &str, node: &Rc<Node>) -> Result<(), CodegenError>;
    add_function_decl_arg(name: &str, node: Option<&Rc<Node>>) -> Result<(), CodegenError>;
    add_function_decl_arg_collapse(name: Option<&str>);
    set_property_decl_arg(name: &str);
    push_context(name: Option<&[u16]>, kind: ContextType) -> usize;
    pop_context() -> Result<(), CodegenError>;
    enter_if_code();
    create_if_expr_code(node: Option<&Rc<Node>>) -> Result<(), CodegenError>;
    exit_if_code() -> Result<(), CodegenError>;
    enter_else_code();
    exit_else_code() -> Result<(), CodegenError>;
    enter_while_code(do_while: bool);
    create_while_expr_code(node: Option<&Rc<Node>>, do_while: bool) -> Result<(), CodegenError>;
    exit_while_code(do_while: bool) -> Result<(), CodegenError>;
    enter_for_code();
    create_for_expr_code(node: Option<&Rc<Node>>) -> Result<(), CodegenError>;
    set_for_third_expr_code(node: Option<&Rc<Node>>) -> Result<(), CodegenError>;
    exit_for_code() -> Result<(), CodegenError>;
    enter_switch_code(node: Option<&Rc<Node>>) -> Result<(), CodegenError>;
    exit_switch_code() -> Result<(), CodegenError>;
    process_case_code(node: Option<&Rc<Node>>) -> Result<(), CodegenError>;
    enter_with_code(node: Option<&Rc<Node>>) -> Result<(), CodegenError>;
    exit_with_code() -> Result<(), CodegenError>;
    enter_try_code();
    enter_catch_code(name: Option<&str>) -> Result<(), CodegenError>;
    exit_try_code() -> Result<(), CodegenError>;
    process_throw_code(node: Option<&Rc<Node>>) -> Result<(), CodegenError>;
    do_break() -> Result<(), CodegenError>;
    do_continue() -> Result<(), CodegenError>;
    do_debugger();
    return_from_func(node: Option<&Rc<Node>>) -> Result<(), CodegenError>;
    create_extends_expr_code(node: Option<&Rc<Node>>, hold: bool) -> Result<(), CodegenError>;
    enter_block();
    exit_block();
}

/// The measurement of one compile.
///
/// The compile entry holds one while it compiles. It carries the instant the
/// compile started and the counters the two timed columns wrote into, so the
/// compile's own phases are told apart from the ones of the compiles before
/// it.
pub(crate) struct Measurement {
    /// The instant the compile started, when the measurement is on.
    started: Option<Instant>,
    /// The lexer's counter when the compile started.
    lexer_before: u64,
    /// The generator's counter when the compile started.
    generator_before: u64,
}

impl Measurement {
    /// Starts the measurement of one compile.
    ///
    /// A measurement that is off keeps no clock and reads no counter, so the
    /// compile pays nothing for it.
    pub(crate) fn start() -> Self {
        if !compile_stats_enabled() {
            return Self {
                started: None,
                lexer_before: 0,
                generator_before: 0,
            };
        }
        Self {
            started: Some(Instant::now()),
            lexer_before: LEXER_NS.load(Ordering::Relaxed),
            generator_before: GENERATOR_NS.load(Ordering::Relaxed),
        }
    }

    /// The nanoseconds since the measurement started.
    pub(crate) fn elapsed_ns(&self) -> u64 {
        match self.started {
            Some(started) => nanos(started.elapsed()),
            None => 0,
        }
    }

    /// Times the work and returns it with the nanoseconds it took.
    pub(crate) fn timed<R>(&self, work: impl FnOnce() -> R) -> (R, u64) {
        if self.started.is_none() {
            return (work(), 0);
        }
        let started = Instant::now();
        let result = work();
        (result, nanos(started.elapsed()))
    }

    /// Records the compile's phases and adds them to the totals.
    ///
    /// The lexer's and the generator's times are already in the totals: the
    /// two columns wrote them there as they worked. The parser's own time is
    /// what the parse window holds besides the two, and the whole compile's
    /// time and the image build's time are added here.
    pub(crate) fn finish(self, total_ns: u64, parse_ns: u64, image_ns: u64, code_units: u64) {
        if self.started.is_none() {
            return;
        }
        let lexer = LEXER_NS
            .load(Ordering::Relaxed)
            .saturating_sub(self.lexer_before);
        let generator = GENERATOR_NS
            .load(Ordering::Relaxed)
            .saturating_sub(self.generator_before);
        // The parse window holds the two timed columns and the parser's own
        // work; what is left of it is the parser's.
        let parser = parse_ns.saturating_sub(lexer).saturating_sub(generator);
        COMPILES.fetch_add(1, Ordering::Relaxed);
        CODE_UNITS.fetch_add(code_units, Ordering::Relaxed);
        BYTES.fetch_add(code_units.saturating_mul(2), Ordering::Relaxed);
        TOTAL_NS.fetch_add(total_ns, Ordering::Relaxed);
        PARSER_NS.fetch_add(parser, Ordering::Relaxed);
        IMAGE_NS.fetch_add(image_ns, Ordering::Relaxed);
    }
}
