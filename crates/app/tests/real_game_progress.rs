//! Real-game progress regression: the scenario must keep moving.
//!
//! A game that stops making progress is silent: the frames keep coming, the
//! scene keeps its layers, and the engine logs nothing. The run this test
//! drives therefore has to answer two questions on its own: did a script
//! error end the run, and did the scenario move while the frames ran.
//!
//! Set `KRKR_TEST_GAME` to the game directory and run:
//!
//! ```text
//! KRKR_TEST_GAME=/path/to/game cargo test -p app --test real_game_progress -- --ignored --nocapture
//! ```
//!
//! The test reads the scenario position from the game's KAG conductor, which
//! is the parser the conductor walks: its storage, label, line and offset
//! move with every tag the conductor executes.

use std::cell::RefCell;
use std::path::Path;
use std::time::{Duration, Instant};

use app::{GameHost, Host, HostConfig, InjectedFrameTime};
use tjs::TjsValue;

/// The frames the run drives.
///
/// A frame is a 60 Hz frame of game time, so 3600 frames are a minute of
/// game time. The budget is the measured one: the boot flow waits on
/// one-second timers and on played audio, and this game's caution flow waits
/// for a voice to finish, so its position moves at frames 1, 2, 3, 4 and 36,
/// stands still until frame 1023 and reaches the title at frame 3249.
const FRAMES: u32 = 3600;

/// The frame period, in milliseconds.
const FRAME_MS: f32 = 1000.0 / 60.0;

/// The frame after which the scenario must still be moving.
///
/// The boot flow waits on one-second timers and on played audio, so the
/// scenario moves again within the first seconds of game time. A scenario
/// that stands still from the first second on is a game that is not making
/// progress.
const CHECKPOINT_FRAME: u32 = 60;

/// The wall-clock budget for the whole run.
///
/// The guard keeps a game that spends its time inside one frame from holding
/// the suite; the frames are driven as fast as the host runs them, so the
/// budget is only reached by a game that does far more work per frame.
const WALL_CLOCK_BUDGET: Duration = Duration::from_secs(900);

/// The number of captured log lines a failure prints.
const LOG_TAIL_LINES: usize = 40;

/// The markers that mean the run hit a fatal script error.
///
/// The first is the fatal message the engine's reference prints; the second
/// is the header of the exception dump the script engine writes when a script
/// error reaches the top level.
const FATAL_MARKERS: [&str; 2] = [
    "致命的エラーが発生しました。",
    "==== An exception occurred at ",
];

/// The conductors the probe reads, in the order it tries them.
///
/// The conductor is the object that walks the scenario, and it carries the
/// parser's position.
const CONDUCTORS: [&str; 2] = ["kag.mainConductor", "kag.conductor"];

/// The global the probe stores its result in.
const PROBE_GLOBAL: &str = "global.__kagProgress";

thread_local! {
    /// The log lines the capturing sink recorded.
    static CAPTURED_LOGS: RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// A `log` sink that records every record on the calling thread.
struct CaptureLogger;

impl log::Log for CaptureLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        CAPTURED_LOGS.with(|logs| {
            logs.borrow_mut()
                .push(format!("{} {}", record.level(), record.args()));
        });
    }

    fn flush(&self) {}
}

/// Installs the capturing sink and checks that it records.
///
/// The run's log assertions are worth nothing when the sink is not the one
/// this test reads, so the installation is verified before the game starts.
fn install_capture() {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        // The logger is process-global: a logger another test installed is
        // not an error here, and the check below says whether the capturing
        // sink is the one that records.
        if let Err(error) = log::set_boxed_logger(Box::new(CaptureLogger)) {
            eprintln!("the log sink is already taken: {error}");
        }
        log::set_max_level(log::LevelFilter::Debug);
    });
    log::info!("kag progress capture probe");
    let captured = CAPTURED_LOGS.with(|logs| logs.borrow().clone());
    assert!(
        captured
            .iter()
            .any(|line| line.contains("kag progress capture probe")),
        "the capturing log sink is not installed: {captured:?}"
    );
    CAPTURED_LOGS.with(|logs| logs.borrow_mut().clear());
}

/// The answer of the scenario probe.
enum Probe {
    /// The scenario position: the storage, the label, the line and the offset.
    Position(String),
    /// The conductor is not there, so the game exposes no position to read.
    Absent,
    /// The probe itself failed.
    Failed(String),
}

/// The outcome of the frame run.
struct Run {
    /// The frames the run drove.
    frames: u32,
    /// The conductor the probe reads, once it has found one.
    conductor: Option<String>,
    /// The positions the scenario stood on, with the frame each appeared in.
    timeline: Vec<(u32, String)>,
    /// The number of times the scenario moved after the checkpoint.
    changes_after_checkpoint: usize,
    /// The first frame that failed, with the error it failed with.
    frame_error: Option<String>,
    /// The first failure of the probe script itself.
    probe_error: Option<String>,
    /// Whether the wall-clock budget stopped the run.
    budget_exceeded: bool,
    /// The wall-clock time the run took.
    elapsed: Duration,
}

/// The script that reads the scenario position from `conductor`.
///
/// The position is the scenario file, the label, the line and the offset the
/// parser stands on. The script catches its own errors, so a game without
/// that conductor reports the absence instead of raising.
fn probe_script(conductor: &str) -> String {
    format!(
        "try {{\n    {PROBE_GLOBAL} = {conductor}.curStorage + \"|\" + {conductor}.curLabel \
         + \"|\" + {conductor}.curLine + \"|\" + {conductor}.curPos;\n}} catch (e) {{\n    \
         {PROBE_GLOBAL} = \"ERR \" + e.message;\n}}\n"
    )
}

/// Reads the scenario position from the game's conductor.
fn read_position(game: &GameHost, conductor: &str) -> Probe {
    let script = probe_script(conductor);
    if let Err(error) = game.vm().exec(&script, "kag-progress") {
        return Probe::Failed(format!("the probe script failed: {error}"));
    }
    match game.vm().eval(PROBE_GLOBAL, "kag-progress") {
        Ok(TjsValue::String(value)) if value.starts_with("ERR ") => Probe::Absent,
        Ok(TjsValue::String(value)) => Probe::Position(value),
        Ok(other) => Probe::Failed(format!("the probe returned {other:?}")),
        Err(error) => Probe::Failed(format!("the probe result could not be read: {error}")),
    }
}

/// Drives the frames and records where the scenario stood.
fn drive(game: &mut GameHost) -> Run {
    let started = Instant::now();
    let mut run = Run {
        frames: 0,
        conductor: None,
        timeline: Vec::new(),
        changes_after_checkpoint: 0,
        frame_error: None,
        probe_error: None,
        budget_exceeded: false,
        elapsed: Duration::ZERO,
    };
    let mut previous: Option<String> = None;
    for frame in 0..FRAMES {
        if started.elapsed() > WALL_CLOCK_BUDGET {
            run.budget_exceeded = true;
            break;
        }
        if let Err(error) = game.step(&InjectedFrameTime::new(f64::from(FRAME_MS)), &[]) {
            run.frame_error = Some(format!("frame {frame}: {error}"));
            break;
        }
        run.frames = frame + 1;

        if run.conductor.is_none() {
            for candidate in CONDUCTORS {
                match read_position(game, candidate) {
                    Probe::Position(_) => {
                        run.conductor = Some(candidate.to_owned());
                        break;
                    }
                    Probe::Absent => {}
                    Probe::Failed(error) => {
                        run.probe_error = Some(error);
                        break;
                    }
                }
            }
        }
        let Some(conductor) = run.conductor.as_ref() else {
            continue;
        };
        let position = match read_position(game, conductor) {
            Probe::Position(value) => value,
            Probe::Absent => continue,
            Probe::Failed(error) => {
                if run.probe_error.is_none() {
                    run.probe_error = Some(error);
                }
                continue;
            }
        };
        if previous.as_deref() != Some(position.as_str()) {
            if previous.is_some() && frame > CHECKPOINT_FRAME {
                run.changes_after_checkpoint += 1;
            }
            run.timeline.push((frame, position.clone()));
            previous = Some(position);
        }
    }
    run.elapsed = started.elapsed();
    run
}

/// The diagnostics a failure prints: the captured log's tail and the
/// positions the scenario stood on.
fn diagnostics(logs: &[String], run: &Run) -> String {
    let tail = logs
        .iter()
        .rev()
        .take(LOG_TAIL_LINES)
        .rev()
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    let timeline = run
        .timeline
        .iter()
        .map(|(frame, position)| format!("  frame {frame}: {position}"))
        .collect::<Vec<_>>()
        .join("\n");
    let scenarios = logs
        .iter()
        .filter_map(|line| line.split_once("scenario loaded : "))
        .map(|(_, name)| name.to_owned())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "the run drove {frames} frames in {seconds:.1} s; the conductor was {conductor:?}; \
         the scenario moved {changes} time(s) after frame {CHECKPOINT_FRAME}; \
         the wall-clock budget stopped the run: {budget}; \
         the scenarios the run loaded: {scenarios}\n\
         the captured log's last {LOG_TAIL_LINES} lines:\n{tail}\n\
         the positions the scenario stood on:\n{timeline}",
        frames = run.frames,
        seconds = run.elapsed.as_secs_f64(),
        conductor = run.conductor,
        changes = run.changes_after_checkpoint,
        budget = run.budget_exceeded,
        scenarios = scenarios,
    )
}

/// A real game must keep making progress while the frames run.
///
/// The run is judged on four things: no frame failed, the probe ran, the log
/// holds no fatal script error, and the scenario moved after the checkpoint
/// frame. The scenario's position is the observable that answers the last
/// question: the conductor walks the scenario, and the parser's storage,
/// label, line and offset move with every tag it executes. A scenario that
/// stands still while the frames run is a game that stopped making progress.
#[test]
#[ignore = "needs KRKR_TEST_GAME pointing at a real game"]
fn the_real_game_keeps_making_progress() {
    let Ok(dir) = std::env::var("KRKR_TEST_GAME") else {
        eprintln!("skipping: set KRKR_TEST_GAME to a game directory");
        return;
    };
    let dir = Path::new(&dir);
    let _lock = testkit::vm_lock();

    install_capture();

    let archives = testkit::game_archives(dir).expect("list game directory");
    let host = Host::new(HostConfig::headless()).expect("create host");
    let mut game = GameHost::open(host, archives, dir.to_path_buf()).expect("open game");

    let run = drive(&mut game);
    let logs = CAPTURED_LOGS.with(|logs| logs.borrow().clone());
    let diagnostics = diagnostics(&logs, &run);

    if let Some(error) = run.frame_error.as_ref() {
        panic!("a frame of the run failed: {error}\n{diagnostics}");
    }
    if let Some(error) = run.probe_error.as_ref() {
        panic!("the scenario probe failed: {error}\n{diagnostics}");
    }
    for marker in FATAL_MARKERS {
        assert!(
            !logs.iter().any(|line| line.contains(marker)),
            "the run logged a fatal script error ({marker})\n{diagnostics}"
        );
    }
    assert!(
        run.conductor.is_some(),
        "the game exposes no KAG conductor to read the scenario position from; \
         the candidates were {CONDUCTORS:?}\n{diagnostics}"
    );
    assert!(
        run.changes_after_checkpoint > 0,
        "the scenario stopped moving: it did not move once after frame \
         {CHECKPOINT_FRAME}\n{diagnostics}"
    );
}
