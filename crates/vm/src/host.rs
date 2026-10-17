//! The VM host: engine lifecycle, logging, script execution and the frame
//! body.
//!
//! The engine, its scene, its frame clock and its input and timer state are L2
//! state. The host owns only the C++ engine and the L3 surface; the
//! composition root attaches the L2 service handles through
//! [`VmHost::with_default_services`] before the surface is installed.
#![allow(unsafe_code)]

use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use compat::{CompatHost, EventAdapter, FrameBody, PluginRegistry, run_frame_body};
use platform::PlatformEvent;
use tjs::{LOG_DEBUG, LOG_ERROR, LOG_INFO, LOG_WARN, Tjs2Engine, TjsValue};

use crate::VmError;

/// The per-frame inputs handed to [`VmHost::tick`].
pub struct FrameCtx<'a> {
    /// Monotonic frame timestamp in milliseconds.
    pub now_ms: u64,
    /// Elapsed milliseconds since the previous frame.
    pub dt_ms: u32,
    /// The ordered frame-body polls (doc 08 §6).
    pub body: &'a mut dyn FrameBody,
    /// Platform events for L3 to adapt and deliver this frame.
    pub events: &'a [PlatformEvent],
}

/// The TJS VM host (L4).
///
/// Owns the C++ engine (boxed so its address is stable for the L3 binding)
/// and the L3 [`CompatHost`] that holds the native surface state. The fields
/// are ordered so the engine is dropped last: the dispatch state retains
/// values whose `Drop` releases an id on the engine.
pub struct VmHost {
    /// The L3 dispatch state. Declared before `engine` so its retained values
    /// are released first (fields drop in declaration order).
    compat: CompatHost,
    adapter: EventAdapter,
    /// The per-host VM log deduplicator. The engine's log callback points at
    /// this boxed value, so its address stays stable while the host moves.
    log_dedup: Box<LogDedup>,
    /// The C++ engine. Declared last so it outlives every value the native
    /// state retains.
    engine: Box<Tjs2Engine>,
}

/// Full exception dumps emitted before repeats are collapsed.
///
/// The first dump is the root-cause evidence; a second and third confirm the
/// repetition is deterministic rather than a one-off. Beyond three there is
/// nothing new to see: the site and the disassembled instruction are
/// identical, and the summary carries the running count and the last
/// timestamp.
const FULL_EXCEPTION_DUMPS: u32 = 3;

/// A suppression summary is emitted for every this many suppressed dumps.
///
/// The periodic summary keeps a long-running failure from growing without
/// bound and bounds what a log rotation can drop: at most this many
/// suppressed dumps are ever unreported between summaries. A final summary is
/// emitted when the run ends or the host shuts down.
const SUPPRESSED_PER_SUMMARY: u64 = 100;

/// The line that separates the disassembly from the register dump.
const REGISTER_DUMP_MARKER: &str = "-- Register dump --";

/// One decision the deduplicator makes for an incoming VM log record.
enum LogAction {
    /// Pass a non-dump record through unchanged.
    Record { level: c_int, text: String },
    /// Emit the lines of one full exception dump.
    Dump { level: c_int, lines: Vec<String> },
    /// Emit a suppression summary for the current run.
    Summary(String),
}

/// The dump block being accumulated: its level, every line, and the subset of
/// lines that form the identity (the header plus the disassembly).
struct PendingDump {
    level: c_int,
    lines: Vec<String>,
    identity: Vec<String>,
    /// Set once the register dump begins; register values are volatile and are
    /// deliberately excluded from the identity.
    in_registers: bool,
}

/// Collapses consecutive VM exception dumps with the same identity.
///
/// `DisplayExceptionGeneratedCode` prints a dump as a sequence of console
/// records: a header naming the file, function and VM ip, the disassembly, the
/// register dump, then a separator. The identity is the header plus the
/// disassembly; the register dump is excluded so a repeated failure with
/// different object addresses still collapses. A dump with a different
/// identity is always emitted in full and starts a new run, so the first
/// occurrence of every distinct failure survives.
#[derive(Default)]
struct LogDedup {
    /// The dump block currently being accumulated, if any.
    pending: Option<PendingDump>,
    /// The identity of the current run of dumps.
    run_identity: Option<String>,
    /// Dumps already emitted in full in the current run.
    run_emitted: u32,
    /// Dumps suppressed since the last summary.
    since_summary: u64,
    /// Wall-clock time of the most recently suppressed dump.
    last_suppressed: Option<SystemTime>,
}

impl LogDedup {
    /// Feeds one VM log record and returns the records to emit.
    fn record(&mut self, level: c_int, msg: String) -> Vec<LogAction> {
        // A header always closes any open block and starts a new one, so a
        // partial dump is never merged with the next.
        if is_dump_header(&msg) {
            let out = self.close_pending();
            self.begin_dump(level, msg);
            return out;
        }
        // A line inside a dump belongs to the open block.
        if let Some(pending) = self.pending.as_mut() {
            if msg == REGISTER_DUMP_MARKER {
                pending.in_registers = true;
            } else if !pending.in_registers && !is_dump_separator(&msg) {
                pending.identity.push(msg.clone());
            }
            pending.lines.push(msg.clone());
        } else {
            return vec![LogAction::Record { level, text: msg }];
        }
        if is_dump_separator(&msg) {
            self.close_pending()
        } else {
            Vec::new()
        }
    }

    /// Starts accumulating a dump from its header line.
    fn begin_dump(&mut self, level: c_int, header: String) {
        self.pending = Some(PendingDump {
            level,
            identity: vec![header.clone()],
            lines: vec![header],
            in_registers: false,
        });
    }

    /// Completes the open dump block and decides whether to emit it.
    fn close_pending(&mut self) -> Vec<LogAction> {
        let Some(pending) = self.pending.take() else {
            return Vec::new();
        };
        let identity = pending.identity.join("\n");
        let mut out = Vec::new();
        if self.run_identity.as_deref() == Some(identity.as_str()) {
            if self.run_emitted < FULL_EXCEPTION_DUMPS {
                self.run_emitted += 1;
                out.push(LogAction::Dump {
                    level: pending.level,
                    lines: pending.lines,
                });
            } else {
                self.since_summary += 1;
                self.last_suppressed = Some(SystemTime::now());
                if self.since_summary >= SUPPRESSED_PER_SUMMARY {
                    out.push(self.summary());
                }
            }
        } else {
            // A different exception always ends the previous run (emitting its
            // pending summary) and is emitted in full.
            out.extend(self.flush_run_summary());
            self.run_identity = Some(identity);
            self.run_emitted = 1;
            self.since_summary = 0;
            self.last_suppressed = None;
            out.push(LogAction::Dump {
                level: pending.level,
                lines: pending.lines,
            });
        }
        out
    }

    /// Builds the summary for the suppressed dumps and resets the counter.
    fn summary(&mut self) -> LogAction {
        let count = self.since_summary;
        self.since_summary = 0;
        let header = self
            .run_identity
            .as_deref()
            .and_then(|identity| identity.lines().next())
            .unwrap_or("exception")
            .trim();
        let when = self
            .last_suppressed
            .map(utc_timestamp)
            .unwrap_or_else(|| "unknown".to_string());
        LogAction::Summary(format!(
            "tjs: suppressed {count} further identical exception dump(s) for {header}; last at {when}"
        ))
    }

    /// Emits the run's pending summary and clears the run state.
    fn flush_run_summary(&mut self) -> Vec<LogAction> {
        let mut out = Vec::new();
        if self.since_summary > 0 {
            out.push(self.summary());
        }
        self.run_identity = None;
        self.run_emitted = 0;
        self.since_summary = 0;
        self.last_suppressed = None;
        out
    }

    /// Flushes any partial dump and the run's pending summary at shutdown.
    fn flush(&mut self) -> Vec<LogAction> {
        let mut out = self.close_pending();
        out.extend(self.flush_run_summary());
        out
    }
}

/// Whether `msg` opens an exception dump.
fn is_dump_header(msg: &str) -> bool {
    msg.starts_with("==== An exception occurred at ")
}

/// Whether `msg` is the all-dash separator that closes an exception dump.
fn is_dump_separator(msg: &str) -> bool {
    !msg.is_empty() && msg.bytes().all(|byte| byte == b'-')
}

/// Emits a deduplicated action through the `log` facade.
fn emit_action(action: LogAction) {
    match action {
        LogAction::Record { level, text } => emit_record(level, &text),
        LogAction::Dump { level, lines } => {
            for line in lines {
                emit_record(level, &line);
            }
        }
        LogAction::Summary(text) => log::debug!("{text}"),
    }
}

/// Emits one VM record at `level` under the `tjs:` prefix.
fn emit_record(level: c_int, msg: &str) {
    match level {
        LOG_ERROR => log::error!("tjs: {msg}"),
        LOG_WARN => log::warn!("tjs: {msg}"),
        LOG_INFO => log::info!("tjs: {msg}"),
        LOG_DEBUG => log::debug!("tjs: {msg}"),
        _ => log::trace!("tjs: {msg}"),
    }
}

/// UTC `YYYY-MM-DDTHH:MM:SSZ` for a wall-clock instant, or `"unknown"` for a
/// clock that cannot be represented relative to the epoch.
fn utc_timestamp(time: SystemTime) -> String {
    let Ok(elapsed) = time.duration_since(UNIX_EPOCH) else {
        return "unknown".to_string();
    };
    // A second count always fits in `i64`; the conversion states that rather
    // than truncating with `as`.
    let Ok(days) = i64::try_from(elapsed.as_secs() / 86_400) else {
        return "unknown".to_string();
    };
    let (year, month, day) = civil_from_days(days);
    let tod = elapsed.as_secs() % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Epoch days to `(year, month, day)`.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    (year, month, day)
}

extern "C" fn vm_log(level: c_int, msg: *const c_char, user: *mut c_void) {
    if msg.is_null() {
        return;
    }
    // SAFETY: the VM guarantees a NUL-terminated UTF-8 string valid for the
    // duration of the callback.
    let msg = unsafe { CStr::from_ptr(msg) }
        .to_string_lossy()
        .into_owned();
    if user.is_null() {
        emit_record(level, &msg);
        return;
    }
    // SAFETY: `user` points at the `LogDedup` owned by the live `VmHost`; the
    // box outlives the engine and the callback is cleared before that field is
    // freed.
    let dedup = unsafe { &mut *(user as *mut LogDedup) };
    for action in dedup.record(level, msg) {
        emit_action(action);
    }
}

impl VmHost {
    /// Create a host with the built-in plugin registry.
    pub fn with_default() -> Result<Self, VmError> {
        Self::new(PluginRegistry::builtin())
    }

    /// Create a host with the given plugin registry.
    ///
    /// The host boxes the engine and installs the L3 binding/classes before
    /// returning, so it is ready to [`exec`](Self::exec) and
    /// [`tick`](Self::tick).
    pub fn new(registry: PluginRegistry) -> Result<Self, VmError> {
        Self::new_with_services(registry, |_| {})
    }

    /// Create a host with the built-in plugin registry, attaching L2 service
    /// handles before the compat surface is installed.
    ///
    /// A plugin may read an L2 service while it registers (the `xp3filter`
    /// plugin reads `xp3filter.tjs` through the storage), so the handles must
    /// be attached before [`CompatHost::install`] runs — the same order
    /// [`CompatHost::services_mut`] documents. [`Self::with_default`] and
    /// [`Self::new`] attach nothing.
    pub fn with_default_services(
        attach: impl FnOnce(&mut compat::CompatServices),
    ) -> Result<Self, VmError> {
        Self::new_with_services(PluginRegistry::builtin(), attach)
    }

    /// Create a host with the given plugin registry, attaching L2 service
    /// handles through `attach` before the compat surface is installed.
    ///
    /// See [`Self::with_default_services`].
    pub fn new_with_services(
        registry: PluginRegistry,
        attach: impl FnOnce(&mut compat::CompatServices),
    ) -> Result<Self, VmError> {
        // The dedup state is boxed first so its address is stable for the log
        // callback and it outlives the engine on every exit path.
        let mut log_dedup = Box::new(LogDedup::default());
        let engine = Box::new(Tjs2Engine::new().map_err(|err| VmError::Vm(err.to_string()))?);
        // SAFETY: `log_dedup` is a live box that outlives the engine; the
        // callback is cleared in `Drop` before the box is freed.
        unsafe {
            engine.set_log_cb(
                Some(vm_log),
                (&mut *log_dedup) as *mut LogDedup as *mut c_void,
            )
        };
        let mut compat = CompatHost::with_registry(registry);
        attach(compat.services_mut());
        compat.install(&engine)?;
        Ok(Self {
            compat,
            adapter: EventAdapter::new(),
            log_dedup,
            engine,
        })
    }

    /// The underlying TJS engine.
    pub fn engine(&self) -> &Tjs2Engine {
        &self.engine
    }

    /// The L3 compatibility host.
    pub fn compat(&self) -> &CompatHost {
        &self.compat
    }

    /// The L3 compatibility host, mutably.
    pub fn compat_mut(&mut self) -> &mut CompatHost {
        &mut self.compat
    }

    /// Execute a TJS script.
    ///
    /// The `Storages` native and the boot sequence are a later wave; this is
    /// the raw execution seam they will call.
    pub fn exec(&self, source: &str, name: &str) -> Result<TjsValue, VmError> {
        self.engine
            .exec_script(source, name)
            .map_err(|err| VmError::Script(err.to_string()))
    }

    /// Evaluate a TJS expression.
    pub fn eval(&self, expression: &str, name: &str) -> Result<TjsValue, VmError> {
        self.engine
            .eval(expression, name)
            .map_err(|err| VmError::Script(err.to_string()))
    }

    /// Run one frame body: adapt/deliver platform events, then run the
    /// ordered polls (doc 08 §6).
    pub fn tick(&mut self, frame: FrameCtx<'_>) -> Result<(), VmError> {
        let mut ctx = self.compat.ctx()?;
        // The input state is per-frame: clear it, fold in every raw event,
        // then commit so held/repeat counters advance exactly once (doc 08 §6).
        // Each segment reports its wall-clock cost through the body hooks.
        let started = Instant::now();
        self.adapter.begin_input_frame(&mut ctx);
        frame.body.input_begin_completed(started.elapsed());
        let started = Instant::now();
        for event in frame.events {
            self.adapter.apply_input_event(&mut ctx, event);
            if let Some(mapped) = self.adapter.map(event) {
                self.adapter.dispatch(&mut ctx, &mapped)?;
            }
        }
        frame.body.input_events_completed(started.elapsed());
        let started = Instant::now();
        self.adapter.end_input_frame(&mut ctx);
        frame.body.input_end_completed(started.elapsed());
        run_frame_body(frame.body, &mut ctx)?;
        Ok(())
    }
}

impl Drop for VmHost {
    fn drop(&mut self) {
        // Release the native state's retained values while the engine is
        // still alive, then remove the binding. Dropping the state after the
        // engine is destroyed would release ids against a freed engine.
        self.compat.shutdown();
        // Emit the final suppression summary, capturing any log the shutdown
        // produced, then detach the callback so a late engine log cannot
        // reach the state that drops with this host.
        for action in self.log_dedup.flush() {
            emit_action(action);
        }
        // SAFETY: the engine is alive and the callback slot accepts a null
        // callback.
        unsafe { self.engine.set_log_cb(None, ptr::null_mut()) };
    }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tjs::{DetachedValue, RetainedValue};

    use super::*;

    static STATE_DROPS: AtomicUsize = AtomicUsize::new(0);

    thread_local! {
        /// Log lines written on this thread since the last clear. Tests run in
        /// parallel, so the capture is per-thread.
        static LINES: std::cell::RefCell<Vec<String>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    /// A `log` sink that records every record on the calling thread.
    struct CaptureLogger;

    impl log::Log for CaptureLogger {
        fn enabled(&self, _metadata: &log::Metadata) -> bool {
            true
        }

        fn log(&self, record: &log::Record) {
            LINES.with(|lines| {
                lines
                    .borrow_mut()
                    .push(format!("{} {}", record.level(), record.args()));
            });
        }

        fn flush(&self) {}
    }

    /// Installs the capture sink once per test binary and clears this thread's
    /// buffer.
    fn begin_capture() {
        static ONCE: OnceLock<()> = OnceLock::new();
        ONCE.get_or_init(|| {
            let _ = log::set_boxed_logger(Box::new(CaptureLogger));
            log::set_max_level(log::LevelFilter::Debug);
        });
        LINES.with(|lines| lines.borrow_mut().clear());
    }

    /// This thread's captured log lines.
    fn captured() -> Vec<String> {
        LINES.with(|lines| lines.borrow().clone())
    }

    /// Native dispatch state that holds a value retained on the engine. Its
    /// `Drop` records that teardown reached the state; the retained value's
    /// own `Drop` then releases the id on the engine.
    #[derive(Default)]
    struct TeardownProbe {
        value: Option<DetachedValue>,
    }

    impl Drop for TeardownProbe {
        fn drop(&mut self) {
            STATE_DROPS.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Dropping the host must release the native dispatch state while the
    /// engine is still alive. The dropped `DetachedValue` releases its id on
    /// the engine, so a teardown order that freed the engine first would use
    /// the freed pointer.
    #[test]
    fn dropping_the_host_releases_native_state_before_the_engine() {
        let _lock = testkit::vm_lock();
        let host = VmHost::with_default().expect("vm host");
        let value = host
            .engine()
            .eval_retained("[]", "teardown")
            .expect("retain an object");
        let RetainedValue::Object(object) = value else {
            panic!("eval_retained must return an object");
        };
        host.compat().services().state::<TeardownProbe>().value = Some(object);
        STATE_DROPS.store(0, Ordering::SeqCst);
        drop(host);
        assert_eq!(
            STATE_DROPS.load(Ordering::SeqCst),
            1,
            "VmHost::drop must drop the native dispatch state"
        );
    }

    /// A caught script exception travels the full path: the VM's spdlog
    /// record is bridged to the engine callback, which the host routes to
    /// the `log` crate. The record must be DEBUG and carry the exception
    /// message. Reference: `reference/cpp/core/tjs2/tjsInterCodeExec.cpp:1311`.
    #[test]
    fn caught_exception_reaches_the_engine_log() {
        use std::ffi::{c_char, c_int, c_void};
        use tjs::{NativeClassBuilder, NativeMethodDef, Value};

        // A native that always fails: the VM raises it as an `eTJS` error,
        // so a surrounding `try` reaches the logging catch arm.
        extern "C" fn boom(
            _engine: *mut c_void,
            _argc: c_int,
            _argv: *const Value,
            _out: *mut Value,
            out_error: *mut *mut c_char,
        ) -> c_int {
            native_error(out_error, b"boom-message")
        }

        let _lock = testkit::vm_lock();
        let host = VmHost::with_default().expect("vm host");
        host.engine()
            .register_native_class(&NativeClassBuilder {
                name: "Boom",
                methods: vec![NativeMethodDef {
                    name: "fire",
                    f: boom,
                }],
                properties: Vec::new(),
            })
            .expect("register Boom");

        begin_capture();
        host.exec(
            "var r = 0; try { Boom.fire(); } catch(e) { r = 1; }",
            "test",
        )
        .expect("exec");
        assert_eq!(host.eval("r", "test").unwrap(), TjsValue::Integer(1));

        let lines = captured();
        // The bare message is the record the spdlog bridge adds; a debug-mode
        // build also emits a `... at position` console record, which does not
        // match exactly.
        assert!(
            lines.iter().any(|line| line == "DEBUG tjs: boom-message"),
            "the caught exception must reach the engine log: {lines:?}"
        );
    }

    /// Fills `out_error` with a heap string and reports failure, the native
    /// error shape the VM raises as an uncaught exception.
    fn native_error(out_error: *mut *mut c_char, message: &[u8]) -> c_int {
        let mut bytes = message.to_vec();
        bytes.push(0);
        // SAFETY: tjs2_malloc returns a writable buffer of the requested size,
        // and out_error is a valid slot for the call.
        unsafe {
            let p = tjs::tjs2_malloc(bytes.len()) as *mut u8;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
            *out_error = p as *mut c_char;
        }
        1
    }

    /// One synthetic exception dump: header, disassembly, register dump, then
    /// the all-dash separator. `reg` varies the register value so the test can
    /// prove the register dump is not part of the identity.
    fn dump_lines(header: &str, reg: u32) -> Vec<String> {
        vec![
            header.to_string(),
            "-- Disassembled VM code --".to_string(),
            "*0 = (string)\"x\"".to_string(),
            "8 gpi %3, %1.%2".to_string(),
            REGISTER_DUMP_MARKER.to_string(),
            format!("%0=(int){reg}"),
            "------------------------------".to_string(),
        ]
    }

    /// Runs a full dump through the deduplicator, collecting emitted dump
    /// lines and summaries.
    fn feed_dump(
        dedup: &mut LogDedup,
        emitted: &mut Vec<String>,
        summaries: &mut Vec<String>,
        header: &str,
        reg: u32,
    ) {
        for line in dump_lines(header, reg) {
            for action in dedup.record(LOG_DEBUG, line) {
                collect(action, emitted, summaries);
            }
        }
    }

    /// Sorts a deduplicator action into the emitted dump lines or the
    /// summaries.
    fn collect(action: LogAction, emitted: &mut Vec<String>, summaries: &mut Vec<String>) {
        match action {
            LogAction::Record { text, .. } => emitted.push(text),
            LogAction::Dump { lines, .. } => emitted.extend(lines),
            LogAction::Summary(text) => summaries.push(text),
        }
    }

    /// N identical dumps are emitted `FULL_EXCEPTION_DUMPS` times and then
    /// collapsed; the final summary carries the suppressed count. The register
    /// value changes every time to prove the register dump is not the identity.
    #[test]
    fn log_dedup_collapses_identical_dumps() {
        const HEADER: &str = "==== An exception occurred at a.tjs(1), VM ip = 4 ==== ";
        let mut dedup = LogDedup::default();
        let (mut emitted, mut summaries) = (Vec::new(), Vec::new());
        for reg in 0..6 {
            feed_dump(&mut dedup, &mut emitted, &mut summaries, HEADER, reg);
        }
        for action in dedup.flush() {
            collect(action, &mut emitted, &mut summaries);
        }

        let full = emitted
            .iter()
            .filter(|line| line.starts_with("==== An exception occurred"))
            .count();
        assert_eq!(
            full, FULL_EXCEPTION_DUMPS as usize,
            "only the first full dumps are emitted: {emitted:?}"
        );
        assert_eq!(summaries.len(), 1, "one final summary: {summaries:?}");
        assert!(
            summaries[0].contains("suppressed 3 further identical exception dump(s)"),
            "the summary carries the count: {summaries:?}"
        );
    }

    /// A different exception in between is always emitted in full and ends the
    /// previous run; a later repeat of the first exception starts its own run
    /// and is emitted again.
    #[test]
    fn log_dedup_emits_a_different_dump_in_full_and_resets() {
        const A: &str = "==== An exception occurred at a.tjs(1), VM ip = 4 ==== ";
        const B: &str = "==== An exception occurred at b.tjs(9), VM ip = 7 ==== ";
        let mut dedup = LogDedup::default();
        let (mut emitted, mut summaries) = (Vec::new(), Vec::new());

        for reg in 0..4 {
            feed_dump(&mut dedup, &mut emitted, &mut summaries, A, reg);
        }
        feed_dump(&mut dedup, &mut emitted, &mut summaries, B, 0);
        for reg in 0..4 {
            feed_dump(&mut dedup, &mut emitted, &mut summaries, A, reg);
        }
        for action in dedup.flush() {
            collect(action, &mut emitted, &mut summaries);
        }

        let count = |needle: &str| {
            emitted
                .iter()
                .filter(|line| line.starts_with(needle))
                .count()
        };
        assert_eq!(count(A), 6, "six A dumps: three per run: {emitted:?}");
        assert_eq!(
            count(B),
            1,
            "the different dump is always emitted: {emitted:?}"
        );
        assert_eq!(summaries.len(), 2, "one summary per A run: {summaries:?}");
        assert!(
            summaries.iter().all(|s| s.contains(A.trim())),
            "each summary names its run: {summaries:?}"
        );
    }

    /// Two different uncaught exceptions driven through the VM: the repeated
    /// one is collapsed by the host log bridge and the different one is
    /// emitted in full. Reference:
    /// `reference/cpp/core/tjs2/tjsInterCodeExec.cpp:1313`.
    #[test]
    fn repeated_uncaught_exceptions_are_collapsed_in_the_engine_log() {
        use std::ffi::{c_char, c_int, c_void};
        use tjs::{NativeClassBuilder, NativeMethodDef, Value};

        extern "C" fn fire_a(
            _engine: *mut c_void,
            _argc: c_int,
            _argv: *const Value,
            _out: *mut Value,
            out_error: *mut *mut c_char,
        ) -> c_int {
            native_error(out_error, b"dump-a")
        }
        extern "C" fn fire_b(
            _engine: *mut c_void,
            _argc: c_int,
            _argv: *const Value,
            _out: *mut Value,
            out_error: *mut *mut c_char,
        ) -> c_int {
            native_error(out_error, b"dump-b")
        }

        let _lock = testkit::vm_lock();
        {
            let host = VmHost::with_default().expect("vm host");
            host.engine()
                .register_native_class(&NativeClassBuilder {
                    name: "DumpBoom",
                    methods: vec![
                        NativeMethodDef {
                            name: "fireA",
                            f: fire_a,
                        },
                        NativeMethodDef {
                            name: "fireB",
                            f: fire_b,
                        },
                    ],
                    properties: Vec::new(),
                })
                .expect("register DumpBoom");
            // The compat install runs its own registration scripts, which can
            // raise unrelated exceptions; clear the per-thread capture after
            // setup so only the A/B exercise below is counted.
            begin_capture();
            for _ in 0..5 {
                assert!(host.eval("DumpBoom.fireA()", "test").is_err());
            }
            assert!(host.eval("DumpBoom.fireB()", "test").is_err());
        }

        let lines = captured();
        // The suppression summary names the dump it counts, so it contains the
        // header text too; only the header lines themselves are dumps.
        let headers = lines
            .iter()
            .filter(|line| line.contains("An exception occurred") && !line.contains("suppressed"))
            .count();
        assert_eq!(headers, 4, "three A dumps plus the one B dump: {lines:?}");
        assert!(
            lines
                .iter()
                .any(|line| line.contains("suppressed 2 further identical exception dump(s)")),
            "the repeated A run is summarized: {lines:?}"
        );
    }
}
