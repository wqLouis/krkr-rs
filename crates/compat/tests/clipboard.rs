//! Integration tests for the `Clipboard` native class.
//!
//! `Clipboard` is a thin seam: L3 owns no clipboard, so these tests install a
//! stub backend through the public host seam
//! ([`CompatHost::set_clipboard_backend`]) and assert the script-visible
//! surface, then check the typed "no backend" error (D11). The stub is the
//! proof that the seam is reachable and that the native composes it rather
//! than a process global.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::vm_lock;
use compat::CompatHost;
use tjs::{Tjs2Engine, TjsValue};

/// A bound VM + host.
///
/// The engine is boxed so the binding's stored address stays valid when `Env`
/// is moved, and the host is declared **before** it so its `Drop` removes the
/// process-wide binding while the VM is still alive.
struct Env {
    host: CompatHost,
    engine: Box<Tjs2Engine>,
}

impl Env {
    fn new() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut host = CompatHost::new();
        host.install(&engine).expect("install compat surface");
        Env { host, engine }
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "clipboard-test")
            .map_err(|error| error.to_string())
    }

    fn eval_ok(&self, expression: &str) -> TjsValue {
        self.eval(expression)
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.eval_ok(expression) {
            TjsValue::Integer(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    fn error(&self, expression: &str) -> String {
        self.eval(expression)
            .expect_err(&format!("{expression:?} should have raised"))
    }
}

/// A recording stub backend: the seam's proof.
///
/// The three operations are installed as the closure tuple
/// [`CompatHost::set_clipboard_backend`] accepts, so the test never has to
/// name the internal trait.
#[derive(Default)]
struct StubClipboard {
    text: RefCell<Option<String>>,
    written: RefCell<Vec<String>>,
}

impl StubClipboard {
    fn install(host: &CompatHost) -> Rc<Self> {
        let stub = Rc::new(Self::default());
        let for_has = Rc::clone(&stub);
        let for_get = Rc::clone(&stub);
        let for_set = Rc::clone(&stub);
        host.set_clipboard_backend((
            // `hasFormat` only ever forwards `cbfText == 1`.
            move |format| format == 1 && for_has.text.borrow().is_some(),
            move || for_get.text.borrow().clone(),
            move |text: String| {
                *for_set.text.borrow_mut() = Some(text.clone());
                for_set.written.borrow_mut().push(text);
            },
        ));
        stub
    }
}

#[test]
fn members_without_a_backend_raise_typed_errors() {
    let _lock = vm_lock();
    let env = Env::new();

    // Every member is a typed "no backend" error, never a silent false/void.
    for expression in [
        "Clipboard.hasFormat(1)",
        "Clipboard.asText",
        "Clipboard.asText = 'x'",
    ] {
        let message = env.error(expression);
        assert!(
            message.contains("no clipboard backend is installed"),
            "{expression:?} gave an unexpected message: {message}"
        );
    }

    // Argument-count validation happens before the backend check.
    let message = env.error("Clipboard.hasFormat()");
    assert!(
        message.contains("requires 1 argument"),
        "unexpected message: {message}"
    );
}

#[test]
fn stub_backend_proves_the_seam_round_trips_text() {
    let _lock = vm_lock();
    let env = Env::new();
    let stub = StubClipboard::install(&env.host);

    // Empty stub: `asText` is void (reference), and `hasFormat(1)` is false.
    assert_eq!(env.eval_ok("Clipboard.asText"), TjsValue::Void);
    assert_eq!(env.eval_int("Clipboard.hasFormat(1)"), 0);

    // A write reaches the backend and a read comes back.
    env.engine
        .exec_script("Clipboard.asText = 'hello, world';", "clipboard-test")
        .expect("assign asText");
    assert_eq!(
        env.eval_ok("Clipboard.asText"),
        TjsValue::String("hello, world".into())
    );
    assert_eq!(env.eval_int("Clipboard.hasFormat(1)"), 1);
    assert_eq!(
        stub.written.borrow().as_slice(),
        ["hello, world".to_string()]
    );

    // Unknown formats are answered by the reference default (`false`)
    // without consulting the backend.
    assert_eq!(env.eval_int("Clipboard.hasFormat(2)"), 0);
    assert_eq!(env.eval_int("Clipboard.hasFormat(-1)"), 0);
}

#[test]
fn clearing_the_backend_text_reads_back_as_void() {
    let _lock = vm_lock();
    let env = Env::new();
    let stub = StubClipboard::install(&env.host);

    env.engine
        .exec_script("Clipboard.asText = 'temporary';", "clipboard-test")
        .expect("assign asText");
    assert_eq!(env.eval_int("Clipboard.hasFormat(1)"), 1);

    // The host clipboard is cleared behind the script's back; the getter
    // reports void rather than a fabricated empty string.
    *stub.text.borrow_mut() = None;
    assert_eq!(env.eval_ok("Clipboard.asText"), TjsValue::Void);
    assert_eq!(env.eval_int("Clipboard.hasFormat(1)"), 0);
}
