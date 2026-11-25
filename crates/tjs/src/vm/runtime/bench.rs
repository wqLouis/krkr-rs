//! The value layer measured against the primitives it replaces.
//!
//! Every entry of the value layer is timed against the vendored primitive it
//! stands for, over the same corpus, in the same build, at the same
//! optimisation level, and the result is one line per primitive: the vendored
//! side's nanoseconds per operation, this side's, and the ratio (this side
//! over the vendored side, so below one is a win for this side).
//!
//! # The command
//!
//! ```text
//! cargo test -p tjs --release --lib vm::runtime::bench -- --ignored --nocapture
//! ```
//!
//! The build profile is part of the measurement: the vendored tree is
//! compiled with the profile's optimisation level (`-O2` in release, `-g` in
//! debug), and a debug build measures two things nobody runs. Take the
//! numbers on a quiet machine, and report the load average with them.
//!
//! Both sides are built at the level the engine itself is built with: the
//! vendored tree at `-O2` (the shipped level, which the `TJS2_OPT`
//! environment variable overrides) and this side at the release profile's
//! `opt-level = 3` with thin LTO. To hold both sides at one level as a second
//! opinion, run the same command with `TJS2_OPT=-O3`.
//!
//! # What is timed, and what is not
//!
//! Each side builds its corpus before the timed region, so the setup is
//! never mistaken for the operation. One warm-up pass runs on each side and
//! is discarded; then [`PASSES`] passes are measured and the best one per
//! side is reported, because the best pass is the one least disturbed by
//! another process and by the scheduler.
//!
//! The clock is read outside the loop. Every pass feeds a checksum that it
//! writes through a pointer, so the optimiser cannot delete the work, and
//! each iteration's corpus index goes through an optimisation barrier
//! ([`std::hint::black_box`], and the vendored side's assembly barrier), so
//! no iteration's work can be hoisted out of the loop or folded into a
//! constant.
//!
//! The two checksums are printed beside the ratio. A row marked `DIFFER` did
//! not do the same work on the two sides, and its ratio is not a measurement
//! of one operation; the checksums are the proof that the rows that are not
//! marked are.
//!
//! # What is not equalised
//!
//! Both sides make the allocation the operation itself needs, and neither
//! side's corpus is cached between passes. The storage differs by design: the
//! vendored text is a cell of a pool with its own count of holders, and this
//! side's is one shared allocation, so part of what the string rows measure is
//! exactly that difference. The table rows measure the two tables the same
//! way: the vendored one allocates an element per insertion and this one
//! appends to an arena, and both file a key under the same hash into the same
//! number of buckets.
//!
//! The three table rows are printed at two corpus sizes, because the fixed
//! bucket count makes the chain length the whole cost of a row: at 512 keys a
//! chain holds about eight entries, the size a big class's member map has, and
//! at 16384 it holds about 256, the size a dictionary reaches.

use std::cmp::Ordering;
use std::hint::black_box;
use std::time::Instant;

use super::hash::HashTable;
use super::number::Number;
use super::string::TjsString;

/// The operations one measured pass of the string rows runs.
const STRING_ITERATIONS: i64 = 1_000_000;

/// The operations one measured pass of the comparison row runs.
const COMPARE_ITERATIONS: i64 = 5_000_000;

/// The operations one measured pass of the hashing row runs.
const HASH_ITERATIONS: i64 = 5_000_000;

/// The operations one measured pass of the escape row runs.
const ESCAPE_ITERATIONS: i64 = 200_000;

/// The operations one measured pass of the number-to-text row runs.
const FORMAT_ITERATIONS: i64 = 1_000_000;

/// The keys the large table rows insert, look up and remove: chains of about
/// 256 entries over the fixed bucket count.
const TABLE_KEYS: i64 = 16_384;

/// The keys the small table rows insert, look up and remove: chains of about
/// eight entries over the fixed bucket count, the size a big class's member
/// map has.
const TABLE_KEYS_SMALL: i64 = 512;

/// The measured passes per side; the best one is reported.
const PASSES: usize = 3;

/// The member-name texts the comparison, hashing and table rows read: texts of
/// several lengths, a text that is a prefix of another, and the empty text.
const MEMBER_NAMES: [&str; 8] = [
    "x",
    "xy",
    "abc",
    "abd",
    "length",
    "member_1",
    "a longer member name",
    "",
];

/// The text the escape row escapes: the control units that have a named
/// escape, the backslash, both quote kinds, two control units followed by a
/// hexadecimal digit, and a unit above the control range.
const ESCAPED_UNITS: [u16; 14] = [
    0x61, 0x09, 0x62, 0x0a, 0x63, 0x5c, 0x22, 0x27, 0x01, 0x61, 0x02, 0x66, 0x7f, 0x7a,
];

/// One value the number-to-text row writes.
#[derive(Clone, Copy)]
enum FormatValue {
    /// An integer.
    Integer(i64),
    /// A real.
    Real(f64),
}

/// The values the number-to-text row writes: an integer, a real, a real with
/// no exact decimal form, a large and a small exponent form, both zeros and
/// both named forms.
const FORMAT_VALUES: [FormatValue; 8] = [
    FormatValue::Integer(1_234_567_890),
    FormatValue::Real(1.5),
    FormatValue::Real(1.0 / 3.0),
    FormatValue::Real(1e20),
    FormatValue::Real(1e-20),
    FormatValue::Real(-0.0),
    FormatValue::Real(f64::NAN),
    FormatValue::Real(f64::INFINITY),
];

/// One pass of one primitive: its elapsed nanoseconds and its checksum.
#[derive(Clone, Copy)]
struct Pass {
    /// The elapsed nanoseconds of the pass.
    ns: i64,
    /// The checksum the pass fed.
    checksum: u64,
}

/// Prints the table's header.
fn print_header() {
    println!("value layer against the vendored primitives: nanoseconds per operation");
    println!(
        "best of {PASSES} measured passes after one discarded warm-up per side; release build only"
    );
    let primitive = "primitive";
    let vendored = "vendored";
    let rust = "rust";
    let ratio = "ratio";
    println!("{primitive:<26} {vendored:>10} {rust:>10} {ratio:>7}  answers");
}

/// Times one primitive on both sides and prints its row.
///
/// `operations` is the number of operations one pass performs, which both
/// sides are given; the ratio is this side's nanoseconds per operation over
/// the vendored side's.
fn print_row(
    name: &str,
    operations: i64,
    mut vendored: impl FnMut() -> Pass,
    mut column: impl FnMut() -> Pass,
) {
    let _warm_vendored = vendored();
    let _warm_column = column();
    let mut vendored_best = Pass {
        ns: i64::MAX,
        checksum: 0,
    };
    let mut column_best = Pass {
        ns: i64::MAX,
        checksum: 0,
    };
    for _ in 0..PASSES {
        let pass = vendored();
        if pass.ns < vendored_best.ns {
            vendored_best = pass;
        }
        let pass = column();
        if pass.ns < column_best.ns {
            column_best = pass;
        }
    }
    let operations = operations.max(1) as f64;
    let vendored_ns = vendored_best.ns as f64 / operations;
    let column_ns = column_best.ns as f64 / operations;
    let ratio = column_ns / vendored_ns;
    let answers = if vendored_best.checksum == column_best.checksum {
        String::from("same")
    } else {
        let vendored = vendored_best.checksum;
        let column = column_best.checksum;
        format!("DIFFER {vendored:#018x} {column:#018x}")
    };
    println!("{name:<26} {vendored_ns:>10.2} {column_ns:>10.2} {ratio:>7.3}  {answers}");
}

/// The nanoseconds from `start` to now.
fn elapsed(start: Instant) -> i64 {
    start.elapsed().as_nanos() as i64
}

/// The `count` keys the table rows use: `member_0` upwards.
fn member_names(count: usize) -> Vec<TjsString> {
    (0..count)
        .map(|index| TjsString::from(format!("member_{index}")))
        .collect()
}

/// A table holding every key with its index as the value.
fn filled_table(keys: &[TjsString]) -> HashTable<TjsString, i32> {
    let mut table = HashTable::new();
    for (index, key) in keys.iter().enumerate() {
        // a fresh table holds no key yet, so the answer is always `None`
        let _replaced = table.insert(key.clone(), index as i32);
    }
    table
}

/// The pass a measurement entry answered.
fn pass((ns, checksum): (i64, u64)) -> Pass {
    Pass { ns, checksum }
}

/// Times the vendored concatenation.
fn vendored_concat() -> Pass {
    pass(crate::run_bench_entry(
        crate::tjs2_bench_string_concat,
        STRING_ITERATIONS,
    ))
}

/// Times the vendored construction of a text from a literal.
fn vendored_construct() -> Pass {
    pass(crate::run_bench_entry(
        crate::tjs2_bench_string_construct,
        STRING_ITERATIONS,
    ))
}

/// Times the vendored text comparison.
fn vendored_compare() -> Pass {
    pass(crate::run_bench_entry(
        crate::tjs2_bench_string_compare,
        COMPARE_ITERATIONS,
    ))
}

/// Times the vendored member hash.
fn vendored_hash() -> Pass {
    pass(crate::run_bench_entry(
        crate::tjs2_bench_string_hash,
        HASH_ITERATIONS,
    ))
}

/// Times the vendored escape rule.
fn vendored_escape() -> Pass {
    pass(crate::run_bench_entry(
        crate::tjs2_bench_string_escape,
        ESCAPE_ITERATIONS,
    ))
}

/// Times the vendored number-to-text rule.
fn vendored_format() -> Pass {
    pass(crate::run_bench_entry(
        crate::tjs2_bench_number_format,
        FORMAT_ITERATIONS,
    ))
}

/// Times the vendored table insertions over `count` keys.
fn vendored_table_insert(count: i64) -> Pass {
    pass(crate::run_bench_entry(
        crate::tjs2_bench_table_insert,
        count,
    ))
}

/// Times the vendored table lookups over `count` keys.
fn vendored_table_find(count: i64) -> Pass {
    pass(crate::run_bench_entry(crate::tjs2_bench_table_find, count))
}

/// Times the vendored table removals over `count` keys.
fn vendored_table_remove(count: i64) -> Pass {
    pass(crate::run_bench_entry(
        crate::tjs2_bench_table_remove,
        count,
    ))
}

/// Prints one row per primitive: the vendored side, this side, and the ratio.
///
/// The command and the corpus are in the module documentation. Every row
/// prints the two checksums' verdict: `same` means both sides did the same
/// work and answered the same, and a row marked `DIFFER` is not a
/// measurement of one operation.
#[test]
#[ignore = "a measurement, not a test: release build, quiet machine"]
fn print_value_layer_table() {
    print_header();

    let left = TjsString::from("a member name of twenty-one units");
    let right = TjsString::from(".bitmap");
    print_row("string concat", STRING_ITERATIONS, vendored_concat, || {
        let mut checksum: u64 = 0;
        let start = Instant::now();
        for _ in 0..STRING_ITERATIONS {
            let text = black_box(&left).concat(&right);
            checksum = checksum
                .wrapping_mul(31)
                .wrapping_add(text.len() as u64 * 31 + u64::from(text.as_utf16()[text.len() - 1]));
        }
        Pass {
            ns: elapsed(start),
            checksum,
        }
    });

    let constructed = "member_1234567890";
    print_row(
        "string construct",
        STRING_ITERATIONS,
        vendored_construct,
        || {
            let mut checksum: u64 = 0;
            let start = Instant::now();
            for _ in 0..STRING_ITERATIONS {
                let text = TjsString::from(black_box(constructed));
                checksum = checksum.wrapping_mul(31).wrapping_add(text.len() as u64);
            }
            Pass {
                ns: elapsed(start),
                checksum,
            }
        },
    );

    let names: Vec<TjsString> = MEMBER_NAMES
        .iter()
        .map(|text| TjsString::from(*text))
        .collect();
    print_row(
        "string compare",
        COMPARE_ITERATIONS,
        vendored_compare,
        || {
            let mut checksum: u64 = 0;
            let start = Instant::now();
            for iteration in 0..COMPARE_ITERATIONS as usize {
                let index = black_box(iteration % names.len());
                let other = (index * 5 + 1) % names.len();
                let order = match names[index].cmp(&names[other]) {
                    Ordering::Less => 1u64,
                    Ordering::Equal => 2,
                    Ordering::Greater => 3,
                };
                checksum = checksum.wrapping_mul(31).wrapping_add(order);
            }
            Pass {
                ns: elapsed(start),
                checksum,
            }
        },
    );

    print_row("member hash", HASH_ITERATIONS, vendored_hash, || {
        let mut checksum: u64 = 0;
        let start = Instant::now();
        for iteration in 0..HASH_ITERATIONS as usize {
            let index = black_box(iteration % names.len());
            checksum = checksum
                .wrapping_mul(31)
                .wrapping_add(u64::from(names[index].member_hash()));
        }
        Pass {
            ns: elapsed(start),
            checksum,
        }
    });

    let escaped = TjsString::from(&ESCAPED_UNITS[..]);
    print_row("string escape", ESCAPE_ITERATIONS, vendored_escape, || {
        let mut checksum: u64 = 0;
        let start = Instant::now();
        for _ in 0..ESCAPE_ITERATIONS {
            let text = black_box(&escaped).escape_c();
            checksum = checksum
                .wrapping_mul(31)
                .wrapping_add(text.len() as u64 * 31 + u64::from(text.as_utf16()[text.len() - 1]));
        }
        Pass {
            ns: elapsed(start),
            checksum,
        }
    });

    print_row("number to text", FORMAT_ITERATIONS, vendored_format, || {
        let mut checksum: u64 = 0;
        let start = Instant::now();
        for iteration in 0..FORMAT_ITERATIONS as usize {
            let index = black_box(iteration % FORMAT_VALUES.len());
            let text = match FORMAT_VALUES[index] {
                FormatValue::Integer(value) => Number::Integer(value).to_string(),
                FormatValue::Real(value) => Number::Real(value).to_string(),
            };
            // a formatted number is ASCII, so its first byte is its first
            // code unit
            checksum = checksum
                .wrapping_mul(31)
                .wrapping_add(text.len() as u64 * 31 + u64::from(text.as_bytes()[0]));
        }
        Pass {
            ns: elapsed(start),
            checksum,
        }
    });

    print_table_rows(
        ["table insert", "table find", "table remove"],
        &member_names(TABLE_KEYS as usize),
    );
    print_table_rows(
        ["table_insert_512", "table_find_512", "table_remove_512"],
        &member_names(TABLE_KEYS_SMALL as usize),
    );
}

/// Prints the three table rows for `keys`: insertion, lookup and removal.
///
/// Both sides build the same keys for the size, so the two columns file the
/// same hash into the same buckets; the rows are printed once per size.
fn print_table_rows(names: [&str; 3], keys: &[TjsString]) {
    let operations = keys.len() as i64;
    print_row(
        names[0],
        operations,
        || vendored_table_insert(operations),
        || {
            let mut table: HashTable<TjsString, i32> = HashTable::new();
            let mut checksum: u64 = 0;
            let start = Instant::now();
            for (index, key) in keys.iter().enumerate() {
                // a fresh table holds no key yet, so the answer is always `None`
                let _replaced = table.insert(key.clone(), index as i32);
                checksum = checksum.wrapping_mul(31).wrapping_add(key.len() as u64);
            }
            let ns = elapsed(start);
            // the lookup after the timed region keeps the table observable
            let last = table.get(&keys[keys.len() - 1]);
            checksum = checksum
                .wrapping_mul(31)
                .wrapping_add(last.copied().map_or(0, |value| value as u64));
            Pass { ns, checksum }
        },
    );

    print_row(
        names[1],
        operations,
        || vendored_table_find(operations),
        || {
            let table = filled_table(keys);
            let mut checksum: u64 = 0;
            let start = Instant::now();
            for key in keys {
                checksum = checksum
                    .wrapping_mul(31)
                    .wrapping_add(table.get(key).copied().map_or(0, |value| value as u64));
            }
            Pass {
                ns: elapsed(start),
                checksum,
            }
        },
    );

    print_row(
        names[2],
        operations,
        || vendored_table_remove(operations),
        || {
            let mut table = filled_table(keys);
            let mut checksum: u64 = 0;
            let start = Instant::now();
            for key in keys {
                checksum = checksum
                    .wrapping_mul(31)
                    .wrapping_add(u64::from(table.remove(key)));
            }
            Pass {
                ns: elapsed(start),
                checksum,
            }
        },
    );
}
