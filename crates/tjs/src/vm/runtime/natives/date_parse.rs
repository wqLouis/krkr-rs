//! The date text grammar: the forms a date is read from, and their refusals.
//!
//! The grammar is a fixed order of date, time and zone tokens, with optional
//! weekday and punctuation. Its forms are:
//!
//! | form | example |
//! |---|---|
//! | day month year time zone | `Sun, 3 May 2004 11:22:33 GMT +900` |
//! | day-month year time zone | `Sun, 3-May 2004 11:22:33 GMT +900` |
//! | day-month-year time zone | `Sun, 3-May-2004 11:22:33 GMT +900` |
//! | month day year time zone | `Sun, May 3 2004 11:22:33 GMT +900` |
//! | month-day year time zone | `Sun, May-3 2004 11:22:33 GMT +900` |
//! | month-day-year time zone | `Sun, May-3-2004 11:22:33 GMT +900` |
//! | day month time year zone | `Sun, 3 May 11:22:33 2004 GMT +900` |
//! | day-month time year zone | `Sun, 3-May 11:22:33 2004 GMT +900` |
//! | month day time year zone | `Sun, May 3 11:22:33 2004 GMT +900` |
//! | month-day time year zone | `Sun, May-3 11:22:33 2004 GMT +900` |
//! | year-month-day time zone | `2004/03/03 11:22:33 GMT +900` |
//!
//! The weekday is optional and ignored, with or without its comma. A month is
//! named in words or abbreviated, with an optional period; the names are
//! matched without regard to ASCII case. The time is `hour:minute`, with an
//! optional `:second` and an optional fraction after the second; an `am` or
//! `pm` may stand before or after the time, and `pm` adds twelve hours. A year
//! below 100 is read as 2000 through 2050 for 0 through 50 and 1900 through
//! 1999 for 51 through 99.
//!
//! The zone is optional. It may be a name (`GMT`, `JST`, `EST`, …), a signed
//! four-digit offset (`+900`, `-500`), both (they add), and a parenthesised
//! description that is skipped. Without a zone the fields are in the system
//! zone at `system_offset`.
//!
//! Every other arrangement is refused: a missing date field, a missing time,
//! an unknown word, a trailing token, an unterminated description, or a
//! field set that names no instant. A refusal is [`DateError::CannotParse`]
//! or [`DateError::InvalidTimestamp`], never a guessed date.

use super::date::{self, DateError, Fields};

/// A word the grammar knows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Word {
    /// A month, counted from zero.
    Month(i32),
    /// A day of the week.
    Weekday,
    /// A zone written as `[+/-]hhmm`.
    TimeZone(i32),
    /// The morning marker.
    Am,
    /// The afternoon marker.
    Pm,
}

/// The words the lexer matches, longest prefix first at each position.
///
/// The keys are lower case; the lexer folds an ASCII upper-case unit before
/// comparing. A key matches only when the unit after it is not a letter, so
/// `jan` is a month and `janx` is not.
const WORDS: &[(&str, Word)] = &[
    ("jst", Word::TimeZone(900)),
    ("ut", Word::TimeZone(0)),
    ("utc", Word::TimeZone(0)),
    ("gmt", Word::TimeZone(0)),
    ("est", Word::TimeZone(-500)),
    ("edt", Word::TimeZone(-400)),
    ("cst", Word::TimeZone(-600)),
    ("cdt", Word::TimeZone(-500)),
    ("mst", Word::TimeZone(-700)),
    ("mdt", Word::TimeZone(-600)),
    ("pst", Word::TimeZone(-800)),
    ("pdt", Word::TimeZone(-700)),
    ("z", Word::TimeZone(0)),
    ("a", Word::TimeZone(-100)),
    ("m", Word::TimeZone(-1200)),
    ("n", Word::TimeZone(100)),
    ("y", Word::TimeZone(1200)),
    ("nzdt", Word::TimeZone(1300)),
    ("idle", Word::TimeZone(1200)),
    ("nzst", Word::TimeZone(1200)),
    ("nzt", Word::TimeZone(1200)),
    ("aesst", Word::TimeZone(1100)),
    ("acsst", Word::TimeZone(1030)),
    ("cadt", Word::TimeZone(1030)),
    ("sadt", Word::TimeZone(1030)),
    ("aest", Word::TimeZone(1000)),
    ("east", Word::TimeZone(1000)),
    ("gst", Word::TimeZone(1000)),
    ("ligt", Word::TimeZone(1000)),
    ("acst", Word::TimeZone(930)),
    ("sast", Word::TimeZone(930)),
    ("cast", Word::TimeZone(930)),
    ("awsst", Word::TimeZone(900)),
    ("kst", Word::TimeZone(900)),
    ("wdt", Word::TimeZone(900)),
    ("mt", Word::TimeZone(830)),
    ("awst", Word::TimeZone(800)),
    ("cct", Word::TimeZone(800)),
    ("wadt", Word::TimeZone(800)),
    ("wst", Word::TimeZone(800)),
    ("jt", Word::TimeZone(730)),
    ("wast", Word::TimeZone(700)),
    ("it", Word::TimeZone(330)),
    ("bt", Word::TimeZone(300)),
    ("eetdst", Word::TimeZone(300)),
    ("cetdst", Word::TimeZone(200)),
    ("eet", Word::TimeZone(200)),
    ("fwt", Word::TimeZone(200)),
    ("ist", Word::TimeZone(200)),
    ("mest", Word::TimeZone(200)),
    ("metdst", Word::TimeZone(200)),
    ("sst", Word::TimeZone(200)),
    ("bst", Word::TimeZone(100)),
    ("cet", Word::TimeZone(100)),
    ("dnt", Word::TimeZone(100)),
    ("fst", Word::TimeZone(100)),
    ("met", Word::TimeZone(100)),
    ("mewt", Word::TimeZone(100)),
    ("mez", Word::TimeZone(100)),
    ("nor", Word::TimeZone(100)),
    ("set", Word::TimeZone(100)),
    ("swt", Word::TimeZone(100)),
    ("wetdst", Word::TimeZone(100)),
    ("wet", Word::TimeZone(0)),
    ("wat", Word::TimeZone(-100)),
    ("ndt", Word::TimeZone(-230)),
    ("adt", Word::TimeZone(-300)),
    ("nft", Word::TimeZone(-330)),
    ("nst", Word::TimeZone(-330)),
    ("ast", Word::TimeZone(-400)),
    ("ydt", Word::TimeZone(-800)),
    ("hdt", Word::TimeZone(-900)),
    ("ahst", Word::TimeZone(-1000)),
    ("cat", Word::TimeZone(-1000)),
    ("nt", Word::TimeZone(-1100)),
    ("idlw", Word::TimeZone(-1200)),
    ("sun", Word::Weekday),
    ("sun.", Word::Weekday),
    ("sunday", Word::Weekday),
    ("mon", Word::Weekday),
    ("mon.", Word::Weekday),
    ("monday", Word::Weekday),
    ("tue", Word::Weekday),
    ("tue.", Word::Weekday),
    ("tues", Word::Weekday),
    ("tues.", Word::Weekday),
    ("tuesday", Word::Weekday),
    ("wed", Word::Weekday),
    ("wed.", Word::Weekday),
    ("wednesday", Word::Weekday),
    ("thu", Word::Weekday),
    ("thu.", Word::Weekday),
    ("thurs", Word::Weekday),
    ("thurs.", Word::Weekday),
    ("thursday", Word::Weekday),
    ("fri", Word::Weekday),
    ("fri.", Word::Weekday),
    ("friday", Word::Weekday),
    ("sat", Word::Weekday),
    ("sat.", Word::Weekday),
    ("saturday", Word::Weekday),
    ("jan", Word::Month(0)),
    ("jan.", Word::Month(0)),
    ("january", Word::Month(0)),
    ("feb", Word::Month(1)),
    ("feb.", Word::Month(1)),
    ("february", Word::Month(1)),
    ("mar", Word::Month(2)),
    ("mar.", Word::Month(2)),
    ("march", Word::Month(2)),
    ("apr", Word::Month(3)),
    ("apr.", Word::Month(3)),
    ("april", Word::Month(3)),
    ("may", Word::Month(4)),
    ("ju", Word::Month(5)),
    ("ju.", Word::Month(5)),
    ("jun", Word::Month(5)),
    ("jun.", Word::Month(5)),
    ("june", Word::Month(5)),
    ("jul", Word::Month(6)),
    ("jul.", Word::Month(6)),
    ("july", Word::Month(6)),
    ("aug", Word::Month(7)),
    ("aug.", Word::Month(7)),
    ("august", Word::Month(7)),
    ("sep", Word::Month(8)),
    ("sep.", Word::Month(8)),
    ("sept", Word::Month(8)),
    ("sept.", Word::Month(8)),
    ("september", Word::Month(8)),
    ("oct", Word::Month(9)),
    ("oct.", Word::Month(9)),
    ("october", Word::Month(9)),
    ("nov", Word::Month(10)),
    ("nov.", Word::Month(10)),
    ("november", Word::Month(10)),
    ("dec", Word::Month(11)),
    ("dec.", Word::Month(11)),
    ("december", Word::Month(11)),
    ("am", Word::Am),
    ("pm", Word::Pm),
];

/// One lexical token.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Token {
    /// A run of decimal digits, read with the 32-bit wrapping of a field.
    Number(i32),
    /// A month name.
    Month(i32),
    /// A weekday name.
    Weekday,
    /// A zone name, as `[+/-]hhmm`.
    TimeZone(i32),
    /// The morning marker.
    Am,
    /// The afternoon marker.
    Pm,
    /// One other code unit, with an ASCII upper-case letter folded down.
    Char(u16),
    /// The end of the text.
    End,
}

/// The fields the grammar sets as it reads, and the zone it read.
#[derive(Default)]
struct Settings {
    /// The year, after the two-digit rule.
    year: Option<i32>,
    /// The month, counted from zero.
    month: Option<i32>,
    /// The day of the month.
    day: Option<i32>,
    /// The hour.
    hour: Option<i32>,
    /// The minute.
    minute: Option<i32>,
    /// The second; zero when the text omits it.
    second: Option<i32>,
    /// The afternoon marker: `Some(true)` is `pm`, `Some(false)` is `am`.
    am_pm: Option<bool>,
    /// A zone name, as `[+/-]hhmm`.
    zone: Option<i32>,
    /// A signed zone offset, as `[+/-]hhmm`, already signed.
    zone_offset: Option<i32>,
}

/// Reads a date from text.
///
/// The text is UTF-16 code units, and ends at the first zero unit. A zone the
/// text does not name is `system_offset`, the system zone's standard offset in
/// seconds east of UTC; the same offset places the fields before the text's
/// own zone is applied, so the invalid-instant refusal is the system zone's.
///
/// # Errors
///
/// [`DateError::CannotParse`] when the text is not one of the grammar's forms,
/// and [`DateError::InvalidTimestamp`] when its fields name no instant.
pub fn parse(text: &[u16], system_offset: i64) -> Result<i64, DateError> {
    let tokens = Lexer::new(text).tokens();
    let mut parser = Parser {
        tokens,
        at: 0,
        settings: Settings::default(),
    };
    parser.entry()?;
    finish(parser.settings, system_offset)
}

/// Places the fields, applies the text's zone and answers the instant.
fn finish(settings: Settings, system_offset: i64) -> Result<i64, DateError> {
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute)) = (
        settings.year,
        settings.month,
        settings.day,
        settings.hour,
        settings.minute,
    ) else {
        return Err(DateError::CannotParse);
    };
    let second = settings.second.unwrap_or(0);
    let hour = if matches!(settings.am_pm, Some(true)) {
        hour.wrapping_add(12)
    } else {
        hour
    };
    let zone = settings.zone.map(zone_seconds).unwrap_or(0);
    let zone_offset = settings.zone_offset.map(zone_seconds).unwrap_or(0);
    let declared = if settings.zone.is_none() && settings.zone_offset.is_none() {
        system_offset
    } else {
        zone + zone_offset
    };
    let fields = Fields {
        year: i64::from(year),
        month: i64::from(month),
        day: i64::from(day),
        hour: i64::from(hour),
        minute: i64::from(minute),
        second: i64::from(second),
        weekday: 0,
    };
    let naive = date::naive_seconds(&fields)?;
    let placed = naive
        .checked_sub(system_offset)
        .ok_or(DateError::InvalidTimestamp)?;
    if placed == -1 {
        return Err(DateError::InvalidTimestamp);
    }
    naive
        .checked_sub(declared)
        .ok_or(DateError::InvalidTimestamp)
}

/// The seconds a `[+/-]hhmm` zone names.
fn zone_seconds(value: i32) -> i64 {
    let magnitude = i64::from(value).abs();
    let seconds = (magnitude / 100) * 3_600 + (magnitude % 100) * 60;
    if value < 0 { -seconds } else { seconds }
}

/// The tokens of one text.
struct Lexer<'a> {
    /// The code units, not including a zero after the text.
    units: &'a [u16],
    /// The next unit to read.
    at: usize,
}

impl<'a> Lexer<'a> {
    /// The lexer over a text.
    fn new(units: &'a [u16]) -> Self {
        Self { units, at: 0 }
    }

    /// Every token, in order.
    fn tokens(mut self) -> Vec<Token> {
        let mut tokens = Vec::new();
        loop {
            let token = self.next();
            if token == Token::End {
                break;
            }
            tokens.push(token);
        }
        tokens
    }

    /// The next token, or [`Token::End`] at the end of the text.
    fn next(&mut self) -> Token {
        loop {
            let Some(&unit) = self.units.get(self.at) else {
                return Token::End;
            };
            if unit == 0 {
                return Token::End;
            }
            if !is_space(unit) {
                break;
            }
            self.at += 1;
        }
        let unit = self.units[self.at];
        if is_digit(unit) {
            let mut value: i32 = 0;
            while let Some(&unit) = self.units.get(self.at) {
                if !is_digit(unit) {
                    break;
                }
                let digit = i32::from(unit - u16::from(b'0'));
                value = value.wrapping_mul(10).wrapping_add(digit);
                self.at += 1;
            }
            return Token::Number(value);
        }
        if let Some((length, word)) = word_at(self.units, self.at) {
            self.at += length;
            return match word {
                Word::Month(value) => Token::Month(value),
                Word::Weekday => Token::Weekday,
                Word::TimeZone(value) => Token::TimeZone(value),
                Word::Am => Token::Am,
                Word::Pm => Token::Pm,
            };
        }
        self.at += 1;
        Token::Char(lower(unit))
    }
}

/// The grammar's parser over one token list.
struct Parser {
    /// The tokens, with no end marker.
    tokens: Vec<Token>,
    /// The next token to read.
    at: usize,
    /// The fields read so far.
    settings: Settings,
}

impl Parser {
    /// Reads the one date form.
    fn entry(&mut self) -> Result<(), DateError> {
        if let Some(Token::Weekday) = self.peek() {
            self.take();
            self.eat_char(b',');
        }
        match self.peek() {
            Some(Token::Month(_)) => self.month_first()?,
            Some(Token::Number(_)) => self.number_first()?,
            _ => return Err(DateError::CannotParse),
        }
        self.timezone()?;
        if self.peek().is_some() {
            return Err(DateError::CannotParse);
        }
        Ok(())
    }

    /// The form that starts with a month name.
    fn month_first(&mut self) -> Result<(), DateError> {
        let month = self.take_month()?;
        self.settings.month = Some(month);
        let dashed = self.eat_char(b'-');
        let day = self.take_number()?;
        self.settings.day = Some(day);
        self.after_month_and_day(dashed)
    }

    /// The form that starts with a number: a day before the month, or a year
    /// in the year-month-day form.
    fn number_first(&mut self) -> Result<(), DateError> {
        let first = self.take_number()?;
        match self.peek() {
            Some(Token::Char(unit)) if unit == u16::from(b'-') => {
                self.take();
                match self.peek() {
                    Some(Token::Month(_)) => {
                        let month = self.take_month()?;
                        self.settings.month = Some(month);
                        self.settings.day = Some(first);
                        self.after_month_and_day(true)
                    }
                    Some(Token::Number(_)) => {
                        let month = self.take_number()?;
                        self.settings.month = Some(month.wrapping_sub(1));
                        self.year_month_day(first)
                    }
                    _ => Err(DateError::CannotParse),
                }
            }
            Some(Token::Char(unit)) if unit == u16::from(b'/') => {
                self.take();
                let month = self.take_number()?;
                self.settings.month = Some(month.wrapping_sub(1));
                self.year_month_day(first)
            }
            Some(Token::Month(_)) => {
                let month = self.take_month()?;
                self.settings.month = Some(month);
                self.settings.day = Some(first);
                self.after_month_and_day(false)
            }
            _ => Err(DateError::CannotParse),
        }
    }

    /// The year-month-day form: the year is already read, the month and the
    /// day follow across one of `-` and `/`.
    fn year_month_day(&mut self, year: i32) -> Result<(), DateError> {
        self.expect_hyphen_or_slash()?;
        let day = self.take_number()?;
        self.set_year(year);
        self.settings.day = Some(day);
        self.time()
    }

    /// The fields after a month and its day: an optional second dash, then
    /// either the year before the time or the time before the year.
    ///
    /// The second dash is available only when the month and the day were
    /// joined by a dash; without it the year and the time follow directly.
    fn after_month_and_day(&mut self, dashed: bool) -> Result<(), DateError> {
        if dashed && self.eat_char(b'-') {
            let year = self.take_number()?;
            self.set_year(year);
            return self.time();
        }
        if !matches!(self.peek(), Some(Token::Number(_))) {
            return Err(DateError::CannotParse);
        }
        if self.number_starts_time() {
            self.time()?;
            let year = self.take_number()?;
            self.set_year(year);
            Ok(())
        } else {
            let year = self.take_number()?;
            self.set_year(year);
            self.time()
        }
    }

    /// The time of day and its optional morning or afternoon marker.
    fn time(&mut self) -> Result<(), DateError> {
        let leading = match self.peek() {
            Some(Token::Am) => Some(false),
            Some(Token::Pm) => Some(true),
            _ => None,
        };
        if leading.is_some() {
            self.take();
        }
        let hour = self.take_number()?;
        self.expect_char(b':')?;
        let minute = self.take_number()?;
        let mut second = 0;
        if self.eat_char(b':') {
            second = self.take_number()?;
            if self.eat_char(b'.') {
                let _fraction = self.take_number()?;
            }
        }
        self.settings.hour = Some(hour);
        self.settings.minute = Some(minute);
        self.settings.second = Some(second);
        self.settings.am_pm = leading;
        if leading.is_none() {
            match self.peek() {
                Some(Token::Am) => {
                    self.take();
                    self.settings.am_pm = Some(false);
                }
                Some(Token::Pm) => {
                    self.take();
                    self.settings.am_pm = Some(true);
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// The optional zone name, signed offset and parenthesised description.
    fn timezone(&mut self) -> Result<(), DateError> {
        if let Some(Token::TimeZone(value)) = self.peek() {
            self.take();
            self.settings.zone = Some(value);
        }
        match self.peek() {
            Some(Token::Char(unit)) if unit == u16::from(b'+') => {
                self.take();
                let value = self.take_number()?;
                self.settings.zone_offset = Some(value);
            }
            Some(Token::Char(unit)) if unit == u16::from(b'-') => {
                self.take();
                let value = self.take_number()?;
                self.settings.zone_offset = Some(value.wrapping_neg());
            }
            _ => {}
        }
        if self.eat_char(b'(') {
            loop {
                match self.take() {
                    Some(Token::Char(unit)) if unit == u16::from(b')') => break,
                    Some(_) => {}
                    None => return Err(DateError::CannotParse),
                }
            }
        }
        Ok(())
    }

    /// The next token.
    fn peek(&self) -> Option<Token> {
        self.tokens.get(self.at).copied()
    }

    /// The token after the next one.
    fn peek_next(&self) -> Option<Token> {
        self.tokens.get(self.at + 1).copied()
    }

    /// Takes the next token.
    fn take(&mut self) -> Option<Token> {
        let token = self.peek();
        if token.is_some() {
            self.at += 1;
        }
        token
    }

    /// Takes a number token.
    fn take_number(&mut self) -> Result<i32, DateError> {
        match self.take() {
            Some(Token::Number(value)) => Ok(value),
            _ => Err(DateError::CannotParse),
        }
    }

    /// Takes a month token.
    fn take_month(&mut self) -> Result<i32, DateError> {
        match self.take() {
            Some(Token::Month(value)) => Ok(value),
            _ => Err(DateError::CannotParse),
        }
    }

    /// Takes the character `wanted`, or answers whether it was absent.
    fn eat_char(&mut self, wanted: u8) -> bool {
        if self.peek() == Some(Token::Char(u16::from(wanted))) {
            self.take();
            true
        } else {
            false
        }
    }

    /// Takes the character `wanted`, or refuses.
    fn expect_char(&mut self, wanted: u8) -> Result<(), DateError> {
        if self.eat_char(wanted) {
            Ok(())
        } else {
            Err(DateError::CannotParse)
        }
    }

    /// Takes a `-` or a `/`, or refuses.
    fn expect_hyphen_or_slash(&mut self) -> Result<(), DateError> {
        if self.eat_char(b'-') || self.eat_char(b'/') {
            Ok(())
        } else {
            Err(DateError::CannotParse)
        }
    }

    /// Whether the next token is a number and the one after it is a colon.
    fn number_starts_time(&self) -> bool {
        matches!(self.peek(), Some(Token::Number(_)))
            && self.peek_next() == Some(Token::Char(u16::from(b':')))
    }

    /// Stores a year, applying the two-digit rule.
    fn set_year(&mut self, value: i32) {
        let year = if value < 100 {
            if value <= 50 {
                2_000i32.wrapping_add(value)
            } else {
                1_900i32.wrapping_add(value)
            }
        } else {
            value
        };
        self.settings.year = Some(year);
    }
}

/// The longest word at `start`, if its end is not followed by a letter.
///
/// The walk follows the word keys one unit at a time and does not go back to a
/// shorter key once it has taken a longer branch. A key matches when the unit
/// after it is not a letter, so a month abbreviation is not taken out of a
/// longer word.
fn word_at(units: &[u16], start: usize) -> Option<(usize, Word)> {
    let mut candidates: Vec<&(&'static str, Word)> = WORDS.iter().collect();
    let mut length = 0;
    loop {
        let unit = units.get(start + length).copied().unwrap_or(0);
        let exact = candidates
            .iter()
            .find(|(key, _)| key.len() == length)
            .map(|(_, word)| *word);
        let Some(lower) = ascii_lower(unit) else {
            return match exact {
                Some(word) if !is_letter(unit) => Some((length, word)),
                _ => None,
            };
        };
        let edge = candidates
            .iter()
            .any(|(key, _)| key.as_bytes().get(length) == Some(&lower));
        if !edge {
            return match exact {
                Some(word) if !is_letter(unit) => Some((length, word)),
                _ => None,
            };
        }
        length += 1;
        candidates.retain(|(key, _)| key.as_bytes().get(length - 1) == Some(&lower));
    }
}

/// The ASCII lower-case form of a code unit, or `None` above ASCII.
fn ascii_lower(unit: u16) -> Option<u8> {
    let byte = u8::try_from(unit).ok()?;
    if byte.is_ascii_uppercase() {
        Some(byte.to_ascii_lowercase())
    } else {
        Some(byte)
    }
}

/// The code unit with an ASCII upper-case letter folded down.
fn lower(unit: u16) -> u16 {
    if (u16::from(b'A')..=u16::from(b'Z')).contains(&unit) {
        unit + u16::from(b'a' - b'A')
    } else {
        unit
    }
}

/// Whether the code unit is a letter for a word's end boundary.
///
/// A code unit above ASCII counts as a letter, so a word is never matched
/// immediately before a non-ASCII unit.
fn is_letter(unit: u16) -> bool {
    unit >= 0x100 || u8::try_from(unit).is_ok_and(|byte| byte.is_ascii_alphabetic())
}

/// Whether the code unit is a digit of the decimal alphabet.
fn is_digit(unit: u16) -> bool {
    (u16::from(b'0')..=u16::from(b'9')).contains(&unit)
}

/// Whether the code unit is white space: the six ASCII space characters.
fn is_space(unit: u16) -> bool {
    unit == 0x20 || (0x09..=0x0d).contains(&unit)
}

#[cfg(test)]
mod tests {
    use super::parse;
    use crate::vm::runtime::natives::date::{DateError, Fields, local_fields};

    /// The fields of one text at a fixed `+09:00` zone.
    fn read(text: &str) -> Result<Fields, DateError> {
        let units: Vec<u16> = text.encode_utf16().collect();
        let seconds = parse(&units, 32_400)?;
        local_fields(seconds, 32_400)
    }

    /// The year, month and day of one text.
    fn day(text: &str) -> (i64, i64, i64) {
        let fields = read(text).expect("the text is a date");
        (fields.year, fields.month, fields.day)
    }

    /// The hour, minute and second of one text.
    fn time(text: &str) -> (i64, i64, i64) {
        let fields = read(text).expect("the text is a date");
        (fields.hour, fields.minute, fields.second)
    }

    // reference/krkr2/cpp/core/tjs2/bison/tjsdate.y:54-136
    #[test]
    fn every_date_form_reads_the_same_day() {
        let forms = [
            "Sun, 3 May 2004 11:22:33 GMT +900",
            "Sun, 3-May 2004 11:22:33 GMT +900",
            "Sun, 3-May-2004 11:22:33 GMT +900",
            "Sun, May 3 2004 11:22:33 GMT +900",
            "Sun, May-3 2004 11:22:33 GMT +900",
            "Sun, May-3-2004 11:22:33 GMT +900",
            "Sun, 3 May 11:22:33 2004 GMT +900",
            "Sun, 3-May 11:22:33 2004 GMT +900",
            "Sun, May 3 11:22:33 2004 GMT +900",
            "Sun, May-3 11:22:33 2004 GMT +900",
            "2004/03/03 11:22:33 GMT +900",
            "2004-03-03 11:22:33 GMT +900",
        ];
        for form in forms {
            assert_eq!(day(form), (2004, 4, 3), "{form}");
            assert_eq!(time(form), (11, 22, 33), "{form}");
        }
    }

    // reference/krkr2/cpp/core/tjs2/bison/tjsdate.y:142-146
    // reference/krkr2/cpp/core/tjs2/tjsDateParser.cpp:63-70
    #[test]
    fn the_weekday_is_optional_and_ignored() {
        assert_eq!(day("3 May 2004 11:22:33 GMT"), (2004, 4, 3));
        assert_eq!(day("Sun 3 May 2004 11:22:33 GMT"), (2004, 4, 3));
        assert_eq!(day("Mon, 3 May 2004 11:22:33 GMT"), (2004, 4, 3));
    }

    // reference/krkr2/cpp/core/tjs2/tjsDateParser.cpp:34-55
    #[test]
    fn a_word_is_named_without_case_and_with_an_optional_period() {
        assert_eq!(day("3 september 2004 11:22:33 GMT"), (2004, 8, 3));
        assert_eq!(day("3 SEP. 2004 11:22:33 GMT"), (2004, 8, 3));
        assert_eq!(day("3 ju 2004 11:22:33 GMT"), (2004, 5, 3));
        assert_eq!(day("3 jul 2004 11:22:33 GMT"), (2004, 6, 3));
        assert_eq!(day("Mon., 3 May 2004 11:22:33 GMT"), (2004, 4, 3));
    }

    // reference/krkr2/cpp/core/tjs2/tjsDateParser.cpp:49-56
    #[test]
    fn a_word_does_not_match_inside_a_longer_word() {
        assert_eq!(
            parse(
                "3 Janx 2004 11:22:33 GMT"
                    .encode_utf16()
                    .collect::<Vec<_>>()
                    .as_slice(),
                0
            ),
            Err(DateError::CannotParse)
        );
        let wide = "3 May\u{3002}2004 11:22:33 GMT";
        assert_eq!(
            parse(wide.encode_utf16().collect::<Vec<_>>().as_slice(), 0),
            Err(DateError::CannotParse)
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsDateParser.cpp:183-192
    #[test]
    fn a_two_digit_year_is_nearer_the_century() {
        assert_eq!(day("3 May 04 11:22:33 GMT"), (2004, 4, 3));
        assert_eq!(day("3 May 50 11:22:33 GMT"), (2050, 4, 3));
        assert_eq!(day("3 May 51 11:22:33 GMT"), (1951, 4, 3));
        assert_eq!(day("3 May 99 11:22:33 GMT"), (1999, 4, 3));
        assert_eq!(day("3 May 00 11:22:33 GMT"), (2000, 4, 3));
        assert_eq!(day("04/03/03 11:22:33 GMT"), (2004, 2, 3));
    }

    // reference/krkr2/cpp/core/tjs2/tjsDateParser.cpp:74-127
    #[test]
    fn a_zone_is_a_name_or_an_offset_and_the_default_is_the_system_zone() {
        // the same wall clock at +09:00 is 02:22:33 UTC
        assert_eq!(time("3 May 2004 11:22:33 JST"), (11, 22, 33));
        assert_eq!(time("3 May 2004 11:22:33 GMT"), (20, 22, 33));
        assert_eq!(time("3 May 2004 11:22:33 EST"), (1, 22, 33));
        assert_eq!(time("3 May 2004 11:22:33 GMT -500"), (1, 22, 33));
        let unnamed = read("3 May 2004 11:22:33").expect("the text is a date");
        assert_eq!((unnamed.day, unnamed.hour), (3, 11));
    }

    // reference/krkr2/cpp/core/tjs2/bison/tjsdate.y:159-191
    #[test]
    fn the_time_may_omit_seconds_and_may_be_am_or_pm() {
        assert_eq!(time("3 May 2004 11:22 GMT +900"), (11, 22, 0));
        assert_eq!(time("3 May 2004 11:22:33.75 GMT +900"), (11, 22, 33));
        assert_eq!(time("3 May 2004 PM 11:22:33 GMT +900"), (23, 22, 33));
        assert_eq!(time("3 May 2004 11:22:33 PM GMT +900"), (23, 22, 33));
        assert_eq!(time("3 May 2004 AM 11:22:33 GMT +900"), (11, 22, 33));
        assert_eq!(time("3 May 2004 12:00:00 PM GMT +900"), (0, 0, 0));
        assert_eq!(time("3 May 2004 12:00:00 AM GMT +900"), (12, 0, 0));
    }

    // reference/krkr2/cpp/core/tjs2/bison/tjsdate.y:54-156
    #[test]
    fn a_month_field_carries_and_a_day_field_carries() {
        assert_eq!(day("2004/13/03 11:22:33 GMT"), (2005, 0, 3));
        assert_eq!(day("2004/03/00 11:22:33 GMT"), (2004, 1, 29));
        assert_eq!(day("2004/03/32 11:22:33 GMT"), (2004, 3, 1));
    }

    // reference/krkr2/cpp/core/tjs2/tjsDateParser.cpp:63-70
    #[test]
    fn a_form_without_a_date_field_is_refused() {
        for text in [
            "   ",
            "11:22:33",
            "3 May 2004",
            "3 May 11:22:33 GMT",
            "May 2004 11:22:33 GMT",
            "2004/03 11:22:33 GMT",
            "3 May 2004 11:22.5 GMT",
            "3 May 2004 11:22:33. GMT",
            "3 May 2004 11:22:33 GMT +900 junk",
            "3 May 2004 11:22:33 XYZ",
            "3 May 2004 11:22:33 GMT (JST",
            "3 May 2004 AM 11:22:33 PM GMT",
        ] {
            let units: Vec<u16> = text.encode_utf16().collect();
            assert_eq!(parse(&units, 32_400), Err(DateError::CannotParse), "{text}");
        }
    }

    /// An empty text is refused before its first unit is read.
    ///
    /// reference/krkr2/cpp/core/tjs2/tjsDateParser.cpp:138 — the engine reads
    /// the first unit before it has established that the text has storage, and
    /// a zero-length text has none; the process stops instead of raising. A
    /// refusal is the answer this grammar gives, and no script can observe the
    /// other one without losing the process.
    #[test]
    fn an_empty_text_is_refused() {
        assert_eq!(parse(&[], 32_400), Err(DateError::CannotParse));
        assert_eq!(parse(&[0], 32_400), Err(DateError::CannotParse));
    }

    // reference/krkr2/cpp/core/tjs2/tjsDateParser.cpp:63-70
    #[test]
    fn exactly_minus_one_second_is_an_invalid_instant() {
        let units: Vec<u16> = "1 Jan 1970 08:59:59".encode_utf16().collect();
        assert_eq!(parse(&units, 32_400), Err(DateError::InvalidTimestamp));
        let before: Vec<u16> = "1 Jan 1970 08:59:58".encode_utf16().collect();
        assert_eq!(parse(&before, 32_400), Ok(-2_000));
        let after: Vec<u16> = "1 Jan 1970 09:00:00".encode_utf16().collect();
        assert_eq!(parse(&after, 32_400), Ok(0));
    }
}
