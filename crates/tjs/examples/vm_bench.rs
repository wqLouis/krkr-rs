//! The interpreter on both executors: one corpus, three rows per script.
//!
//! A script reaches an engine through one entry, text in and a call out, and
//! the tree has two executors over that entry: the vendored tree under
//! `crates/tjs/cpp` and the Rust runtime under `crates/tjs/src/vm/runtime`.
//! Both compile the same text with the same Rust front end, so what is left to
//! differ is the interpreter — the frame, the instruction decode, the opcodes,
//! the calls, the contexts and the exceptions. This harness drives the same
//! corpus through both and prints the two sides' nanoseconds beside their ratio
//! (the Rust side over the vendored side, so below one is a win for the Rust
//! executor).
//!
//! Nothing outside the interpreter is measured: no storage, no image decode, no
//! class registration, no engine service and no game's data. One native call
//! (`Math.floor`) sits in the arithmetic row, because a game's arithmetic does
//! not run without natives; that row is labelled `arith+native` so it is never
//! read as a pure-interpreter row.
//!
//! # The corpus
//!
//! Eight `const` scripts, each declaring a function called `vmTarget` whose
//! body is a loop of ten thousand iterations, so one call runs for milliseconds
//! and the clock, the frame the call lays out and the host call around it are
//! not what a row reports: integer and real arithmetic (the one native call is
//! here), comparison and the flag rules (`if`, `&&`, `||`, `<`, `<=`, `>=`),
//! text concatenation and comparison, an array index read and write, a
//! dictionary key read and write, a small function called in the loop, a
//! property written and read on an object, and a `try`/`catch` around one throw
//! per iteration.
//!
//! # The three rows
//!
//! * `compile` — the front end alone, through `tjs::vm::compile`. Both
//!   executors compile with the same Rust front end, so this row is the
//!   harness's own control: it is the same call on both sides, and a ratio that
//!   is not 1.0 there is the harness and not an executor.
//! * `cold` — one call of the entry on a fresh engine: compile the block, build
//!   it and run the script, which declares `vmTarget`, then call the compiled
//!   body once.
//! * `hot` — the compiled body called `HOT_CALLS` times on the engine that
//!   already ran the script. Neither side compiles here: the row is the
//!   execution.
//!
//! # Why the hot row calls the body and not the entry again
//!
//! The Rust engine keeps the block it compiled, filed under the block's name
//! and the text it was compiled from, so a second `run` of the same text is
//! answered from that block and compiles nothing. The vendored script entry
//! keeps no such block: every call binds a new block and compiles the text
//! again. Re-entering the entry for the hot row would therefore put a compile
//! into the vendored side only, and the two numbers would no longer be the same
//! quantity. Both sides compile once instead, and the row calls the compiled
//! body; the harness prints both compile counts at the end of a run, so the
//! difference is read off the run itself rather than taken on trust.
//!
//! # The command
//!
//! ```sh
//! cargo run --release -p tjs --example vm_bench
//! ```
//!
//! The profile is part of the measurement — the vendored tree's optimisation
//! level follows it, and `TJS2_OPT` overrides it — so only a release build
//! gives numbers worth keeping. The harness opens no window, needs no display
//! and no GPU: it is a measurement.
//!
//! # The method
//!
//! Both sides are timed in turn inside every repetition, so a machine that
//! drifts moves both sides together. Each row discards one warm-up pass per
//! side and then reports the median of `REPETITIONS` measured passes. A number
//! is nanoseconds per measured unit: one compile, one cold entry, one call of
//! the compiled body. Every table is printed beside the machine's load average
//! — a load above about four makes a number a report of the machine and not of
//! the interpreter — and the two sides' answers are compared, because a row
//! whose sides did different work is not a measurement of one script.

use std::fmt;
use std::hint::black_box;
use std::process::ExitCode;
use std::time::Instant;

use tjs::MemberRead;
use tjs::Tjs2Engine;
use tjs::TjsValue;
use tjs::vm::compile;
use tjs::vm::runtime::engine_core::EngineCore;
use tjs::vm::runtime::retained::RetainedId;
use tjs::vm::runtime::string::TjsString;
use tjs::vm::runtime::variant::Variant;

/// The function every corpus script declares; calling it is the measured work.
const TARGET: &str = "vmTarget";

/// The name both sides file the corpus block under, so a second run of the same
/// text is the same block and not a new one.
const BLOCK: &str = "vm_bench";

/// The measured passes one row takes, whose median is reported.
const REPETITIONS: usize = 7;

/// The body calls one hot pass makes; the row reports their mean.
const HOT_CALLS: usize = 5;

/// The script the block-cache check runs, small enough to compile twice without
/// costing anything.
const CACHE_CHECK: &str = "function vmCacheCheck() { return 1; }";

/// One script of the corpus.
struct Corpus {
    /// The name the table reports.
    name: &'static str,
    /// The text both sides compile and run.
    source: &'static str,
}

/// The eight scripts the interpreter is measured on.
const CORPUS: [Corpus; 8] = [
    Corpus {
        name: "arith+native",
        // An integer accumulator, a real one and one native call (`Math.floor`).
        // The label says the row includes a native call, so it is not read
        // beside the pure-interpreter rows.
        source: r#"function vmTarget() {
    var integers = 0;
    var reals = 0.5;
    var floors = 0;
    for (var i = 0; i < 10000; i++) {
        integers = integers + i * 3 - (i % 7);
        reals = reals + i * 0.25;
        floors = floors + Math.floor(i * 0.5);
    }
    return integers + reals + floors;
}"#,
    },
    Corpus {
        name: "compare",
        // The flag rules: `if`, short-circuiting `&&` and `||`, and the four
        // ordering comparisons.
        source: r#"function vmTarget() {
    var hits = 0;
    var misses = 0;
    for (var i = 0; i < 10000; i++) {
        if (i % 3 == 0 && i >= 1000 || i <= 10) {
            hits = hits + 1;
        }
        if (i < 5000 && i != 4000) {
            misses = misses + 1;
        }
        if (i > 9990 || i == 5) {
            misses = misses - 1;
        }
    }
    return hits * 1000 + misses;
}"#,
    },
    Corpus {
        name: "text",
        // One text built per iteration from a number, and two comparisons.
        source: r#"function vmTarget() {
    var hits = 0;
    var text = "";
    for (var i = 0; i < 10000; i++) {
        text = "item" + i;
        if (text == "item5000") { hits = hits + 1; }
        if (text == "item") { hits = hits - 1; }
    }
    return hits;
}"#,
    },
    Corpus {
        name: "array",
        // A literal array filled by index, then read back by index.
        source: r#"function vmTarget() {
    var values = [];
    var total = 0;
    for (var i = 0; i < 10000; i++) {
        values[i] = i * 2;
    }
    for (var j = 0; j < 10000; j++) {
        total = total + values[j];
    }
    return total;
}"#,
    },
    Corpus {
        name: "dictionary",
        // A literal dictionary, one key write and one key read per iteration,
        // over the 256 keys a game's table holds.
        source: r#"function vmTarget() {
    var table = %[];
    var total = 0;
    var key = "";
    for (var i = 0; i < 10000; i++) {
        key = "k" + (i % 256);
        table[key] = i;
        total = total + table[key];
    }
    return total;
}"#,
    },
    Corpus {
        name: "call",
        // One small function called in the loop: the call and the frame it lays
        // out, not the callee's body.
        source: r#"function bump(value) { return value + 1; }
function vmTarget() {
    var total = 0;
    for (var i = 0; i < 10000; i++) {
        total = total + bump(i);
    }
    return total;
}"#,
    },
    Corpus {
        name: "property",
        // A class instance's member written and read, the object built once.
        source: r#"class Node {
    var value;
}
function vmTarget() {
    var node = new Node();
    var total = 0;
    for (var i = 0; i < 10000; i++) {
        node.value = i;
        total = total + node.value;
    }
    return total;
}"#,
    },
    Corpus {
        name: "exception",
        // One protected region per iteration, entered and left by a throw.
        source: r#"function vmTarget() {
    var caught = 0;
    for (var i = 0; i < 10000; i++) {
        try {
            throw "boom";
        } catch (e) {
            caught = caught + 1;
        }
    }
    return caught;
}"#,
    },
];

/// One side's measurement of one unit: the nanoseconds it took and the number
/// the script answered (absent for the row that runs no script).
struct Sample {
    /// The nanoseconds one measured unit took.
    ns: f64,
    /// The number the script answered.
    answer: Option<f64>,
}

/// Whether the two sides of a row did the same work.
enum Work {
    /// Both sides answered the same number on every sample.
    Same,
    /// Both sides called the same front end, so there is one answer between
    /// them: the compile row's control.
    Control,
    /// The two sides answered different numbers; the row's ratio measures two
    /// different scripts.
    Differ {
        /// The number the vendored side answered.
        cpp: f64,
        /// The number the Rust side answered.
        rust: f64,
    },
}

impl fmt::Display for Work {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Same => formatter.write_str("same"),
            Self::Control => formatter.write_str("control"),
            Self::Differ { cpp, rust } => write!(formatter, "DIFFER {cpp} vs {rust}"),
        }
    }
}

/// One row of the table.
struct Row {
    /// The corpus script the row measures.
    item: &'static str,
    /// The row's phase: `compile`, `cold` or `hot`.
    phase: &'static str,
    /// The vendored executor's nanoseconds per measured unit.
    cpp_ns: f64,
    /// The Rust executor's nanoseconds per measured unit.
    rust_ns: f64,
    /// Whether the two sides did the same work.
    work: Work,
}

/// The command's entry point.
fn main() -> ExitCode {
    let mut rows = Vec::new();
    let mut failures = 0;

    report_load("before the timed rows");
    println!("# the interpreter on both executors: nanoseconds per measured unit");
    println!("# name\tcpp_ns\trust_ns\tratio");

    // a profiling run narrows the corpus: the exception row costs more than
    // every other row together on the vendored side, and its profile is
    // unwinding, not the interpreter
    let only: Option<Vec<String>> = std::env::var("KRKR_VM_BENCH_ONLY").ok().map(|names| {
        names
            .split(',')
            .map(|name| name.trim().to_owned())
            .collect()
    });
    for item in &CORPUS {
        if let Some(only) = &only
            && !only.iter().any(|name| name == item.name)
        {
            continue;
        }
        match measure(item) {
            Ok(measured) => {
                for row in &measured {
                    print_machine_row(row);
                }
                rows.extend(measured);
            }
            Err(error) => {
                failures += 1;
                let name = item.name;
                eprintln!("the corpus script `{name}` was not measured: {error}");
            }
        }
    }

    report_load("after the timed rows");
    println!("# the human summary follows");
    print_summary(&rows);
    println!();
    if let Err(error) = check_block_cache() {
        failures += 1;
        eprintln!("the block-cache check did not run: {error}");
    }
    report_load("after the whole run");

    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Measures one corpus script's three rows.
fn measure(item: &Corpus) -> Result<Vec<Row>, String> {
    let source = item.source;
    // the compile row's two sides are the same call: it is the control
    Ok(vec![
        time_row(
            item.name,
            "compile",
            || compile_once(source),
            || compile_once(source),
        )?,
        time_row(item.name, "cold", || cold_cpp(source), || cold_rust(source))?,
        time_row(item.name, "hot", || hot_cpp(source), || hot_rust(source))?,
    ])
}

/// Times both sides of one row and answers their medians.
///
/// One warm-up pass runs on each side before the measured passes, and the two
/// sides are timed in turn within a repetition so that a machine which drifts
/// moves both of them together.
fn time_row(
    item: &'static str,
    phase: &'static str,
    cpp: impl Fn() -> Result<Sample, String>,
    rust: impl Fn() -> Result<Sample, String>,
) -> Result<Row, String> {
    // one warm-up pass per side, discarded
    cpp()?;
    rust()?;
    let mut cpp_samples = Vec::with_capacity(REPETITIONS);
    let mut rust_samples = Vec::with_capacity(REPETITIONS);
    for _ in 0..REPETITIONS {
        cpp_samples.push(cpp()?);
        rust_samples.push(rust()?);
    }
    let work = work_of(&cpp_samples, &rust_samples);
    let mut cpp_times: Vec<f64> = cpp_samples.iter().map(|sample| sample.ns).collect();
    let mut rust_times: Vec<f64> = rust_samples.iter().map(|sample| sample.ns).collect();
    Ok(Row {
        item,
        phase,
        cpp_ns: median(&mut cpp_times),
        rust_ns: median(&mut rust_times),
        work,
    })
}

/// The middle of the samples, or the mean of the two middle ones.
fn median(samples: &mut [f64]) -> f64 {
    samples.sort_by(f64::total_cmp);
    let middle = samples.len() / 2;
    if samples.len() % 2 == 1 {
        samples[middle]
    } else {
        (samples[middle - 1] + samples[middle]) / 2.0
    }
}

/// Whether the two sides answered the same number.
fn work_of(cpp: &[Sample], rust: &[Sample]) -> Work {
    let Some(first) = cpp.iter().chain(rust).find_map(|sample| sample.answer) else {
        return Work::Control;
    };
    let agreed = cpp
        .iter()
        .chain(rust)
        .filter_map(|sample| sample.answer)
        .all(|answer| answer == first);
    if agreed {
        return Work::Same;
    }
    Work::Differ {
        cpp: cpp.iter().find_map(|sample| sample.answer).unwrap_or(first),
        rust: rust
            .iter()
            .find_map(|sample| sample.answer)
            .unwrap_or(first),
    }
}

/// One compile of the front end, timed.
fn compile_once(source: &str) -> Result<Sample, String> {
    let started = Instant::now();
    let image = compile(source, BLOCK).map_err(|error| error.to_string())?;
    // the image is the compile's whole output, so reading it here is what keeps
    // the compile inside the timed region
    black_box(&image);
    let ns = nanos(started);
    Ok(Sample { ns, answer: None })
}

/// One call of the vendored script entry on a fresh engine, timed.
fn cold_cpp(source: &str) -> Result<Sample, String> {
    let engine = Tjs2Engine::new().map_err(|error| error.to_string())?;
    let started = Instant::now();
    engine
        .exec_script(source, BLOCK)
        .map_err(|error| error.to_string())?;
    let function = cpp_target(&engine)?;
    let answer = engine
        .call_detached(&function, &[])
        .map_err(|error| error.to_string())?;
    let ns = nanos(started);
    Ok(Sample {
        ns,
        answer: Some(cpp_number(&answer)?),
    })
}

/// One call of the Rust script entry on a fresh engine, timed.
fn cold_rust(source: &str) -> Result<Sample, String> {
    let core = EngineCore::new();
    let started = Instant::now();
    core.run(source, BLOCK).map_err(|error| error.to_string())?;
    let function = rust_target(&core)?;
    let answer = core
        .call_value(function, &[])
        .map_err(|error| error.to_string())?;
    let ns = nanos(started);
    Ok(Sample {
        ns,
        answer: Some(rust_number(&answer)?),
    })
}

/// The compiled body called `HOT_CALLS` times on a warm vendored engine.
fn hot_cpp(source: &str) -> Result<Sample, String> {
    let engine = Tjs2Engine::new().map_err(|error| error.to_string())?;
    engine
        .exec_script(source, BLOCK)
        .map_err(|error| error.to_string())?;
    let function = cpp_target(&engine)?;
    // the first call is the answer every later call repeats, and it warms the
    // frame the body lays out
    let answer = engine
        .call_detached(&function, &[])
        .map_err(|error| error.to_string())?;
    let expected = cpp_number(&answer)?;
    let started = Instant::now();
    for _ in 0..HOT_CALLS {
        let answer = engine
            .call_detached(&function, &[])
            .map_err(|error| error.to_string())?;
        black_box(&answer);
    }
    let ns = nanos(started) / HOT_CALLS as f64;
    Ok(Sample {
        ns,
        answer: Some(expected),
    })
}

/// The compiled body called `HOT_CALLS` times on a warm Rust engine.
fn hot_rust(source: &str) -> Result<Sample, String> {
    let core = EngineCore::new();
    core.run(source, BLOCK).map_err(|error| error.to_string())?;
    let function = rust_target(&core)?;
    let answer = core
        .call_value(function, &[])
        .map_err(|error| error.to_string())?;
    let expected = rust_number(&answer)?;
    let started = Instant::now();
    for _ in 0..HOT_CALLS {
        let answer = core
            .call_value(function, &[])
            .map_err(|error| error.to_string())?;
        black_box(&answer);
    }
    let ns = nanos(started) / HOT_CALLS as f64;
    Ok(Sample {
        ns,
        answer: Some(expected),
    })
}

/// The function the vendored engine's global holds under `TARGET`, retained so
/// the hot row's call needs no member lookup.
fn cpp_target(engine: &Tjs2Engine) -> Result<tjs::DetachedValue, String> {
    let global = engine.global_object()?;
    match engine
        .member_read(global.raw_id(), TARGET)
        .map_err(|error| error.to_string())?
    {
        MemberRead::Object(function) => Ok(function),
        other => Err(format!("the global member is {other:?}, not a function")),
    }
}

/// The function the Rust engine's global holds under `TARGET`, retained so the
/// hot row's call needs no member lookup.
fn rust_target(core: &EngineCore) -> Result<RetainedId, String> {
    let global = core.global_object();
    let function = core
        .member_read(global, &TjsString::from(TARGET))
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("the global holds no `{TARGET}`"))?;
    Ok(core.retain(function))
}

/// The number the vendored side answered.
fn cpp_number(value: &TjsValue) -> Result<f64, String> {
    match value {
        TjsValue::Integer(number) => Ok(*number as f64),
        TjsValue::Real(number) => Ok(*number),
        other => Err(format!(
            "the vendored side answered {other:?}, not a number"
        )),
    }
}

/// The number the Rust side answered.
fn rust_number(value: &Variant) -> Result<f64, String> {
    match value {
        Variant::Integer(number) => Ok(*number as f64),
        Variant::Real(number) => Ok(*number),
        other => Err(format!("the Rust side answered {other:?}, not a number")),
    }
}

/// Prints one row in the machine-readable form.
fn print_machine_row(row: &Row) {
    let item = row.item;
    let phase = row.phase;
    let cpp = row.cpp_ns;
    let rust = row.rust_ns;
    let quotient = ratio(cpp, rust);
    println!("{item}/{phase}\t{cpp:.1}\t{rust:.1}\t{quotient:.3}");
}

/// Prints the human summary: one line per row, in corpus order.
fn print_summary(rows: &[Row]) {
    println!(
        "the interpreter: each script's loop runs ten thousand iterations, {HOT_CALLS} calls per hot pass, median of {REPETITIONS}"
    );
    println!("      item     phase          cpp ns         rust ns   ratio  work");
    let mut previous = "";
    for row in rows {
        let item = if row.item == previous { "" } else { row.item };
        previous = row.item;
        let phase = row.phase;
        let cpp = row.cpp_ns;
        let rust = row.rust_ns;
        let quotient = ratio(cpp, rust);
        let work = &row.work;
        println!("{item:>10}  {phase:>8}  {cpp:>14.1}  {rust:>14.1}  {quotient:>7.3}  {work}");
    }
    println!(
        "the ratio is the Rust side over the vendored side: below 1.0 the Rust executor is faster"
    );
    println!("`compile` is the same front end on both sides: it is the harness's own control");
    println!("`cold` is compile plus one call; `hot` is one call of the compiled body");
}

/// How many blocks one script entry compiled on the two runs of one text.
struct Compiled {
    /// The blocks the first run compiled.
    first: u64,
    /// The blocks the second run compiled.
    second: u64,
}

/// What each executor compiled when the same text was run twice.
struct CacheCounts {
    /// The Rust engine's two runs.
    rust: Compiled,
    /// The vendored engine's two runs.
    cpp: Compiled,
}

/// Whether both engines compile the same text again when it is run again.
///
/// The front end's counter is process-wide, so a second run that compiles
/// nothing is a run that was answered from the block the first one built.
fn check_block_cache() -> Result<(), String> {
    tjs::vm::set_compile_stats_enabled(true);
    let counts = block_cache_counts();
    tjs::vm::set_compile_stats_enabled(false);
    let counts = counts?;
    let rust_first = counts.rust.first;
    let rust_second = counts.rust.second;
    let cpp_first = counts.cpp.first;
    let cpp_second = counts.cpp.second;
    println!("# the block cache, the same text and name twice:");
    println!(
        "#   rust executor: run compiled {rust_first} block, then {rust_second} (a repeat is answered from the block cache)"
    );
    println!(
        "#   vendored executor: the script entry compiled {cpp_first} block, then {cpp_second} (every call compiles the script)"
    );
    Ok(())
}

/// How many blocks each script entry compiled on the first and on the second
/// run of the same text.
fn block_cache_counts() -> Result<CacheCounts, String> {
    let rust = {
        let core = EngineCore::new();
        let before = tjs::vm::compile_stats().compiles;
        core.run(CACHE_CHECK, BLOCK)
            .map_err(|error| error.to_string())?;
        let first = tjs::vm::compile_stats().compiles - before;
        core.run(CACHE_CHECK, BLOCK)
            .map_err(|error| error.to_string())?;
        let second = tjs::vm::compile_stats().compiles - before - first;
        Compiled { first, second }
    };
    let cpp = {
        let engine = Tjs2Engine::new().map_err(|error| error.to_string())?;
        let before = tjs::vm::compile_stats().compiles;
        engine
            .exec_script(CACHE_CHECK, BLOCK)
            .map_err(|error| error.to_string())?;
        let first = tjs::vm::compile_stats().compiles - before;
        engine
            .exec_script(CACHE_CHECK, BLOCK)
            .map_err(|error| error.to_string())?;
        let second = tjs::vm::compile_stats().compiles - before - first;
        Compiled { first, second }
    };
    Ok(CacheCounts { rust, cpp })
}

/// The Rust side over the vendored side, `NaN` when the vendored side measured
/// nothing.
fn ratio(cpp_ns: f64, rust_ns: f64) -> f64 {
    if cpp_ns > 0.0 {
        rust_ns / cpp_ns
    } else {
        f64::NAN
    }
}

/// The nanoseconds from `started` to now.
fn nanos(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1e9
}

/// The machine's load average, as the kernel reports it.
///
/// The load is what says whether a number is a measurement: a number taken
/// while the machine was busy is a report of the machine. A host without the
/// file has no load to report, and that is not a failure.
fn load_average() -> Result<Option<(f64, f64, f64)>, String> {
    let text = match std::fs::read_to_string("/proc/loadavg") {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot read /proc/loadavg: {error}")),
    };
    let mut parts = text.split_whitespace();
    let mut next = |name: &str| -> Result<f64, String> {
        let value = parts
            .next()
            .ok_or_else(|| format!("/proc/loadavg names no {name}"))?;
        value
            .parse()
            .map_err(|_| format!("/proc/loadavg names a {name} that is not a number: {value}"))
    };
    let one = next("one-minute load")?;
    let five = next("five-minute load")?;
    let fifteen = next("fifteen-minute load")?;
    Ok(Some((one, five, fifteen)))
}

/// Prints the machine's load average beside the table, or says why there is
/// none.
fn report_load(when: &str) {
    match load_average() {
        Ok(Some((one, five, fifteen))) => {
            println!("# the load average {when}: {one:.2} {five:.2} {fifteen:.2}")
        }
        Ok(None) => println!("# the load average {when}: the kernel did not report one"),
        Err(error) => eprintln!("the load average {when}: {error}"),
    }
}
