//! The text-to-number rule against the answers recorded from its predecessor.
//!
//! The rule lives in `tjs::vm::runtime::number`. It was read out of the
//! engine's number parser, and `tests/fixtures/number_parse.txt` holds what
//! that parser answered for the corpus below: the kind, the value and the
//! number of code units the number occupies, for every text of the corpus.
//!
//! The corpus is generated here, in the same order, so the fixture names the
//! text with its answer and a mismatch names the text that differs. The record
//! was written from the rule's predecessor, and every run reads it back with
//! only the Rust rule.
//!
//! # The fixture
//!
//! One line per written or generated text: the text escaped, the kind
//! (`int`, `real` or `no`), the value and the extent. A real's value is the
//! bits of the double in hexadecimal, so the record is exact; a text that is
//! not a number has no value and no extent.
//!
//! The random texts are not written down: they are the generator's output, and
//! the fixture records one digest per block of [`RANDOM_BLOCK`] texts, folding
//! in the kind, the value and the extent of every answer. The answers of all of
//! them are pinned without the file holding the texts.

use std::fs;
use std::path::PathBuf;

/// How many texts the generated half of the corpus holds.
const RANDOM_TEXTS: usize = 300_000;

/// How many random texts one recorded digest covers.
const RANDOM_BLOCK: usize = 4_096;

/// The number a text answered: the kind and the value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answer {
    /// The text does not begin with a number.
    Absent,
    /// The text begins with an integer.
    Integer(i64),
    /// The text begins with a real, kept as its bits so the answer is exact.
    Real(u64),
}

impl Answer {
    /// The kind, as the digest folds it.
    fn kind(self) -> u64 {
        match self {
            Self::Absent => 0,
            Self::Integer(_) => 1,
            Self::Real(_) => 2,
        }
    }

    /// The value, as the digest folds it.
    fn value(self) -> u64 {
        match self {
            Self::Absent => 0,
            Self::Integer(value) => value as u64,
            Self::Real(bits) => bits,
        }
    }
}

/// An answer, and how many code units the number occupies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Parsed {
    /// The number the text begins with.
    answer: Answer,
    /// The number of code units it occupies; zero when there is no number.
    extent: usize,
}

/// The answer the rule in Rust gives.
fn rust(text: &[u16]) -> Parsed {
    let mut terminated = text.to_vec();
    terminated.push(0);
    let mut kind = -1;
    let mut integer = 0i64;
    let mut real = 0f64;
    let mut consumed = 0usize;
    // SAFETY: the text is terminated and every output is this call's own.
    let answered = unsafe {
        tjs::vm::tjs2_number_parse(
            terminated.as_ptr(),
            &mut kind,
            &mut integer,
            &mut real,
            &mut consumed,
        )
    };
    finish(answered, kind, integer, real, consumed)
}

/// The answer of the entry, from what it wrote.
fn finish(answered: i32, kind: i32, integer: i64, real: f64, consumed: usize) -> Parsed {
    if answered == 0 {
        return Parsed {
            answer: Answer::Absent,
            extent: 0,
        };
    }
    let answer = match kind {
        0 => Answer::Integer(integer),
        1 => Answer::Real(real.to_bits()),
        other => panic!("the entry answered an unknown kind {other}"),
    };
    Parsed {
        answer,
        extent: consumed,
    }
}

/// The corpus: the texts the fixture records, and the random texts it folds.
fn corpus() -> (Vec<Vec<u16>>, Vec<Vec<u16>>) {
    let mut recorded = written_texts();
    recorded.extend(generated_texts());
    (recorded, random_texts())
}

/// The texts every form of the rule is written as.
fn written_texts() -> Vec<Vec<u16>> {
    let mut texts: Vec<Vec<u16>> = Vec::new();
    for text in [
        // the empty text and the texts that hold no number
        "",
        " ",
        "   ",
        "12abc",
        ".",
        "..",
        "1.2.3",
        "0x",
        "0xg",
        "0b",
        "0b2",
        "0o17",
        "0O17",
        "0p",
        "0p1",
        "+",
        "-",
        "+ ",
        "- ",
        "+   ",
        // a sign and the spaces after it
        "+12",
        "-12",
        "+ 12",
        "-  12",
        " 12",
        "12 ",
        "+12x",
        "- 1.5e",
        // the bases
        "0",
        "7",
        "9",
        "00",
        "007",
        "0777",
        "08",
        "09",
        "0x0",
        "0x1f",
        "0X1F",
        "0x1z",
        "0b0",
        "0b1011",
        // the decimal forms
        "12",
        "12.5",
        ".5",
        "5.",
        "1e3",
        "1E3",
        "1e+3",
        "1e-3",
        "1e 3",
        "1e  3",
        "1e\t3",
        // the exponent marks with no digit after them
        "1e",
        "1E",
        "1e+",
        "1e-",
        "1.5e",
        "1.5e-",
        "1.5e+",
        "1.5E",
        "12e-",
        "12.5e",
        "0e",
        "0.5e",
        ".5e",
        "1e.5",
        "1e-.5",
        // the two-power exponents of the other bases
        "0x1.8p2",
        "0x1.8p+2",
        "0x1.8p-2",
        "0x1.8P2",
        "0x1p",
        "0x1p+",
        "0x1p-",
        "0x1p3",
        "0x1p999",
        "0b1.1p2",
        "0b1.1p-2",
        "0b1p",
        "0777p3",
        // the ends of the real range
        "1e400",
        "-1e400",
        "1e-400",
        "1e-323",
        "1e-324",
        "5e-324",
        "2.2250738585072014e-308",
        "2.2250738585072011e-308",
        "0.1",
        "0.30000000000000004",
        "9.090909090909093",
        "1.7976931348623157e308",
        "1.7976931348623159e308",
        "1e999999999999999999999",
        "1e-999999999999999999999",
        // the ends of the integer range
        "9223372036854775807",
        "9223372036854775808",
        "-9223372036854775808",
        "-9223372036854775809",
        "18446744073709551616",
        "99999999999999999999",
        "0xFFFFFFFFFFFFFFFF",
        "0xFFFFFFFFFFFFFFFFF",
        "0x8000000000000000",
        // the words the parser reads
        "true",
        "false",
        "NaN",
        "Infinity",
        "truex",
        "TRUE",
        "Infinit",
        "false0",
        "NaNx",
        "Infinity1",
    ] {
        texts.push(text.encode_utf16().collect());
    }
    // a long mantissa, and a long run of digits
    let mut long_mantissa = String::from("0.1234567890123456789012345678901234567890123456789");
    long_mantissa.push_str("0123456789");
    texts.push(long_mantissa.encode_utf16().collect());
    texts.push("1".repeat(60).encode_utf16().collect());
    texts.push(format!("0.{}e10", "9".repeat(60)).encode_utf16().collect());
    texts.push(format!("0b{}", "1".repeat(70)).encode_utf16().collect());
    // a code unit that stands where the conditional compilation removed text
    texts.push(vec![u16::from(b'1'), 0xffff, u16::from(b'2')]);
    texts.push(vec![0xffff, u16::from(b'1')]);
    texts.push(vec![
        u16::from(b'0'),
        u16::from(b'x'),
        0xffff,
        u16::from(b'1'),
    ]);
    texts.push(vec![u16::from(b'1'), u16::from(b'e'), 0xffff]);
    texts
}

/// The cross product of the signs, the bodies and the texts that follow them.
fn generated_texts() -> Vec<Vec<u16>> {
    const SIGNS: [&str; 7] = ["", "+", "-", "+ ", "- ", "+  ", "-   "];
    const BODIES: [&str; 22] = [
        "0", "7", "08", "12", "0777", "0x1f", "0X1F", "0x", "0xg", "0b1011", "0b2", "12.5", ".5",
        "5.", "1e3", "1e", "1.5e", "0x1.8p2", "0x1p", "abc", " 12", "1.5e 3",
    ];
    const TAILS: [&str; 6] = ["", "z", " 1", "\t2", "e", ".5"];
    let mut texts = Vec::new();
    for sign in SIGNS {
        for body in BODIES {
            for tail in TAILS {
                texts.push(format!("{sign}{body}{tail}").encode_utf16().collect());
            }
        }
    }
    texts
}

/// The texts the generator makes: a few hundred thousand of them.
fn random_texts() -> Vec<Vec<u16>> {
    // the units a text is built out of: the digits, the prefixes, the signs,
    // the marks, the spaces and the junk
    const ALPHABET: [u16; 31] = [
        0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, // 0-9
        0x2e, 0x65, 0x45, 0x70, 0x50, // . e E p P
        0x78, 0x58, 0x62, 0x42, // x X b B
        0x2b, 0x2d, // + -
        0x20, 0x09, 0x0b, // space, tab, vertical tab
        0x7a, 0x5f, 0x2c,   // z _ ,
        0xffff, // the unit the conditional compilation removes
        0x3000, 0x00a0, 0x3042, // an ideographic space, a no-break space, a letter
    ];
    let mut random = Lcg(0x5eed_1234_9abc_def1);
    let mut texts = Vec::with_capacity(RANDOM_TEXTS);
    for _ in 0..RANDOM_TEXTS {
        // a short text most of the time, a long one once in a while
        let length = if (random.next() & 0xff) == 0 {
            30 + (random.next() % 40) as usize
        } else {
            (random.next() % 14) as usize
        };
        let mut text = Vec::with_capacity(length);
        for _ in 0..length {
            text.push(ALPHABET[(random.next() % 31) as usize]);
        }
        texts.push(text);
    }
    texts
}

/// A 64-bit linear congruential generator: the corpus needs a repeatable
/// sequence, and the crate takes no random-number dependency.
struct Lcg(u64);

impl Lcg {
    /// The next value of the sequence, from the generator's high half.
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }
}

/// The text escaped: a printable character as itself, and any other unit by
/// its value.
fn escape(text: &[u16]) -> String {
    let mut escaped = String::with_capacity(text.len());
    for &unit in text {
        match unit {
            0x5c => escaped.push_str("\\\\"),
            0x20..=0x7e => escaped.push(char::from_u32(u32::from(unit)).expect("a printable unit")),
            _ => escaped.push_str(&format!("\\u{unit:04x}")),
        }
    }
    escaped
}

/// One recorded line: the text, the kind, the value and the extent.
fn record(text: &[u16], parsed: Parsed) -> String {
    match parsed.answer {
        Answer::Absent => format!("{}\tno\t-\t-", escape(text)),
        Answer::Integer(value) => format!("{}\tint\t{value}\t{}", escape(text), parsed.extent),
        Answer::Real(bits) => format!("{}\treal\t{bits:016x}\t{}", escape(text), parsed.extent),
    }
}

/// One digest per block of texts: the kind, the value and the extent of each
/// answer, folded in.
fn digests(texts: &[Vec<u16>]) -> Vec<u64> {
    let mut digests = Vec::new();
    for block in texts.chunks(RANDOM_BLOCK) {
        let mut digest = 0xcbf2_9ce4_8422_2325u64;
        for text in block {
            let parsed = rust(text);
            for part in [
                parsed.answer.kind(),
                parsed.answer.value(),
                parsed.extent as u64,
            ] {
                digest ^= part;
                digest = digest.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        digests.push(digest);
    }
    digests
}

/// The fixture's place.
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/number_parse.txt")
}

/// The fixture's recorded texts, and its block digests.
fn read_fixture(body: &str) -> (Vec<String>, Vec<u64>) {
    let mut texts = Vec::new();
    let mut blocks = Vec::new();
    for line in body.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match line.strip_prefix("block ") {
            Some(digest) => {
                blocks.push(u64::from_str_radix(digest, 16).unwrap_or_else(|error| {
                    panic!("the block digest {digest:?} is not hex: {error}")
                }))
            }
            None => texts.push(line.to_string()),
        }
    }
    (texts, blocks)
}

#[test]
fn the_number_rule_answers_the_recorded_answers() {
    let (recorded, random) = corpus();
    let path = fixture_path();
    let body = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} could not be read: {error}", path.display()));

    let (lines, blocks) = read_fixture(&body);
    assert_eq!(
        lines.len(),
        recorded.len(),
        "the fixture holds {} recorded texts and the corpus makes {}",
        lines.len(),
        recorded.len()
    );
    let mut wrong = Vec::new();
    for (text, line) in recorded.iter().zip(&lines) {
        let actual = record(text, rust(text));
        if &actual != line {
            wrong.push(format!("{actual:?} recorded as {line:?}"));
        }
    }
    if let Some(first) = wrong.first() {
        panic!(
            "{} texts answer what was not recorded; the first: {first}",
            wrong.len()
        );
    }
    assert_eq!(
        blocks,
        digests(&random),
        "the random texts answer what was not recorded"
    );
}
