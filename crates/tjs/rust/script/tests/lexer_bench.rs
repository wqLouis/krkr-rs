//! The analyser's throughput over a corpus of scripts.
//!
//! The corpus is not part of the repository, so the test is ignored: run it
//! with the directory that holds the scripts in `KRKR_LEXER_CORPUS`.
//!
//! ```text
//! KRKR_LEXER_CORPUS=/path/to/scripts \
//!     cargo test -p script --test lexer_bench -- --ignored --nocapture
//! ```
//!
//! The scripts are read as UTF-16LE code units, the form the analyser
//! takes them in. The reading is outside the measured region: the
//! measurement is the analysis itself.

use std::path::{Path, PathBuf};
use std::time::Instant;

use script::lexer::Lexer;

/// The scripts of the corpus.
fn scripts() -> Vec<PathBuf> {
    let Some(root) = std::env::var_os("KRKR_LEXER_CORPUS") else {
        return Vec::new();
    };
    let root = Path::new(&root);
    let mut scripts: Vec<PathBuf> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|extension| extension == "tjs") {
                scripts.push(path);
            }
        }
    }
    scripts.sort();
    scripts
}

/// The code units of a script.
fn units(path: &Path) -> Vec<u16> {
    let bytes = std::fs::read(path).expect("the script is readable");
    let bytes = if bytes.starts_with(&[0xff, 0xfe]) {
        bytes[2..].to_vec()
    } else {
        bytes
    };
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from(pair[0]) | (u16::from(pair[1]) << 8))
        .collect()
}

/// The tokens of a script, and the failures the analyser reported.
///
/// A script that holds a regular expression the analyser was not told
/// about stops at the first failure, so the failures are counted and
/// reported: a silent truncation would make the measurement a lie.
fn count(units: &[u16]) -> (usize, usize) {
    let mut lexer = Lexer::new(units);
    let mut tokens = 0;
    let mut failures = 0;
    loop {
        match lexer.next_token() {
            Ok(Some(_token)) => tokens += 1,
            Ok(None) => break,
            Err(_) => {
                failures += 1;
                break;
            }
        }
    }
    (tokens, failures)
}

/// The analyser's throughput over the corpus.
#[test]
#[ignore = "the corpus is not part of the repository"]
fn throughput() {
    let scripts = scripts();
    assert!(!scripts.is_empty(), "KRKR_LEXER_CORPUS names no script");
    let rounds: usize = std::env::var("KRKR_LEXER_ROUNDS")
        .ok()
        .and_then(|rounds| rounds.parse().ok())
        .unwrap_or(5);

    let sources: Vec<(Vec<u16>, usize)> = scripts
        .iter()
        .map(|path| {
            let units = units(path);
            let (tokens, _) = count(&units);
            (units, tokens)
        })
        .collect();
    let total_units: usize = sources.iter().map(|(units, _)| units.len()).sum();
    let total_tokens: usize = sources.iter().map(|(_, tokens)| tokens).sum();
    let failures: usize = scripts.iter().map(|path| count(&units(path)).1).sum();

    let mut best = f64::INFINITY;
    for _ in 0..rounds {
        let start = Instant::now();
        let mut tokens = 0;
        for (units, _) in &sources {
            tokens += count(units).0;
        }
        let seconds = start.elapsed().as_secs_f64();
        assert_eq!(tokens, total_tokens);
        best = best.min(seconds);
    }

    println!(
        "scripts: {}, code units: {}, tokens: {}, scripts that stopped early: {}",
        sources.len(),
        total_units,
        total_tokens,
        failures
    );
    println!("best round: {best:.6} s");
    println!("tokens/s: {:.0}", total_tokens as f64 / best);
    println!("code units/s: {:.0}", total_units as f64 / best);
}
