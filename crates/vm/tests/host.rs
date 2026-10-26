//! Integration tests for the L4 VM host.

use compat::{CompatCtx, CompatError, FrameBody};
use engine::clock::FrameClock;
use testkit::vm_lock;
use tjs::TjsValue;
use vm::{FrameCtx, VmHost};

#[derive(Default)]
struct Recorder {
    order: Vec<&'static str>,
}

impl FrameBody for Recorder {
    fn sound_poll(&mut self, _ctx: &mut CompatCtx) -> Result<(), CompatError> {
        self.order.push("sound");
        Ok(())
    }

    fn video_poll(&mut self, _ctx: &mut CompatCtx) -> Result<(), CompatError> {
        self.order.push("video");
        Ok(())
    }

    fn async_trigger_poll(&mut self, _ctx: &mut CompatCtx) -> Result<(), CompatError> {
        self.order.push("async");
        Ok(())
    }

    fn continuous_handler_poll(&mut self, _ctx: &mut CompatCtx) -> Result<(), CompatError> {
        self.order.push("continuous");
        Ok(())
    }

    fn frame_tail(&mut self, _ctx: &mut CompatCtx) -> Result<(), CompatError> {
        self.order.push("present");
        Ok(())
    }
}

#[test]
fn exec_eval_and_tick() {
    let _lock = vm_lock();
    // The frame body the host runs reads the L2 frame clock, so the harness
    // attaches one before the compat surface is installed. The boxed clock
    // outlives the host: it is declared first, so it drops last.
    let mut clock = Box::new(FrameClock::new());
    let clock_ptr = clock.as_mut() as *mut FrameClock;
    let mut vm = VmHost::with_default_services(move |services| {
        // SAFETY: the boxed clock outlives the host, so the pointer stays
        // valid for as long as the compat binding can read it.
        unsafe {
            services.attach_clock(clock_ptr);
        }
    })
    .expect("create vm host");

    vm.exec("var counter = 1;", "boot").expect("exec");
    assert_eq!(
        vm.eval("counter + 1", "boot").unwrap(),
        TjsValue::Integer(2)
    );

    let mut recorder = Recorder::default();
    let frame = FrameCtx {
        now_ms: 0,
        dt_ms: 16,
        body: &mut recorder,
        events: &[],
    };
    vm.tick(frame).expect("tick");

    assert_eq!(
        recorder.order,
        ["sound", "video", "async", "continuous", "present"]
    );
}
