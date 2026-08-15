use tjs2_sys::Tjs2Engine;
fn main() {
    let e = Tjs2Engine::new().unwrap();
    for (label, code) in [
        ("exec expr", "1 + 2"),
        ("exec last expr", "var a = 3; a * 4;"),
        ("eval expr", "5 + 5"),
    ] {
        let r = if label.starts_with("eval") {
            e.eval(code, "p")
        } else {
            e.exec_script(code, "p")
        };
        println!(
            "{label}: {}",
            match r {
                Ok(v) => format!("OK {v:?}"),
                Err(err) => format!("ERR {err}"),
            }
        );
    }
}
