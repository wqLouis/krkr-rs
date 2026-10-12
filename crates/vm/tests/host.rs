//! Integration tests for the L4 VM host.

use compat::{CompatCtx, CompatError, FrameBody};
use testkit::vm_lock;
use tjs::TjsValue;
use vm::{FrameCtx, VmHost};

#[derive(Default)]
struct Recorder {
    order: Vec<&'static str>,
}

impl FrameBody for Recorder {
    fn timer_poll(&mut self, _ctx: &mut CompatCtx) -> Result<(), CompatError> {
        self.order.push("timer");
        Ok(())
    }

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

    fn project_and_render(&mut self, _ctx: &mut CompatCtx) -> Result<(), CompatError> {
        self.order.push("present");
        Ok(())
    }
}

#[test]
fn exec_eval_and_tick() {
    let _lock = vm_lock();
    let mut vm = VmHost::with_default().expect("create vm host");

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
        ["timer", "sound", "video", "async", "continuous", "present"]
    );
}
