//! The proof driver: the corpus, through both paths.
//!
//! The driver runs the vendored compiler's own path and the adapter's path
//! over every script of the corpus and reports the differences. The
//! comparison itself is in the vendored tree's proof instrument; this driver
//! only feeds it and collects what it says.
//!
//! This example is a proof instrument. It is not part of the shipped tree.

use tjs as _;

use std::ffi::c_char;
use std::path::{Path, PathBuf};

unsafe extern "C" {
    fn tjs2_proof_dump(text: *const u16, length: u32, report: *mut c_char, report_size: u32)
    -> i32;

    fn tjs2_proof_values(
        first: *const u16,
        first_length: u32,
        second: *const u16,
        second_length: u32,
        which: i32,
        report: *mut c_char,
        report_size: u32,
    ) -> i32;

    fn tjs2_proof_bytecode(
        text: *const u16,
        length: u32,
        report: *mut c_char,
        report_size: u32,
    ) -> i32;

    fn tjs2_proof_run_reference(
        text: *const u16,
        length: u32,
        report: *mut c_char,
        report_size: u32,
    ) -> i32;

    fn tjs2_proof_run(text: *const u16, length: u32, report: *mut c_char, report_size: u32) -> i32;

    fn tjs2_proof_compare(
        text: *const u16,
        length: u32,
        report: *mut c_char,
        report_size: u32,
    ) -> i32;

    fn tjs2_proof_identity(report: *mut c_char, report_size: u32) -> i32;

    fn tjs2_proof_bytecode_compare(
        text: *const u16,
        length: u32,
        report: *mut c_char,
        report_size: u32,
    ) -> i32;

    fn tjs2_proof_lines(
        text: *const u16,
        length: u32,
        report: *mut c_char,
        report_size: u32,
    ) -> i32;
}

/// The units of a script file: the file holds UTF-16 code units.
fn units_of(path: &Path) -> Vec<u16> {
    let bytes = std::fs::read(path).expect("the script cannot be read");
    let mut start = 0;
    if bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] == 0xfe {
        start = 2;
    }
    let mut units = Vec::with_capacity(bytes.len() / 2);
    let mut index = start;
    while index + 1 < bytes.len() {
        units.push(u16::from_le_bytes([bytes[index], bytes[index + 1]]));
        index += 2;
    }
    units
}

/// Every script of a directory, in the order the walk finds them.
fn scripts_of(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let entries = std::fs::read_dir(&directory).expect("the directory cannot be read");
        for entry in entries {
            let path = entry.expect("the entry cannot be read").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().map(|ext| ext == "tjs").unwrap_or(false) {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn main() {
    let mut files: Vec<PathBuf> = Vec::new();
    for argument in std::env::args().skip(1) {
        let path = PathBuf::from(argument);
        if path.is_dir() {
            files.extend(scripts_of(&path));
        } else {
            files.push(path);
        }
    }

    // The identity of the constant inline objects: the instrument's own
    // scripts, through both paths.
    if std::env::var("PROOF_IDENTITY").is_ok() {
        let mut report = vec![0i8; 1 << 16];
        let differences = unsafe {
            // SAFETY: `report` is the 65536-byte buffer this driver owns and
            // keeps alive across the call; the instrument writes at most
            // `report.len()` bytes into it, so the call stays inside the
            // buffer.
            tjs2_proof_identity(report.as_mut_ptr() as *mut c_char, report.len() as u32)
        };
        let text = unsafe {
            // SAFETY: the instrument wrote a null-terminated string into
            // `report`, and `report` is still alive here, so the read stops
            // at that terminator inside the buffer.
            std::ffi::CStr::from_ptr(report.as_ptr() as *const c_char)
                .to_string_lossy()
                .into_owned()
        };
        if differences == 0 {
            println!("SAME identity");
        } else {
            println!("DIFF identity ({differences})");
        }
        print!("{text}");
        return;
    }

    for path in &files {
        let units = units_of(path);
        if let Ok(first) = std::env::var("PROOF_VALUES_FIRST") {
            let second = std::env::var("PROOF_VALUES_SECOND").unwrap_or_default();
            let one = units_of(Path::new(&first));
            let two = units_of(Path::new(&second));
            for which in 0..2 {
                let mut run = vec![0i8; 1 << 16];
                unsafe {
                    // SAFETY: `one` and `two` are the UTF-16 buffers this
                    // driver owns, and each length passed is its own buffer's
                    // length, so the instrument reads no unit outside them;
                    // `run` is the report buffer it fills with at most
                    // `run.len()` bytes.
                    tjs2_proof_values(
                        one.as_ptr(),
                        one.len() as u32,
                        two.as_ptr(),
                        two.len() as u32,
                        which,
                        run.as_mut_ptr() as *mut c_char,
                        run.len() as u32,
                    );
                }
                let text = unsafe {
                    // SAFETY: the instrument wrote a null-terminated string
                    // into `run`, and `run` is still alive here, so the read
                    // stops at that terminator inside the buffer.
                    std::ffi::CStr::from_ptr(run.as_ptr() as *const c_char)
                        .to_string_lossy()
                        .into_owned()
                };
                println!(
                    "VALUES {} (the {} path) -> {}",
                    path.display(),
                    if which == 0 { "reference" } else { "rust" },
                    text
                );
            }
            continue;
        }
        if std::env::var("PROOF_BYTECODE").is_ok() {
            let mut run = vec![0i8; 1 << 16];
            unsafe {
                // SAFETY: `units` is the UTF-16 buffer this driver owns, and
                // the length passed is that buffer's own length, so the
                // instrument reads no unit outside it; `run` is the report
                // buffer it fills with at most `run.len()` bytes.
                tjs2_proof_bytecode(
                    units.as_ptr(),
                    units.len() as u32,
                    run.as_mut_ptr() as *mut c_char,
                    run.len() as u32,
                );
            }
            let text = unsafe {
                // SAFETY: the instrument wrote a null-terminated string into
                // `run`, and `run` is still alive here, so the read stops at
                // that terminator inside the buffer.
                std::ffi::CStr::from_ptr(run.as_ptr() as *const c_char)
                    .to_string_lossy()
                    .into_owned()
            };
            println!("BYTECODE {} -> {}", path.display(), text);
            continue;
        }
        if std::env::var("PROOF_RUN_REFERENCE").is_ok() {
            let mut run = vec![0i8; 1 << 16];
            unsafe {
                // SAFETY: `units` is the UTF-16 buffer this driver owns, and
                // the length passed is that buffer's own length, so the
                // instrument reads no unit outside it; `run` is the report
                // buffer it fills with at most `run.len()` bytes.
                tjs2_proof_run_reference(
                    units.as_ptr(),
                    units.len() as u32,
                    run.as_mut_ptr() as *mut c_char,
                    run.len() as u32,
                );
            }
            let text = unsafe {
                // SAFETY: the instrument wrote a null-terminated string into
                // `run`, and `run` is still alive here, so the read stops at
                // that terminator inside the buffer.
                std::ffi::CStr::from_ptr(run.as_ptr() as *const c_char)
                    .to_string_lossy()
                    .into_owned()
            };
            println!("RUN-REFERENCE {} -> {}", path.display(), text);
            continue;
        }
        if std::env::var("PROOF_RUN").is_ok() {
            let mut run = vec![0i8; 1 << 16];
            unsafe {
                // SAFETY: `units` is the UTF-16 buffer this driver owns, and
                // the length passed is that buffer's own length, so the
                // instrument reads no unit outside it; `run` is the report
                // buffer it fills with at most `run.len()` bytes.
                tjs2_proof_run(
                    units.as_ptr(),
                    units.len() as u32,
                    run.as_mut_ptr() as *mut c_char,
                    run.len() as u32,
                );
            }
            let text = unsafe {
                // SAFETY: the instrument wrote a null-terminated string into
                // `run`, and `run` is still alive here, so the read stops at
                // that terminator inside the buffer.
                std::ffi::CStr::from_ptr(run.as_ptr() as *const c_char)
                    .to_string_lossy()
                    .into_owned()
            };
            println!("RUN {} -> {}", path.display(), text);
            continue;
        }
        if std::env::var("PROOF_DUMP").is_ok() {
            let mut dump = vec![0i8; 1 << 20];
            unsafe {
                // SAFETY: `units` is the UTF-16 buffer this driver owns, and
                // the length passed is that buffer's own length, so the
                // instrument reads no unit outside it; `dump` is the
                // megabyte-wide report buffer it fills with at most
                // `dump.len()` bytes.
                tjs2_proof_dump(
                    units.as_ptr(),
                    units.len() as u32,
                    dump.as_mut_ptr() as *mut c_char,
                    dump.len() as u32,
                );
            }
            let text = unsafe {
                // SAFETY: the instrument wrote a null-terminated string into
                // `dump`, and `dump` is still alive here, so the read stops
                // at that terminator inside the buffer.
                std::ffi::CStr::from_ptr(dump.as_ptr() as *const c_char)
                    .to_string_lossy()
                    .into_owned()
            };
            println!("DUMP {}", path.display());
            print!("{text}");
            continue;
        }
        if std::env::var("PROOF_BYTECODE_COMPARE").is_ok() {
            let mut report = vec![0i8; 1 << 20];
            let differences = unsafe {
                // SAFETY: `units` is the UTF-16 buffer this driver owns, and
                // the length passed is that buffer's own length, so the
                // instrument reads no unit outside it; `report` is the
                // megabyte-wide report buffer it fills with at most
                // `report.len()` bytes.
                tjs2_proof_bytecode_compare(
                    units.as_ptr(),
                    units.len() as u32,
                    report.as_mut_ptr() as *mut c_char,
                    report.len() as u32,
                )
            };
            let text = unsafe {
                // SAFETY: the instrument wrote a null-terminated string into
                // `report`, and `report` is still alive here, so the read
                // stops at that terminator inside the buffer.
                std::ffi::CStr::from_ptr(report.as_ptr() as *const c_char)
                    .to_string_lossy()
                    .into_owned()
            };
            let name = path.display().to_string();
            if differences == 0 {
                println!("SAME {name}");
            } else if differences.is_negative() {
                println!("FAIL {name}");
                print!("{text}");
            } else {
                println!("DIFF {name} ({differences})");
                print!("{text}");
            }
            continue;
        }
        if std::env::var("PROOF_LINES").is_ok() {
            let mut report = vec![0i8; 1 << 20];
            let differences = unsafe {
                // SAFETY: `units` is the UTF-16 buffer this driver owns, and
                // the length passed is that buffer's own length, so the
                // instrument reads no unit outside it; `report` is the
                // megabyte-wide report buffer it fills with at most
                // `report.len()` bytes.
                tjs2_proof_lines(
                    units.as_ptr(),
                    units.len() as u32,
                    report.as_mut_ptr() as *mut c_char,
                    report.len() as u32,
                )
            };
            let text = unsafe {
                // SAFETY: the instrument wrote a null-terminated string into
                // `report`, and `report` is still alive here, so the read
                // stops at that terminator inside the buffer.
                std::ffi::CStr::from_ptr(report.as_ptr() as *const c_char)
                    .to_string_lossy()
                    .into_owned()
            };
            let name = path.display().to_string();
            if differences == 0 {
                println!("SAME {name}");
            } else if differences.is_negative() {
                println!("FAIL {name}");
                print!("{text}");
            } else {
                println!("DIFF {name} ({differences})");
                print!("{text}");
            }
            continue;
        }
        let mut report = vec![0i8; 1 << 24];
        let differences = unsafe {
            // SAFETY: `units` is the UTF-16 buffer this driver owns, and the
            // length passed is that buffer's own length, so the instrument
            // reads no unit outside it; `report` is the 16-megabyte report
            // buffer it fills with at most `report.len()` bytes.
            tjs2_proof_compare(
                units.as_ptr(),
                units.len() as u32,
                report.as_mut_ptr() as *mut c_char,
                report.len() as u32,
            )
        };
        let text = unsafe {
            // SAFETY: the instrument wrote a null-terminated string into
            // `report`, and `report` is still alive here, so the read stops
            // at that terminator inside the buffer.
            std::ffi::CStr::from_ptr(report.as_ptr() as *const c_char)
                .to_string_lossy()
                .into_owned()
        };
        let name = path.display().to_string();
        if differences == 0 {
            println!("SAME {name}");
        } else if differences.is_negative() {
            println!("FAIL {name}");
            print!("{text}");
        } else {
            println!("DIFF {name} ({differences})");
            print!("{text}");
        }
    }
}
