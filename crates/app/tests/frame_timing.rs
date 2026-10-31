//! Proves the per-frame phase breakdown is populated and internally
//! consistent.
//!
//! The device cannot be measured here, so the test drives the real
//! [`run_frame_body`] with a body whose sound poll sleeps a known amount and
//! checks that the reported sound cost dominates, that every phase is
//! reported, and that the wall-clock total that contains all of them is at
//! least their sum. It then drives a real [`VmHost::tick`] to prove the input
//! segments report before the ordered polls. This is instrumentation
//! coverage, not a reference behaviour assertion.

use std::time::{Duration, Instant};

use compat::{CompatCtx, CompatError, FrameBody, run_frame_body};
use engine::FrameClock;
use engine::input::InputState;
use engine::timer::TimerState;
use testkit::vm_lock;
use vm::{FrameCtx, VmHost};

/// A frame body that records every phase completion and can make the sound
/// phase deliberately slow.
#[derive(Default)]
struct RecordingBody {
    /// Whether `sound_poll` sleeps before returning.
    slow_sound: bool,
    /// `(phase name, reported duration)` in completion order.
    phases: Vec<(&'static str, Duration)>,
}

impl FrameBody for RecordingBody {
    fn sound_poll(&mut self, _ctx: &mut CompatCtx) -> Result<(), CompatError> {
        if self.slow_sound {
            std::thread::sleep(Duration::from_millis(25));
        }
        Ok(())
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

    fn frame_tail_completed(&mut self, elapsed: Duration) {
        self.phases.push(("frame_tail", elapsed));
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

/// Creates a VM host with the input state, the timer registry and the frame
/// clock attached, as the running game attaches them.
fn vm_host() -> (VmHost, Box<InputState>, Box<TimerState>, Box<FrameClock>) {
    let mut input = Box::new(InputState::new());
    let mut timer = Box::new(TimerState::new());
    let mut clock = Box::new(FrameClock::new());
    let host = VmHost::with_default_services(|services| {
        // SAFETY: every box outlives the host in the caller's return value, and
        // nothing else aliases them while the binding is installed.
        unsafe {
            services.attach_input(input.as_mut() as *mut InputState);
            services.attach_timer(timer.as_mut() as *mut TimerState);
            services.attach_clock(clock.as_mut() as *mut FrameClock);
        }
    })
    .expect("vm host");
    (host, input, timer, clock)
}

#[test]
fn slow_phase_dominates_and_the_wall_total_contains_the_sum() {
    // The VM host binds the process-global engine, so the test serializes on
    // the same lock the other VM tests take.
    let _lock = vm_lock();
    let (mut host, _input, _timer, _clock) = vm_host();
    let mut ctx = host.compat().ctx().expect("bound compat context");
    let mut body = RecordingBody {
        slow_sound: true,
        phases: Vec::new(),
    };

    let started = Instant::now();
    run_frame_body(&mut body, &mut ctx).expect("frame body");
    let total = started.elapsed();

    let names: Vec<&str> = body.phases.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        names,
        vec![
            "sound",
            "video",
            "async_trigger",
            "continuous_handler",
            "frame_tail",
        ],
        "every phase is reported, in the documented order"
    );

    let sum: Duration = body.phases.iter().map(|(_, elapsed)| *elapsed).sum();
    let sound = body.phases[0].1;
    assert!(
        sound >= Duration::from_millis(20),
        "the slow sound phase must report its sleep, got {sound:?}"
    );
    for (name, elapsed) in &body.phases[1..] {
        assert!(
            *elapsed < sound,
            "the slow sound phase must dominate {name} ({elapsed:?})"
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
    body.slow_sound = false;
    let frame = FrameCtx {
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
            "sound",
            "video",
            "async_trigger",
            "continuous_handler",
            "frame_tail",
        ],
        "the tick reports the input segments before the ordered polls"
    );
}
