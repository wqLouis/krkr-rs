//! Integration tests for the `AsyncTrigger` native class.
//!
//! Every test builds its own VM + [`CompatHost`](compat::CompatHost) under the
//! shared [`common::vm_lock`] (the engine → context binding is process-wide)
//! and drives the frame body's `async_trigger_poll` step directly — the poll
//! is the seam an L4 host wires.

mod common;

use common::vm_lock;
use compat::CompatHost;
use tjs::{Tjs2Engine, TjsValue};

/// A bound VM + host.
///
/// The engine is boxed so its address stays stable after [`CompatHost::install`]
/// stored it, and the host is declared before the engine so its `Drop` removes
/// the binding while the VM is still alive.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
}

impl Env {
    fn new() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut host = CompatHost::new();
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "async-trigger-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval(&self, expression: &str) -> TjsValue {
        self.engine
            .eval(expression, "async-trigger-test")
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.eval(expression) {
            TjsValue::Integer(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    /// Run one frame-body `async_trigger_poll` step.
    fn poll(&self) {
        let mut ctx = self._host.ctx().expect("bound host");
        compat::async_trigger_poll(&mut ctx).expect("async trigger poll");
    }
}

#[test]
fn constructor_defaults_match_the_reference() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var t = new AsyncTrigger(function(){}, '');");
    // `tTJSNI_AsyncTrigger` ctor: Cached = true, Mode = atmNormal.
    assert_eq!(env.eval_int("t.cached"), 1);
    assert_eq!(env.eval_int("t.mode"), 0);
    // The reference mode constants.
    assert_eq!(env.eval_int("atmNormal"), 0);
    assert_eq!(env.eval_int("atmExclusive"), 1);
    assert_eq!(env.eval_int("atmAtIdle"), 2);
}

#[test]
fn trigger_defers_until_the_poll() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var n = 0; var t = new AsyncTrigger(function(){ n += 1; }, ''); t.trigger();");
    // `trigger()` must not invoke the handler synchronously.
    assert_eq!(env.eval_int("n"), 0);
    env.poll();
    assert_eq!(env.eval_int("n"), 1);
    // A later trigger fires on the next poll again.
    env.run("t.trigger();");
    assert_eq!(env.eval_int("n"), 1);
    env.poll();
    assert_eq!(env.eval_int("n"), 2);
}

#[test]
fn cached_trigger_coalesces() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var n = 0; var t = new AsyncTrigger(function(){ n += 1; }, ''); \
         t.trigger(); t.trigger(); t.trigger();",
    );
    env.poll();
    assert_eq!(
        env.eval_int("n"),
        1,
        "cached triggers pending at the same time coalesce"
    );
}

#[test]
fn uncached_trigger_queues_each_fire() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var n = 0; var t = new AsyncTrigger(function(){ n += 1; }, ''); \
         t.cached = false; t.trigger(); t.trigger(); t.trigger();",
    );
    env.poll();
    assert_eq!(
        env.eval_int("n"),
        3,
        "with cached = false each trigger queues its own fire"
    );
}

#[test]
fn cancel_drops_scheduled_fires() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var n = 0; var t = new AsyncTrigger(function(){ n += 1; }, ''); \
         t.trigger(); t.cancel();",
    );
    env.poll();
    assert_eq!(env.eval_int("n"), 0);
}

#[test]
fn changing_cached_cancels_scheduled_fires() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var n = 0; var t = new AsyncTrigger(function(){ n += 1; }, ''); \
         t.trigger(); t.cached = false;",
    );
    env.poll();
    assert_eq!(env.eval_int("n"), 0, "SetCached cancels pending events");
    // Writing the same value again must not cancel a fresh trigger.
    env.run("t.trigger(); t.cached = false;");
    env.poll();
    assert_eq!(env.eval_int("n"), 1);
}

#[test]
fn changing_mode_cancels_scheduled_fires() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var n = 0; var t = new AsyncTrigger(function(){ n += 1; }, ''); \
         t.trigger(); t.mode = atmAtIdle;",
    );
    env.poll();
    assert_eq!(env.eval_int("n"), 0, "SetMode cancels pending events");
    // Writing the same value again must not cancel a fresh trigger.
    env.run("t.trigger(); t.mode = atmAtIdle;");
    env.poll();
    assert_eq!(env.eval_int("n"), 1);
}

#[test]
fn empty_action_name_invokes_the_action_itself() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var n = 0; var f = function(){ n += 5; }; \
         var t = new AsyncTrigger(f, ''); t.trigger();",
    );
    env.poll();
    assert_eq!(env.eval_int("n"), 5);
}

#[test]
fn default_action_name_invokes_the_action_member() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var obj = %[count: 0, action: function(){ this.count += 10; }]; \
         var t = new AsyncTrigger(obj); t.trigger();",
    );
    env.poll();
    assert_eq!(
        env.eval_int("obj.count"),
        10,
        "an omitted action name invokes obj.action()"
    );
}

#[test]
fn explicit_action_name_selects_a_member() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var obj = %[count: 0, fire: function(){ this.count += 7; }]; \
         var t = new AsyncTrigger(obj, 'fire'); t.trigger();",
    );
    env.poll();
    assert_eq!(env.eval_int("obj.count"), 7);
}

#[test]
fn on_fire_invokes_the_action_immediately() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var n = 0; var t = new AsyncTrigger(function(){ n += 1; }, '');");
    env.run("t.onFire();");
    assert_eq!(env.eval_int("n"), 1);
}

#[test]
fn trigger_from_inside_a_handler_defers_to_the_next_poll() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var n = 0; var t; \
         t = new AsyncTrigger(function(){ n += 1; if(n < 3) t.trigger(); }, ''); \
         t.trigger();",
    );
    // One fire per poll: the re-trigger inside the handler must not re-enter
    // the VM within the same poll.
    env.poll();
    assert_eq!(env.eval_int("n"), 1);
    env.poll();
    assert_eq!(env.eval_int("n"), 2);
    env.poll();
    assert_eq!(env.eval_int("n"), 3);
    env.poll();
    assert_eq!(
        env.eval_int("n"),
        3,
        "no further fires once the handler stops"
    );
}

#[test]
fn constructor_requires_an_action_callback() {
    let _lock = vm_lock();
    let env = Env::new();
    assert!(env.engine.eval("new AsyncTrigger()", "t").is_err());
}
