//! Dump the contents of an XP3 archive.
//!
//! Usage:
//!   cargo run -p xp3 --example dump -- <archive.xp3> [--read <name>]
//!
//! Without `--read`, lists every entry (normalized name, org size, stored
//! size, segment count). With `--read <name>`, prints the first 256 bytes
//! of that entry as a hex dump. Useful for validating against real game
//! archives.

use std::process::ExitCode;

use xp3::Xp3Archive;

const HEX_ROW: usize = 16;
const MAX_DUMP: usize = 256;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: dump <archive.xp3> [--read <name>]");
        return ExitCode::FAILURE;
    };

    let mut read_name: Option<String> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--read" => match args.next() {
                Some(name) => read_name = Some(name),
                None => {
                    eprintln!("error: --read requires a name argument");
                    return ExitCode::FAILURE;
                }
            },
            other => {
                eprintln!("error: unknown argument `{other}`");
                return ExitCode::FAILURE;
            }
        }
    }

    let mut archive = match Xp3Archive::open(&path) {
        Ok(archive) => archive,
        Err(e) => {
            eprintln!("error: cannot open {path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("archive:   {path}");
    println!("base:      {}", archive.base_offset());
    println!("entries:   {}", archive.len());
    println!();
    println!(
        "{:<42} {:>12} {:>12} {:>5}",
        "name", "org_size", "arc_size", "segs"
    );
    println!("{}", "-".repeat(78));
    for entry in archive.entries() {
        let stored = if entry.raw_name != entry.name {
            format!("  (stored as: {})", entry.raw_name)
        } else {
            String::new()
        };
        println!(
            "{:<42} {:>12} {:>12} {:>5}{stored}",
            entry.name,
            entry.org_size,
            entry.arc_size,
            entry.segments.len()
        );
    }

    if let Some(name) = read_name {
        println!();
        match archive.read(&name) {
            Ok(data) => {
                println!(
                    "== {name}: {} bytes (first {MAX_DUMP} shown) ==",
                    data.len()
                );
                for (i, row) in data[..data.len().min(MAX_DUMP)].chunks(HEX_ROW).enumerate() {
                    let mut hex = String::new();
                    let mut ascii = String::new();
                    for (j, byte) in row.iter().enumerate() {
                        if j == HEX_ROW / 2 {
                            hex.push(' ');
                        }
                        hex.push_str(&format!("{byte:02x} "));
                        ascii.push(if byte.is_ascii_graphic() || *byte == b' ' {
                            *byte as char
                        } else {
                            '.'
                        });
                    }
                    println!("{:08x}  {:<50}  |{ascii}|", i * HEX_ROW, hex);
                }
            }
            Err(e) => {
                eprintln!("error: cannot read {name}: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    ExitCode::SUCCESS
}
