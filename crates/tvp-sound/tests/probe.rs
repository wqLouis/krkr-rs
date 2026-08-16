use engine::Storage;
use std::sync::{Arc, Mutex};
use tjs2_sys::Tjs2Engine;
use tvp_sound::register_sound;

#[test]
fn probe_script_class_shadowing_native() {
    let dir = std::env::temp_dir().join("probe-shadow");
    let _ = std::fs::create_dir_all(&dir);
    let storage = Arc::new(Mutex::new(Storage::mount(&dir).unwrap()));
    let e = Tjs2Engine::new().unwrap();
    register_sound(&e, storage).unwrap();
    let script = r#"
        class SoundBuffer extends WaveSoundBuffer {
            function SoundBuffer(owner){ WaveSoundBuffer(owner); }
            function open(){ super.open("x.wav"); }
            function test(){ return this.getStatus(); }
        }
        var sb = new SoundBuffer();
        var made = (sb instanceof SoundBuffer);
        var base = (sb instanceof WaveSoundBuffer);
    "#;
    match e.exec_script(script, "probe") {
        Ok(_) => eprintln!(
            "made={:?} base={:?}",
            e.eval("made", "probe").unwrap(),
            e.eval("base", "probe").unwrap()
        ),
        Err(err) => eprintln!("EXEC ERROR: {err}"),
    }
}
