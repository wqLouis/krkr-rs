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
        common::link_all_builtins(&engine);
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

/// A script subclass may write `cached`/`mode` and call `trigger`/`cancel`
/// before it calls `super.AsyncTrigger(...)`. The reference initializes the
/// flags in the native constructor (`reference/cpp/core/base/EventIntf.cpp:1003-1011`)
/// and guards the queue on `Owner` (`:1043`, `:1064`), which `Construct` sets
/// (`:1015-1036`), so a pre-constructor write lands and a pre-constructor
/// `trigger` is a no-op rather than an error.
#[test]
fn subclass_members_before_the_base_constructor_follow_the_owner_guard() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        r#"
        var fired = 0;
        class Early extends AsyncTrigger {
            function Early() {
                cached = false;
                mode = 2;
                trigger();
                cancel();
                super.AsyncTrigger(function(){ fired = fired + 1; }, "");
            }
        }
        var t = new Early();
        "#,
    );
    // The pre-constructor writes reached the instance.
    assert_eq!(env.eval_int("t.cached"), 0);
    assert_eq!(env.eval_int("t.mode"), 2);
    // The pre-constructor `trigger` scheduled nothing: no owner existed yet.
    env.poll();
    assert_eq!(env.eval_int("fired"), 0);
    // After the constructor the trigger works normally.
    env.run("t.trigger();");
    env.poll();
    assert_eq!(env.eval_int("fired"), 1);
}

/// A queued fire retains its source object, like the reference `tTVPEvent`
/// constructor's `Target->AddRef()` / `Source->AddRef()`
/// (`reference/cpp/core/base/EventIntf.cpp:58-65`). A script may create the
/// trigger in a local and return before the next poll; the queued event must
/// still keep it alive and deliver `onFire`.
#[test]
fn a_queued_fire_keeps_a_local_trigger_alive() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var fired = 0; \
         function arm() { \
             var trigger = new AsyncTrigger(function(){ fired += 1; }, ''); \
             trigger.trigger(); \
         } \
         arm();",
    );
    assert_eq!(env.eval_int("fired"), 0);
    env.poll();
    // `arm()` returned before the poll, so only the posted event can retain
    // the trigger.
    assert_eq!(env.eval_int("fired"), 1);
}

/// The event's source reference is released when the fire is delivered:
/// `~tTVPEvent` calls `Target->Release()` / `Source->Release()`
/// (`reference/cpp/core/base/EventIntf.cpp:88-95`). A delivered local trigger
/// must therefore reach reference count zero and remove its binding.
#[test]
fn a_delivered_fire_releases_its_local_trigger() {
    let _lock = vm_lock();
    let env = Env::new();
    let baseline = env._host.async_trigger_count();
    env.run(
        "for (var cycle = 0; cycle < 5; cycle++) { \
             var trigger = new AsyncTrigger(function(){}, ''); \
             trigger.trigger(); \
             trigger = null; \
         }",
    );
    env.poll();
    env.run("System.doCompact();");
    env.engine.do_gc().expect("idle compact");
    assert_eq!(
        env._host.async_trigger_count(),
        baseline,
        "a delivered fire must release its source object and binding"
    );
}

/// The reference native async trigger keeps only a raw `Owner` pointer
/// (`reference/cpp/core/base/EventIntf.h:322`), so a script object reaches
/// reference count zero and its native destroy runs, removing the
/// `AsyncTriggerBinding`. A strong binding owner rooted the trigger, so each
/// dropped trigger leaked its binding; the state map is the observable form of
/// the "map size back to baseline" rule.
#[test]
fn dropped_async_triggers_release_their_bindings() {
    let _lock = vm_lock();
    let env = Env::new();
    let baseline = env._host.async_trigger_count();
    env.run(
        "for (var cycle = 0; cycle < 5; cycle++) { \
             var t = new AsyncTrigger(function(){}, ''); \
             t = null; \
         }",
    );
    // The collector must run outside the creating frame: a separate
    // execution's `doCompact` plus the direct GC pass.
    env.run("System.doCompact();");
    env.engine.do_gc().expect("idle compact");
    assert_eq!(
        env._host.async_trigger_count(),
        baseline,
        "a dropped AsyncTrigger must release its dispatch-state binding"
    );
}
