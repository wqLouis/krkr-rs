//! Proves the per-frame phase breakdown is populated and internally
//! consistent.
//!
//! The device cannot be measured here, so the test drives the real
//! [`run_frame_body`] with a body whose timer poll sleeps a known amount and
//! checks that the reported timer cost dominates, that every phase is
//! reported, and that the wall-clock total that contains all of them is at
//! least their sum. It then drives a real [`VmHost::tick`] to prove the input
//! segments report before the ordered polls. This is instrumentation
//! coverage, not a reference behaviour assertion.

use std::time::{Duration, Instant};

use compat::{CompatCtx, CompatError, FrameBody, run_frame_body};
use vm::{FrameCtx, VmHost};

/// A frame body that records every phase completion and can make the timer
/// phase deliberately slow.
#[derive(Default)]
struct RecordingBody {
    /// Whether `timer_poll` sleeps before returning.
    slow_timer: bool,
    /// `(phase name, reported duration)` in completion order.
    phases: Vec<(&'static str, Duration)>,
}

impl FrameBody for RecordingBody {
    fn timer_poll(&mut self, _ctx: &mut CompatCtx) -> Result<(), CompatError> {
        if self.slow_timer {
            std::thread::sleep(Duration::from_millis(25));
        }
        Ok(())
    }

    fn timer_completed(&mut self, elapsed: Duration) {
        self.phases.push(("timer", elapsed));
    }

    fn sound_completed(&mut self, elapsed: Duration) {
        self.phases.push(("sound", elapsed));
    }

    fn video_completed(&mut self, elapsed: Duration) {
        self.phases.push(("video", elapsed));
    }

    fn async_trigger_completed(&mut self, elapsed: Duration) {
        self.phases.push(("async_trigger", elapsed));
    }

    fn continuous_handler_completed(&mut self, elapsed: Duration) {
        self.phases.push(("continuous_handler", elapsed));
    }

    fn project_and_render_completed(&mut self, elapsed: Duration) {
        self.phases.push(("project_and_render", elapsed));
    }

    fn input_begin_completed(&mut self, elapsed: Duration) {
        self.phases.push(("input_begin", elapsed));
    }

    fn input_events_completed(&mut self, elapsed: Duration) {
        self.phases.push(("input_events", elapsed));
    }

    fn input_end_completed(&mut self, elapsed: Duration) {
        self.phases.push(("input_end", elapsed));
    }
}

#[test]
fn slow_phase_dominates_and_the_wall_total_contains_the_sum() {
    let mut host = VmHost::with_default().expect("vm host");
    let mut ctx = host.compat().ctx().expect("bound compat context");
    let mut body = RecordingBody {
        slow_timer: true,
        phases: Vec::new(),
    };

    let started = Instant::now();
    run_frame_body(&mut body, &mut ctx).expect("frame body");
    let total = started.elapsed();

    let names: Vec<&str> = body.phases.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        names,
        vec![
            "timer",
            "sound",
            "video",
            "async_trigger",
            "continuous_handler",
            "project_and_render",
        ],
        "every phase is reported, in the documented order"
    );

    let sum: Duration = body.phases.iter().map(|(_, elapsed)| *elapsed).sum();
    let timer = body.phases[0].1;
    assert!(
        timer >= Duration::from_millis(20),
        "the slow timer phase must report its sleep, got {timer:?}"
    );
    for (name, elapsed) in &body.phases[1..] {
        assert!(
            *elapsed < timer,
            "the slow timer phase must dominate {name} ({elapsed:?})"
        );
    }
    assert!(
        total >= sum,
        "the wall total {total:?} must contain the phase sum {sum:?}"
    );
    assert!(
        total >= Duration::from_millis(25),
        "the wall total must include the deliberate sleep, got {total:?}"
    );

    // A real tick reports the event-adapter segments before the ordered
    // polls, so a slow gap can be attributed to input dispatch or to the
    // frame body.
    body.phases.clear();
    body.slow_timer = false;
    let frame = FrameCtx {
        now_ms: 16,
        dt_ms: 16,
        body: &mut body,
        events: &[],
    };
    host.tick(frame).expect("tick");
    let tick_names: Vec<&str> = body.phases.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        tick_names,
        vec![
            "input_begin",
            "input_events",
            "input_end",
            "timer",
            "sound",
            "video",
            "async_trigger",
            "continuous_handler",
            "project_and_render",
        ],
        "the tick reports the input segments before the ordered polls"
    );
}
